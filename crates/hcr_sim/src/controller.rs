//! Joint motion with the head-safety constraint applied.
//!
//! Ports `src/features/robot/RobotController.ts`.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use hcr_contract::{JointConfig, RobotConfig, Vec3, VoxelConfig};

use crate::collision::{RobotCollisionPart, find_robot_head_collision};
use crate::error::SimError;
use crate::kinematics::{RobotPose, compute_robot_pose};
use crate::state::JointAngles;

/// Largest angular increment tested against the head between collision checks.
///
/// This is a *constant*, independent of tick size — which is why the reported
/// safe angle does not drift with frame rate (`docs/backend/02-DETERMINISM.md` §2).
pub const MAX_ANGULAR_STEP_DEG: f64 = 0.5;

/// Bisection iterations used to refine the contact boundary.
///
/// Converges to within `MAX_ANGULAR_STEP_DEG / 2^12` ≈ 0.0001°.
pub const COLLISION_BISECTION_STEPS: u32 = 12;

/// A motion halted because continuing would enter the head.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockedHeadCollision {
    /// Which arm primitive would have touched.
    pub part: RobotCollisionPart,
    /// Joint being driven when contact was found.
    pub joint_id: String,
    /// Last angle that did not collide.
    pub safe_angle_deg: f64,
}

/// Outcome of advancing the active move by one tick.
#[derive(Debug, Clone, PartialEq)]
pub struct MoveAdvanceResult {
    /// Milliseconds actually consumed, which may be less than requested.
    pub consumed_ms: f64,
    /// Whether the move reached its target.
    pub completed: bool,
    /// Whether the end effector actually moved.
    pub moved: bool,
    /// End-effector position before this tick.
    pub previous_end_effector: Vec3,
    /// End-effector position after this tick.
    pub current_end_effector: Vec3,
    /// Set when the move was halted by the head constraint.
    pub blocked_collision: Option<BlockedHeadCollision>,
}

#[derive(Debug, Clone)]
struct ActiveMove {
    joint_id: String,
    start_angle_deg: f64,
    target_angle_deg: f64,
    duration_ms: f64,
    elapsed_ms: f64,
}

/// Drives joints toward commanded angles without letting the arm enter the head.
pub struct RobotController<'a> {
    robot_config: &'a RobotConfig,
    config_by_id: BTreeMap<&'a str, &'a JointConfig>,
    /// `Some` enables the head constraint; `None` disables it (used by unit tests).
    voxel_config: Option<&'a VoxelConfig>,
    joint_angles: JointAngles,
    active_move: Option<ActiveMove>,
}

impl<'a> RobotController<'a> {
    /// Create a controller at the challenge's initial pose.
    pub fn new(robot_config: &'a RobotConfig, voxel_config: Option<&'a VoxelConfig>) -> Self {
        Self {
            robot_config,
            config_by_id: robot_config
                .joints
                .iter()
                .map(|joint| (joint.id.as_str(), joint))
                .collect(),
            voxel_config,
            joint_angles: JointAngles::initial(robot_config),
            active_move: None,
        }
    }

    /// Return every joint to its initial angle and drop any active move.
    pub fn reset(&mut self) {
        self.joint_angles = JointAngles::initial(self.robot_config);
        self.active_move = None;
    }

    /// Current joint angles.
    pub fn angles(&self) -> &JointAngles {
        &self.joint_angles
    }

    /// Current pose.
    pub fn pose(&self) -> Result<RobotPose, SimError> {
        compute_robot_pose(self.robot_config, &self.joint_angles)
    }

    /// Whether a move is in progress.
    pub fn has_active_move(&self) -> bool {
        self.active_move.is_some()
    }

    /// Begin driving `joint_id` toward `target_angle_deg`.
    ///
    /// # Errors
    /// Rejects unknown joints and angles outside the configured travel, matching
    /// the TypeScript guard.
    pub fn begin_move(&mut self, joint_id: &str, target_angle_deg: f64) -> Result<(), SimError> {
        let config = self
            .config_by_id
            .get(joint_id)
            .ok_or_else(|| SimError::UnknownJoint {
                joint_id: joint_id.to_string(),
            })?;

        if !target_angle_deg.is_finite()
            || target_angle_deg < config.min_angle_deg
            || target_angle_deg > config.max_angle_deg
        {
            return Err(SimError::AngleOutOfRange {
                joint_id: joint_id.to_string(),
                angle_deg: target_angle_deg,
                min_angle_deg: config.min_angle_deg,
                max_angle_deg: config.max_angle_deg,
            });
        }

        let start_angle_deg =
            self.joint_angles
                .get(joint_id)
                .ok_or_else(|| SimError::MissingJoint {
                    joint_id: joint_id.to_string(),
                })?;

        self.active_move = Some(ActiveMove {
            joint_id: joint_id.to_string(),
            start_angle_deg,
            target_angle_deg,
            duration_ms: ((target_angle_deg - start_angle_deg).abs() / config.speed_deg_per_sec)
                * 1000.0,
            elapsed_ms: 0.0,
        });

        Ok(())
    }

    /// Advance the active move by up to `delta_ms`.
    pub fn advance_move(&mut self, delta_ms: f64) -> Result<MoveAdvanceResult, SimError> {
        if !delta_ms.is_finite() || delta_ms < 0.0 {
            return Err(SimError::Internal(
                "Delta must be a finite non-negative number.",
            ));
        }

        let active = self
            .active_move
            .clone()
            .ok_or(SimError::Internal("No active robot move."))?;

        let previous_end_effector = self.pose()?.end_effector;

        let remaining_ms = (active.duration_ms - active.elapsed_ms).max(0.0);
        let consumed_ms = delta_ms.min(remaining_ms);
        let target_elapsed_ms = active.elapsed_ms + consumed_ms;
        let target_progress = if active.duration_ms == 0.0 {
            1.0
        } else {
            target_elapsed_ms / active.duration_ms
        };
        let target_angle = active.start_angle_deg
            + (active.target_angle_deg - active.start_angle_deg) * target_progress;

        let current_angle =
            self.joint_angles
                .get(&active.joint_id)
                .ok_or_else(|| SimError::MissingJoint {
                    joint_id: active.joint_id.clone(),
                })?;

        let blocked = self.advance_angle_with_constraint(&active.joint_id, current_angle, target_angle)?;

        if let Some(blocked_collision) = blocked {
            let safe_progress = if active.duration_ms == 0.0 {
                1.0
            } else {
                (blocked_collision.safe_angle_deg - active.start_angle_deg)
                    / (active.target_angle_deg - active.start_angle_deg)
            };
            let safe_elapsed_ms = active.duration_ms * safe_progress.clamp(0.0, 1.0);
            let safe_consumed_ms = (safe_elapsed_ms - active.elapsed_ms).max(0.0);

            if let Some(m) = self.active_move.as_mut() {
                m.elapsed_ms = safe_elapsed_ms;
            }
            let current_end_effector = self.pose()?.end_effector;

            return Ok(MoveAdvanceResult {
                consumed_ms: safe_consumed_ms,
                completed: false,
                moved: !points_equal(previous_end_effector, current_end_effector),
                previous_end_effector,
                current_end_effector,
                blocked_collision: Some(blocked_collision),
            });
        }

        if let Some(m) = self.active_move.as_mut() {
            m.elapsed_ms = target_elapsed_ms;
        }
        let current_end_effector = self.pose()?.end_effector;
        let completed = target_progress >= 1.0;
        if completed {
            self.joint_angles
                .set(&active.joint_id, active.target_angle_deg);
            self.active_move = None;
        }

        Ok(MoveAdvanceResult {
            consumed_ms,
            completed,
            moved: !points_equal(previous_end_effector, current_end_effector),
            previous_end_effector,
            current_end_effector,
            blocked_collision: None,
        })
    }

    /// Step a joint toward `target_angle_deg`, stopping at the last safe angle.
    ///
    /// Sub-steps are capped at [`MAX_ANGULAR_STEP_DEG`] regardless of how much
    /// angle this tick covers, then the contact boundary is refined by bisection.
    fn advance_angle_with_constraint(
        &mut self,
        joint_id: &str,
        start_angle_deg: f64,
        target_angle_deg: f64,
    ) -> Result<Option<BlockedHeadCollision>, SimError> {
        let Some(voxel_config) = self.voxel_config else {
            self.joint_angles.set(joint_id, target_angle_deg);
            return Ok(None);
        };

        let delta_angle = target_angle_deg - start_angle_deg;
        // `f64::ceil` is std-only, so go through libm — which also keeps the
        // server and the firmware on one implementation.
        let step_count = libm::ceil(delta_angle.abs() / MAX_ANGULAR_STEP_DEG).max(1.0);
        let mut last_safe_angle = start_angle_deg;

        let mut index = 1.0_f64;
        while index <= step_count {
            let candidate_angle = start_angle_deg + delta_angle * (index / step_count);
            self.joint_angles.set(joint_id, candidate_angle);
            let pose = compute_robot_pose(self.robot_config, &self.joint_angles)?;

            let Some(collision) = find_robot_head_collision(
                &pose,
                voxel_config,
                &self.robot_config.geometry,
            ) else {
                last_safe_angle = candidate_angle;
                index += 1.0;
                continue;
            };

            let mut safe_angle = last_safe_angle;
            let mut colliding_angle = candidate_angle;
            let mut boundary_part = collision.part;

            for _ in 0..COLLISION_BISECTION_STEPS {
                let midpoint = (safe_angle + colliding_angle) / 2.0;
                self.joint_angles.set(joint_id, midpoint);
                let midpoint_pose = compute_robot_pose(self.robot_config, &self.joint_angles)?;
                match find_robot_head_collision(
                    &midpoint_pose,
                    voxel_config,
                    &self.robot_config.geometry,
                ) {
                    Some(midpoint_collision) => {
                        colliding_angle = midpoint;
                        boundary_part = midpoint_collision.part;
                    }
                    None => safe_angle = midpoint,
                }
            }

            self.joint_angles.set(joint_id, safe_angle);
            return Ok(Some(BlockedHeadCollision {
                part: boundary_part,
                joint_id: joint_id.to_string(),
                safe_angle_deg: safe_angle,
            }));
        }

        Ok(None)
    }
}

/// Component-wise epsilon comparison, matching the TypeScript `pointsEqual`.
fn points_equal(a: Vec3, b: Vec3) -> bool {
    (a[0] - b[0]).abs() < f64::EPSILON
        && (a[1] - b[1]).abs() < f64::EPSILON
        && (a[2] - b[2]).abs() < f64::EPSILON
}
