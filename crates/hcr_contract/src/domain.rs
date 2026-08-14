//! Frozen v1 domain types.
//!
//! These mirror `src/types/domain.ts` and `src/features/blockly/programTypes.ts`
//! in the frontend and are frozen by SPEC v0.3 §15: new capabilities may only be
//! added, never applied by changing these shapes. If this file and `src/` ever
//! disagree, `src/` is correct and this file is a bug.

use alloc::{string::String, vec::Vec};
use serde::{Deserialize, Serialize};

/// Identifier of a simulated joint, e.g. `"baseYaw"`.
pub type JointId = String;

/// A world-space point. Mirrors the frontend's `Vec3Tuple`.
pub type Vec3 = [f64; 3];

/// Rotation axis a joint turns about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    /// Rotation about world X.
    X,
    /// Rotation about world Y.
    Y,
    /// Rotation about world Z.
    Z,
}

/// Integer voxel lattice coordinate.
///
/// `Ord` is derived so voxel sets can be held in a `BTreeSet`, giving the
/// replay engine deterministic iteration order (see `docs/backend/02-DETERMINISM.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoxelCoord {
    /// Lattice X.
    pub x: i32,
    /// Lattice Y.
    pub y: i32,
    /// Lattice Z.
    pub z: i32,
}

/// One servo-driven joint of the simulated arm.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JointConfig {
    /// Stable identifier used by Program IR.
    pub id: JointId,
    /// Human-readable name.
    pub name: String,
    /// Axis this joint rotates about.
    pub axis: Axis,
    /// Lower travel limit, **servo degrees** when `servo` is present.
    pub min_angle_deg: f64,
    /// Upper travel limit, **servo degrees** when `servo` is present.
    pub max_angle_deg: f64,
    /// Angle at reset, **servo degrees** when `servo` is present.
    pub initial_angle_deg: f64,
    /// Slew rate used for duration estimation, degrees per second.
    pub speed_deg_per_sec: f64,
    /// How this joint's angles map onto a servo on the physical arm.
    ///
    /// Absent for simulation-only joints the hardware has no axis for
    /// (`shoulderRoll`), whose angles stay geometric because there is no servo
    /// whose degrees they could be.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub servo: Option<ServoMapping>,
}

/// A servo on the arm, named as `hcr-fw` names them (`robot/axis_config.rs`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ServoAxisId {
    /// Base rotation. `baseYaw` on the simulated arm.
    X,
    /// Shoulder. `shoulder`.
    Y,
    /// Elbow. `elbow`.
    Z,
    /// Wrist balance. `wrist`.
    B,
    /// Cutter open/close. Present on the hardware, deliberately not simulated —
    /// SPEC v0.3 keeps scissor actuation out of the first version.
    E,
}

/// Affine map between a joint's servo degrees and the geometric angle the
/// kinematics rotates by.
///
/// ```text
/// servoDeg     = centerDeg + direction × (geometricDeg − offsetDeg)
/// geometricDeg = offsetDeg + direction × (servoDeg − centerDeg)
/// ```
///
/// Mirrors `features/robot/servoMapping.ts` on the frontend; the two must agree
/// or replayed scores would not match what the learner saw.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServoMapping {
    /// Which physical servo this joint drives.
    pub axis: ServoAxisId,
    /// Servo angle the joint's `offset_deg` lands on. 90° on every axis.
    pub center_deg: f64,
    /// `+1` when the servo turns the same way as the model, `-1` when it opposes.
    pub direction: i8,
    /// Geometric angle that `center_deg` corresponds to.
    pub offset_deg: f64,
}

impl ServoMapping {
    /// Servo degrees -> the geometric angle the kinematics rotates by.
    #[must_use]
    pub fn to_geometric_deg(&self, servo_deg: f64) -> f64 {
        self.offset_deg + f64::from(self.direction) * (servo_deg - self.center_deg)
    }

    /// Geometric angle -> the degrees the servo is commanded to.
    #[must_use]
    pub fn to_servo_deg(&self, geometric_deg: f64) -> f64 {
        self.center_deg + f64::from(self.direction) * (geometric_deg - self.offset_deg)
    }
}

/// Capsule radii used by the head-collision constraint.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobotCollisionConfig {
    /// Radius of the arm links.
    pub link_radius: f64,
    /// Radius of the joint spheres.
    pub joint_radius: f64,
    /// Radius of the tool shaft.
    pub tool_shaft_radius: f64,
    /// Extra margin added to every primitive before the head test.
    pub head_clearance: f64,
}

/// Static geometry of the arm.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobotGeometryConfig {
    /// World position of the base.
    pub base_position: Vec3,
    /// Height from base to shoulder.
    pub shoulder_height: f64,
    /// Shoulder-to-elbow length.
    pub upper_arm_length: f64,
    /// Elbow-to-wrist length.
    pub forearm_length: f64,
    /// Wrist-to-tip length.
    pub tool_length: f64,
    /// Radius of the cutting sphere swept against hair voxels.
    pub tool_radius: f64,
    /// Collision radii.
    pub collision: RobotCollisionConfig,
}

/// Joints plus geometry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobotConfig {
    /// Ordered joint definitions.
    pub joints: Vec<JointConfig>,
    /// Arm geometry.
    pub geometry: RobotGeometryConfig,
}

/// Voxel lattice placement and the head ellipsoid.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoxelConfig {
    /// World position of lattice coordinate (0,0,0).
    pub origin: Vec3,
    /// Edge length of one voxel.
    pub size: f64,
    /// Centre of the impenetrable head ellipsoid.
    pub head_center: Vec3,
    /// Semi-axes of the head ellipsoid.
    pub head_scale: Vec3,
}

/// A named set of hair voxels.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HairstyleDefinition {
    /// Stable identifier.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Occupied lattice cells.
    pub voxels: Vec<VoxelCoord>,
}

/// Relative weights of the three score components. Must sum to 1.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreWeights {
    /// Weight of voxel IoU against the target.
    pub completion: f64,
    /// Weight of program economy.
    pub efficiency: f64,
    /// Weight of estimated runtime.
    pub time: f64,
}

/// Scoring parameters carried by a challenge.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoringConfig {
    /// Component weights.
    pub weights: ScoreWeights,
    /// Program cost that scores 100 on efficiency.
    pub reference_program_cost: f64,
    /// Duration that scores 100 on time.
    pub reference_time_ms: f64,
    /// Cost multiplier applied to each executed command.
    pub command_weight: f64,
}

/// Program size and timing measurements.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramMetrics {
    /// Enabled, non-shadow blocks in the source workspace.
    pub source_block_count: u32,
    /// Atomic commands actually completed. A halted program reports fewer.
    pub executed_command_count: u32,
    /// Estimated duration of the expanded command list.
    pub estimated_duration_ms: f64,
}

/// The four numbers shown to the learner.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreResult {
    /// Voxel IoU against the target, 0..=100.
    pub completion_score: f64,
    /// Program economy, 0..=100.
    pub efficiency_score: f64,
    /// Runtime economy, 0..=100.
    pub time_score: f64,
    /// Weighted total, 0..=100.
    pub final_score: f64,
    /// Raw cost that produced `efficiency_score`.
    pub program_cost: f64,
}

/// Block kinds a challenge may permit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AllowedBlockType {
    /// Drive one joint to an absolute angle.
    SetJointAngle,
    /// Idle for a fixed duration.
    Wait,
    /// Repeat a body a fixed number of times.
    Repeat,
}

/// A full challenge as it appears on the wire.
///
/// This is the *definition* form (`voxels: Vec<VoxelCoord>`), not the frontend's
/// normalized `Challenge` (which holds a `Set<VoxelKey>` and has no JSON form).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeDefinition {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Display description.
    pub description: String,
    /// Arm definition.
    pub robot_config: RobotConfig,
    /// Lattice and head placement.
    pub voxel_config: VoxelConfig,
    /// Hair present at reset.
    pub initial_hair: HairstyleDefinition,
    /// Hair the learner is aiming for.
    pub target_hair: HairstyleDefinition,
    /// Blocks permitted in the editor.
    pub allowed_blocks: Vec<AllowedBlockType>,
    /// Opaque Blockly workspace blob; passed through, never interpreted here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starter_workspace: Option<serde_json::Value>,
    /// Scoring parameters.
    pub scoring: ScoringConfig,
}

/// One atomic, executable command. This is the unit three executors share.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RobotCommand {
    /// Drive a joint to an absolute angle at its configured speed.
    #[serde(rename_all = "camelCase")]
    SetJointAngle {
        /// Joint to drive.
        joint_id: JointId,
        /// Absolute target angle, degrees.
        angle_deg: f64,
        /// Originating Blockly block, for error attribution.
        source_block_id: String,
    },
    /// Hold position for a duration.
    #[serde(rename_all = "camelCase")]
    Wait {
        /// Duration in milliseconds.
        duration_ms: f64,
        /// Originating Blockly block.
        source_block_id: String,
    },
}

impl RobotCommand {
    /// Blockly block this command came from.
    pub fn source_block_id(&self) -> &str {
        match self {
            RobotCommand::SetJointAngle {
                source_block_id, ..
            }
            | RobotCommand::Wait {
                source_block_id, ..
            } => source_block_id,
        }
    }
}

/// Program IR node. `Repeat` is expanded away before execution.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ProgramNode {
    /// See [`RobotCommand::SetJointAngle`].
    #[serde(rename_all = "camelCase")]
    SetJointAngle {
        /// Joint to drive.
        joint_id: JointId,
        /// Absolute target angle, degrees.
        angle_deg: f64,
        /// Originating Blockly block.
        source_block_id: String,
    },
    /// See [`RobotCommand::Wait`].
    #[serde(rename_all = "camelCase")]
    Wait {
        /// Duration in milliseconds.
        duration_ms: f64,
        /// Originating Blockly block.
        source_block_id: String,
    },
    /// Repeat `body` `count` times.
    #[serde(rename_all = "camelCase")]
    Repeat {
        /// Iteration count, 1..=20 as enforced by the compiler.
        count: u32,
        /// Nested nodes.
        body: Vec<ProgramNode>,
        /// Originating Blockly block.
        source_block_id: String,
    },
}

/// A compiled program as submitted by a client.
///
/// The server always re-expands this itself and never trusts a client-supplied
/// command list (`docs/backend/README.md` decision D3).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Program {
    /// Top-level nodes.
    pub nodes: Vec<ProgramNode>,
    /// Enabled, non-shadow block count in the source workspace.
    pub source_block_count: u32,
}

/// Why a run stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalReason {
    /// Every command ran to completion.
    Completed,
    /// A link would have entered the head; the arm stopped at the last safe pose.
    HeadCollision,
    /// Expansion exceeded the atomic-command cap.
    CommandLimit,
    /// The program failed validation.
    Invalid,
    /// The replay exceeded its tick or wall-clock budget.
    Timeout,
}

/// Terminal state of a run, with attribution when it failed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminal {
    /// Why the run stopped.
    pub reason: TerminalReason,
    /// Joint being driven when a collision was detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub joint_id: Option<JointId>,
    /// Last angle that did not collide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_angle_deg: Option<f64>,
    /// Blockly block to highlight.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_block_id: Option<String>,
    /// Which arm part would have touched the head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub part_label: Option<String>,
}

impl Terminal {
    /// A clean completion with no attribution.
    pub fn completed() -> Self {
        Self {
            reason: TerminalReason::Completed,
            joint_id: None,
            safe_angle_deg: None,
            source_block_id: None,
            part_label: None,
        }
    }
}
