//! Cutter Grid: verifying a client-planned trajectory, then replaying it.
//!
//! # Why this is not [`crate::engine::replay`]
//!
//! A servo program says what to do and the server works out what happens. A
//! Cutter Grid program says where to go, and *how the arm gets there* is decided
//! by an inverse-kinematics search the browser runs at compile time. The hair
//! that comes off is swept along the joint path that search produced, not along
//! the ideal lattice line, so scoring the ideal line would score a motion nobody
//! performed.
//!
//! The trajectory therefore has to travel with the submission. That makes this
//! module's job different from the servo engine's: it is not simulating a
//! program, it is **auditing a claim**.
//!
//! # What is checked, and what that buys
//!
//! Everything below is re-derived from `jointAngles` alone. The client's own
//! `endEffector`, `expectedCutVoxels`, `expectedResultVoxels`,
//! `executedCommandCount` and `trajectorySignature` are compared against the
//! server's answer and never substituted for it.
//!
//! | Check | The lie it stops |
//! | --- | --- |
//! | Challenge signature | Planning against a kinder arm or an easier hairstyle |
//! | Steps match the re-expanded IR | Submitting a short program and a long trajectory |
//! | Coordinate chain | Teleporting between cells |
//! | Joint limits | Poses the physical arm cannot reach |
//! | Head collision | Cutting through the customer |
//! | Forward kinematics vs declared tip | Declaring a safe path while claiming a cutting pose |
//! | Per-step axis displacement | Calling a sweep across the whole head "right 1" |
//! | Path deviation | Bulging a cell-to-cell move out through the hair |
//! | Entry cuts nothing | A free haircut before the clock starts |
//!
//! A trajectory that survives all of these is a trajectory the arm could really
//! have flown, whatever the client's intent — which is what makes the resulting
//! score authoritative without the server having to run the IK search itself.
//!
//! # Rejection, not partial credit
//!
//! The servo engine stops at the last safe pose and still scores, because a
//! collision there is something a learner's program *did*. Cutter Grid rejects
//! the whole submission instead, because the frontend already refuses to run
//! such a program at all (SPEC v0.3 §15.3). A server that scored one would be
//! rewarding a plan the client would never have produced.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use hcr_contract::{
    CUTTER_GRID_LADDER_PLANNER_VERSION, CUTTER_TRAJECTORY_PLAN_VERSION, ChallengeDefinition,
    CutterGridAction, CutterGridCoord, CutterGridDirection, CutterGridNode, CutterGridProgram,
    CutterGridSubmission, CutterTrajectoryPlan, CutterTrajectoryStep, CutterTrajectoryStepKind,
    CutterTrajectoryWaypoint, MAX_RUNTIME_COMMANDS, ProgramMetrics, Terminal, Vec3,
    cutter_grid_challenge_signature_v2,
};

use crate::collision::find_robot_head_collision;
use crate::engine::ReplayOutcome;
use crate::error::{CutterRejection, SimError};
use crate::kinematics::compute_robot_pose;
use crate::scoring::calculate_score;
use crate::state::JointAngles;
use crate::voxel::{VoxelSet, find_swept_voxel_hits, result_voxels_hash};

/// Tunables for one verification pass.
#[derive(Debug, Clone, Copy)]
pub struct CutterReplayOptions {
    /// Atomic-action cap applied during expansion. The same 500 the servo path
    /// uses — one cell crossed is one command.
    pub command_limit: usize,
    /// How far the declared tip may sit from where forward kinematics puts it.
    ///
    /// The client computed its value with the same formulae from the same
    /// angles, so honest agreement is at the last few bits. This is loose enough
    /// to absorb JavaScript/Rust transcendental differences and nowhere near
    /// loose enough to hide a fabricated position.
    pub end_effector_tolerance: f64,
    /// How far a waypoint may stray from the straight line its step is meant to
    /// travel, as a divisor of the voxel edge.
    ///
    /// The planner certifies `/16`. Checking at exactly `/16` would bounce plans
    /// that only just satisfy the client's own bound, so the server checks at
    /// `/8`: still far tighter than the tool radius, which is what decides
    /// whether the wander could have cut anything unintended.
    pub path_deviation_divisor: f64,
    /// Slack on joint travel limits, degrees. Angles arrive quantized to `0.1°`,
    /// so this only absorbs representation noise.
    pub joint_limit_epsilon: f64,
    /// Slack when matching a step's closing pose to the next step's opening one.
    pub pose_continuity_epsilon: f64,
    /// Ceiling on total waypoints, a guard against a submission that is
    /// syntactically fine and computationally ruinous.
    pub max_waypoints: usize,
}

impl Default for CutterReplayOptions {
    fn default() -> Self {
        Self {
            command_limit: MAX_RUNTIME_COMMANDS,
            end_effector_tolerance: 1e-6,
            path_deviation_divisor: 8.0,
            joint_limit_epsilon: 1e-9,
            pose_continuity_epsilon: 1e-9,
            // 500 cells at the cap, each resampled far more finely than the
            // planner's `0.5°` rule needs, plus the entry. Reached only by a
            // submission built to be expensive.
            max_waypoints: 120_000,
        }
    }
}

/// What the client claimed, next to what the server found.
///
/// Purely observational. Divergence here is a conformance signal for operators —
/// the two engines drifting apart — and never something a learner is told about
/// or penalised for, exactly as `ClientPreview` works on the servo path.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterDivergence {
    /// Client's `executedCommandCount` against the server's expansion.
    pub command_count_matches: bool,
    /// Client's `expectedResultVoxels` against the hair the server left standing.
    ///
    /// `None` when the client sent no expectation.
    pub result_voxels_match: Option<bool>,
    /// Voxels the server removed that the client did not expect to lose.
    pub server_only_cuts: usize,
    /// Voxels the client expected to lose that the server left standing.
    pub client_only_cuts: usize,
}

impl CutterDivergence {
    /// Whether anything disagreed.
    pub fn diverged(&self) -> bool {
        !self.command_count_matches || self.result_voxels_match == Some(false)
    }
}

/// A verified Cutter Grid replay.
#[derive(Debug, Clone)]
pub struct CutterReplayOutcome {
    /// Score, metrics, remaining hair and hash — the same shape the servo path
    /// produces, so everything downstream is indifferent to which mode ran.
    pub replay: ReplayOutcome,
    /// How the client's own numbers compared.
    pub divergence: CutterDivergence,
}

/// Verify `submission` against `challenge` and score what it actually does.
///
/// # Errors
/// [`SimError::CutterPlanRejected`] when any audit in the module documentation
/// fails, carrying the offending Blockly block where one can be attributed so the
/// editor can highlight it. Ordinary validation failures — unknown joints, an
/// oversized expansion, malformed scoring — surface as their existing
/// [`SimError`] variants.
pub fn verify_and_replay(
    challenge: &ChallengeDefinition,
    submission: &CutterGridSubmission,
    options: CutterReplayOptions,
) -> Result<CutterReplayOutcome, SimError> {
    let program = &submission.program;
    let plan = &submission.plan;

    check_envelope(program, plan)?;
    check_signature(challenge, plan)?;

    let actions = expand_cutter_program(program, options.command_limit)?;
    check_steps_match_actions(&actions, plan)?;
    check_coordinate_chain(plan)?;
    check_waypoint_budget(plan, options.max_waypoints)?;

    // Entry first: it establishes the pose step 0 must open from, and it is the
    // one part of the motion that is allowed to happen for free — so it is also
    // the one part worth checking cuts nothing.
    let initial: VoxelSet = challenge.initial_hair.voxels.iter().copied().collect();
    let mut hair = initial.clone();
    let entry_end = verify_entry(challenge, plan, &initial, options)?;

    let mut previous_pose = entry_end;
    let mut simulated_ms = 0.0_f64;

    for step in &plan.steps {
        let tips = verify_step(challenge, step, options)?;

        if let Some(opening) = tips.first_angles.as_ref()
            && let Some(closing) = previous_pose.as_ref()
        {
            check_pose_continuity(step, closing, opening, options.pose_continuity_epsilon)?;
        }

        for pair in tips.positions.windows(2) {
            for hit in find_swept_voxel_hits(
                pair[0],
                pair[1],
                &hair,
                &challenge.voxel_config,
                challenge.robot_config.geometry.tool_radius,
            ) {
                hair.remove(&hit);
            }
        }

        simulated_ms += step.duration_ms;
        previous_pose = tips.last_angles;
    }

    let target: VoxelSet = challenge.target_hair.voxels.iter().copied().collect();
    let metrics = ProgramMetrics {
        source_block_count: program.source_block_count,
        // Server's own expansion, never the client's count.
        executed_command_count: u32::try_from(actions.len())
            .map_err(|_| SimError::Internal("action count exceeds u32"))?,
        estimated_duration_ms: simulated_ms,
    };
    let score = calculate_score(&initial, &target, &hair, &metrics, &challenge.scoring)?;
    let divergence = compare_with_client(plan, &initial, &hair, &metrics);
    let result_voxels_hash = result_voxels_hash(&hair);

    Ok(CutterReplayOutcome {
        replay: ReplayOutcome {
            score,
            metrics,
            remaining_voxels: hair,
            result_voxels_hash,
            terminal: Terminal::completed(),
            simulated_ms,
        },
        divergence,
    })
}

/// Flatten a Cutter Grid program into the actions that actually run.
///
/// `Move { distance: 3 }` becomes three cell crossings — which is why a long
/// move costs three commands, not one, and why the cap is meaningful.
///
/// # Errors
/// [`SimError::CommandLimitExceeded`] past `limit`, [`SimError::EmptyProgram`]
/// for a program with nothing in it, and [`SimError::InvalidWait`] for a
/// negative or non-finite duration.
pub fn expand_cutter_program(
    program: &CutterGridProgram,
    limit: usize,
) -> Result<Vec<CutterGridAction>, SimError> {
    let mut actions = Vec::new();
    expand_nodes(&program.nodes, limit, &mut actions)?;
    if actions.is_empty() {
        return Err(SimError::EmptyProgram);
    }
    Ok(actions)
}

fn expand_nodes(
    nodes: &[CutterGridNode],
    limit: usize,
    out: &mut Vec<CutterGridAction>,
) -> Result<(), SimError> {
    for node in nodes {
        match node {
            CutterGridNode::Move {
                direction,
                distance,
                source_block_id,
            } => {
                for _ in 0..*distance {
                    push_action(
                        out,
                        limit,
                        CutterGridAction::MoveCell {
                            direction: *direction,
                            source_block_id: source_block_id.clone(),
                        },
                    )?;
                }
            }
            CutterGridNode::Wait {
                duration_ms,
                source_block_id,
            } => {
                if !duration_ms.is_finite() || *duration_ms < 0.0 {
                    return Err(SimError::InvalidWait {
                        duration_ms: *duration_ms,
                    });
                }
                push_action(
                    out,
                    limit,
                    CutterGridAction::Wait {
                        duration_ms: *duration_ms,
                        source_block_id: source_block_id.clone(),
                    },
                )?;
            }
            CutterGridNode::Repeat { count, body, .. } => {
                for _ in 0..*count {
                    expand_nodes(body, limit, out)?;
                }
            }
        }
    }
    Ok(())
}

fn push_action(
    out: &mut Vec<CutterGridAction>,
    limit: usize,
    action: CutterGridAction,
) -> Result<(), SimError> {
    if out.len() >= limit {
        return Err(SimError::CommandLimitExceeded {
            limit,
            source_block_id: action.source_block_id().to_string(),
        });
    }
    out.push(action);
    Ok(())
}

fn check_envelope(program: &CutterGridProgram, plan: &CutterTrajectoryPlan) -> Result<(), SimError> {
    if program.kind != "cutter-grid" {
        return reject(
            CutterRejection::UnsupportedPlanVersion,
            None,
            format!("program kind \"{}\" is not \"cutter-grid\"", program.kind),
        );
    }
    if plan.kind != "cutter-grid-trajectory" {
        return reject(
            CutterRejection::UnsupportedPlanVersion,
            None,
            format!("plan kind \"{}\" is not a cutter trajectory", plan.kind),
        );
    }
    if plan.version != CUTTER_TRAJECTORY_PLAN_VERSION {
        return reject(
            CutterRejection::UnsupportedPlanVersion,
            None,
            format!(
                "plan version {} is not {CUTTER_TRAJECTORY_PLAN_VERSION}",
                plan.version
            ),
        );
    }
    // V1 and V2 disagree about which programs are reachable — V1's greedy search
    // rejects paths V2 finds — so a V1 asset replayed as V2 would be scored under
    // rules it was never planned against.
    if plan.planner_version != CUTTER_GRID_LADDER_PLANNER_VERSION
        || program.planner_version != CUTTER_GRID_LADDER_PLANNER_VERSION
    {
        return reject(
            CutterRejection::UnsupportedPlanVersion,
            None,
            format!(
                "planner {} / {} is not {CUTTER_GRID_LADDER_PLANNER_VERSION}",
                program.planner_version, plan.planner_version
            ),
        );
    }
    Ok(())
}

fn check_signature(
    challenge: &ChallengeDefinition,
    plan: &CutterTrajectoryPlan,
) -> Result<(), SimError> {
    let expected = cutter_grid_challenge_signature_v2(challenge);
    if plan.challenge_signature != expected {
        return reject(
            CutterRejection::SignatureMismatch,
            None,
            format!(
                "plan was built for challenge {} but this is {expected}",
                plan.challenge_signature
            ),
        );
    }
    Ok(())
}

fn check_steps_match_actions(
    actions: &[CutterGridAction],
    plan: &CutterTrajectoryPlan,
) -> Result<(), SimError> {
    if plan.steps.len() != actions.len() {
        return reject(
            CutterRejection::StepMismatch,
            None,
            format!(
                "plan has {} steps but the program expands to {}",
                plan.steps.len(),
                actions.len()
            ),
        );
    }

    for (index, (action, step)) in actions.iter().zip(&plan.steps).enumerate() {
        let index_u32 =
            u32::try_from(index).map_err(|_| SimError::Internal("step index exceeds u32"))?;
        if step.index != index_u32 {
            return reject(
                CutterRejection::StepMismatch,
                Some(step.source_block_id.clone()),
                format!("step at position {index} declares index {}", step.index),
            );
        }
        if step.source_block_id != action.source_block_id() {
            return reject(
                CutterRejection::StepMismatch,
                Some(step.source_block_id.clone()),
                format!(
                    "step {index} attributes block {} but the program says {}",
                    step.source_block_id,
                    action.source_block_id()
                ),
            );
        }
        let expected_kind = match action {
            CutterGridAction::MoveCell { .. } => CutterTrajectoryStepKind::MoveCell,
            CutterGridAction::Wait { .. } => CutterTrajectoryStepKind::Wait,
        };
        if step.kind != expected_kind {
            return reject(
                CutterRejection::StepMismatch,
                Some(step.source_block_id.clone()),
                format!("step {index} is a {:?}, expected {expected_kind:?}", step.kind),
            );
        }
    }
    Ok(())
}

/// Cells must be entered one at a time, in the order the program says.
fn check_coordinate_chain(plan: &CutterTrajectoryPlan) -> Result<(), SimError> {
    let mut coord = plan.start_coord;
    if coord != [0, 0, 0] {
        return reject(
            CutterRejection::CoordDiscontinuity,
            None,
            format!("programs start at the lattice origin, not {coord:?}"),
        );
    }

    for step in &plan.steps {
        if step.start_coord != coord {
            return reject(
                CutterRejection::CoordDiscontinuity,
                Some(step.source_block_id.clone()),
                format!(
                    "step {} starts at {:?} but the previous step ended at {coord:?}",
                    step.index, step.start_coord
                ),
            );
        }
        let expected_end = match step.kind {
            CutterTrajectoryStepKind::Wait => coord,
            CutterTrajectoryStepKind::MoveCell => {
                let direction = direction_between(step.start_coord, step.end_coord).ok_or_else(
                    || rejection(
                        CutterRejection::CoordDiscontinuity,
                        Some(step.source_block_id.clone()),
                        format!(
                            "step {} moves from {:?} to {:?}, which is not one cell along an axis",
                            step.index, step.start_coord, step.end_coord
                        ),
                    ),
                )?;
                apply_delta(coord, direction.delta())
            }
        };
        if step.end_coord != expected_end {
            return reject(
                CutterRejection::CoordDiscontinuity,
                Some(step.source_block_id.clone()),
                format!(
                    "step {} ends at {:?}, expected {expected_end:?}",
                    step.index, step.end_coord
                ),
            );
        }
        coord = step.end_coord;
    }

    if coord != plan.end_coord {
        return reject(
            CutterRejection::CoordDiscontinuity,
            None,
            format!("plan declares it ends at {:?} but reaches {coord:?}", plan.end_coord),
        );
    }
    Ok(())
}

fn check_waypoint_budget(plan: &CutterTrajectoryPlan, max: usize) -> Result<(), SimError> {
    let total = plan.positioning_trajectory.len()
        + plan
            .steps
            .iter()
            .map(|step| step.waypoints.len())
            .sum::<usize>();
    if total > max {
        return reject(
            CutterRejection::TooManyWaypoints,
            None,
            format!("plan carries {total} waypoints, over the {max} ceiling"),
        );
    }
    Ok(())
}

/// Poses a verified trajectory segment passes through.
struct SegmentTips {
    /// Tool-tip positions from the server's own forward kinematics.
    positions: Vec<Vec3>,
    /// Opening angles, for continuity against the previous segment.
    first_angles: Option<JointAngles>,
    /// Closing angles, for continuity against the next.
    last_angles: Option<JointAngles>,
}

/// Check the entry motion and confirm it costs the learner nothing.
///
/// The entry is planned, not programmed: it does not appear in the IR, is not
/// charged as commands or time, and must remove no hair. That last one is the
/// point — an entry allowed to cut would be a free haircut before the run
/// starts, attributed to no block and paid for by nobody.
fn verify_entry(
    challenge: &ChallengeDefinition,
    plan: &CutterTrajectoryPlan,
    hair: &VoxelSet,
    options: CutterReplayOptions,
) -> Result<Option<JointAngles>, SimError> {
    if plan.positioning_trajectory.is_empty() {
        return Ok(None);
    }

    let tips = verify_waypoints(challenge, &plan.positioning_trajectory, None, options)?;

    for pair in tips.positions.windows(2) {
        let hits = find_swept_voxel_hits(
            pair[0],
            pair[1],
            hair,
            &challenge.voxel_config,
            challenge.robot_config.geometry.tool_radius,
        );
        if !hits.is_empty() {
            return reject(
                CutterRejection::EntryCutsHair,
                None,
                format!(
                    "entry trajectory removes {} voxel(s) before the program starts",
                    hits.len()
                ),
            );
        }
    }

    Ok(tips.last_angles)
}

fn verify_step(
    challenge: &ChallengeDefinition,
    step: &CutterTrajectoryStep,
    options: CutterReplayOptions,
) -> Result<SegmentTips, SimError> {
    if step.waypoints.is_empty() {
        return reject(
            CutterRejection::TimelineInvalid,
            Some(step.source_block_id.clone()),
            format!("step {} carries no waypoints", step.index),
        );
    }
    if !step.duration_ms.is_finite() || step.duration_ms < 0.0 {
        return reject(
            CutterRejection::TimelineInvalid,
            Some(step.source_block_id.clone()),
            format!("step {} has duration {}", step.index, step.duration_ms),
        );
    }
    if step.kind == CutterTrajectoryStepKind::MoveCell && step.waypoints.len() < 2 {
        return reject(
            CutterRejection::TimelineInvalid,
            Some(step.source_block_id.clone()),
            format!(
                "step {} crosses a cell in a single waypoint, so it sweeps nothing",
                step.index
            ),
        );
    }

    check_timeline(step)?;

    let tips = verify_waypoints(challenge, &step.waypoints, Some(&step.source_block_id), options)?;

    if step.kind == CutterTrajectoryStepKind::MoveCell {
        let direction = direction_between(step.start_coord, step.end_coord).ok_or_else(|| {
            rejection(
                CutterRejection::CoordDiscontinuity,
                Some(step.source_block_id.clone()),
                format!("step {} is not a single-cell move", step.index),
            )
        })?;
        check_axis_displacement(challenge, step, direction, &tips.positions, options)?;
        check_path_deviation(challenge, step, &tips.positions, options)?;
    }

    Ok(tips)
}

/// Timestamps must start at zero, never go backwards, and end where the step
/// says it ends. A step whose waypoints outlast its declared duration would be
/// cutting on time nobody was charged for.
fn check_timeline(step: &CutterTrajectoryStep) -> Result<(), SimError> {
    let first = step.waypoints[0].time_ms;
    if first != 0.0 {
        return reject(
            CutterRejection::TimelineInvalid,
            Some(step.source_block_id.clone()),
            format!("step {} opens at {first}ms rather than 0", step.index),
        );
    }

    let mut previous = 0.0_f64;
    for waypoint in &step.waypoints {
        if !waypoint.time_ms.is_finite() || waypoint.time_ms < previous {
            return reject(
                CutterRejection::TimelineInvalid,
                Some(step.source_block_id.clone()),
                format!(
                    "step {} has a waypoint at {}ms after one at {previous}ms",
                    step.index, waypoint.time_ms
                ),
            );
        }
        previous = waypoint.time_ms;
    }

    if (previous - step.duration_ms).abs() > 1e-6 {
        return reject(
            CutterRejection::TimelineInvalid,
            Some(step.source_block_id.clone()),
            format!(
                "step {} runs to {previous}ms but declares {}ms",
                step.index, step.duration_ms
            ),
        );
    }
    Ok(())
}

/// Every pose in a segment: joints defined and in range, tip where it is claimed
/// to be, and the arm clear of the head.
fn verify_waypoints(
    challenge: &ChallengeDefinition,
    waypoints: &[CutterTrajectoryWaypoint],
    source_block_id: Option<&String>,
    options: CutterReplayOptions,
) -> Result<SegmentTips, SimError> {
    let mut positions = Vec::with_capacity(waypoints.len());
    let mut first_angles = None;
    let mut last_angles = None;

    for (index, waypoint) in waypoints.iter().enumerate() {
        let mut angles = JointAngles::default();

        // Exactly the configured joints: a missing one would be silently held at
        // its initial value, and an extra one is a client describing an arm the
        // challenge does not define.
        if waypoint.joint_angles.len() != challenge.robot_config.joints.len() {
            return reject(
                CutterRejection::JointLimit,
                source_block_id.cloned(),
                format!(
                    "waypoint {index} defines {} joints, the arm has {}",
                    waypoint.joint_angles.len(),
                    challenge.robot_config.joints.len()
                ),
            );
        }

        for joint in &challenge.robot_config.joints {
            let angle = waypoint.joint_angles.get(&joint.id).copied().ok_or_else(|| {
                SimError::MissingJoint {
                    joint_id: joint.id.clone(),
                }
            })?;
            if !angle.is_finite() {
                return Err(SimError::MissingJoint {
                    joint_id: joint.id.clone(),
                });
            }
            if angle < joint.min_angle_deg - options.joint_limit_epsilon
                || angle > joint.max_angle_deg + options.joint_limit_epsilon
            {
                return Err(SimError::AngleOutOfRange {
                    joint_id: joint.id.clone(),
                    angle_deg: angle,
                    min_angle_deg: joint.min_angle_deg,
                    max_angle_deg: joint.max_angle_deg,
                });
            }
            angles.set(&joint.id, angle);
        }

        let pose = compute_robot_pose(&challenge.robot_config, &angles)?;

        let drift = distance(pose.end_effector, waypoint.end_effector);
        if drift > options.end_effector_tolerance {
            return reject(
                CutterRejection::EndEffectorMismatch,
                source_block_id.cloned(),
                format!(
                    "waypoint {index} claims the tip at {:?} but its angles put it at {:?} ({drift:.3e} away)",
                    waypoint.end_effector, pose.end_effector
                ),
            );
        }

        if let Some(collision) = find_robot_head_collision(
            &pose,
            &challenge.voxel_config,
            &challenge.robot_config.geometry,
        ) {
            return reject(
                CutterRejection::HeadCollision,
                source_block_id.cloned(),
                format!(
                    "waypoint {index} puts the {} inside the head",
                    collision.part.label()
                ),
            );
        }

        positions.push(pose.end_effector);
        if index == 0 {
            first_angles = Some(angles.clone());
        }
        last_angles = Some(angles);
    }

    Ok(SegmentTips {
        positions,
        first_angles,
        last_angles,
    })
}

/// A step must open from the pose the previous one closed in.
///
/// Without this the arm could jump between steps — and a jump is a straight line
/// through whatever lies between, which is how a program would cut hair it never
/// travelled past.
fn check_pose_continuity(
    step: &CutterTrajectoryStep,
    closing: &JointAngles,
    opening: &JointAngles,
    epsilon: f64,
) -> Result<(), SimError> {
    for (joint_id, angle) in closing.iter() {
        let next = opening.get(joint_id).unwrap_or(f64::NAN);
        if !(next - angle).abs().le(&epsilon) {
            return reject(
                CutterRejection::PoseDiscontinuity,
                Some(step.source_block_id.clone()),
                format!(
                    "step {} opens with {joint_id} at {next}° but the previous step left it at {angle}°",
                    step.index
                ),
            );
        }
    }
    Ok(())
}

/// One cell means one cell: the net tip displacement must be a single voxel edge
/// along the declared axis, and nothing along the other two.
fn check_axis_displacement(
    challenge: &ChallengeDefinition,
    step: &CutterTrajectoryStep,
    direction: CutterGridDirection,
    positions: &[Vec3],
    options: CutterReplayOptions,
) -> Result<(), SimError> {
    let (Some(start), Some(end)) = (positions.first(), positions.last()) else {
        return Ok(());
    };
    let size = challenge.voxel_config.size;
    let tolerance = size / options.path_deviation_divisor;

    let mut expected = [0.0_f64; 3];
    expected[direction.axis()] = direction.sign() * size;

    for axis in 0..3 {
        let actual = end[axis] - start[axis];
        if (actual - expected[axis]).abs() > tolerance {
            return reject(
                CutterRejection::AxisDisplacement,
                Some(step.source_block_id.clone()),
                format!(
                    "step {} moves {actual:.4} along axis {axis}, expected {:.4}",
                    step.index, expected[axis]
                ),
            );
        }
    }
    Ok(())
}

/// Waypoints must hug the straight line between the step's endpoints.
///
/// The endpoints alone do not constrain the middle: a move could leave a cell,
/// sweep an arc through the hairstyle and return to the right place. The tool
/// cuts everything it passes, so the path is as much a claim as the destination.
fn check_path_deviation(
    challenge: &ChallengeDefinition,
    step: &CutterTrajectoryStep,
    positions: &[Vec3],
    options: CutterReplayOptions,
) -> Result<(), SimError> {
    let (Some(start), Some(end)) = (positions.first(), positions.last()) else {
        return Ok(());
    };
    let tolerance = challenge.voxel_config.size / options.path_deviation_divisor;

    for (index, point) in positions.iter().enumerate() {
        let deviation = distance_to_segment(*point, *start, *end);
        if deviation > tolerance {
            return reject(
                CutterRejection::PathDeviation,
                Some(step.source_block_id.clone()),
                format!(
                    "step {} waypoint {index} sits {deviation:.4} off the straight path, over {tolerance:.4}",
                    step.index
                ),
            );
        }
    }
    Ok(())
}

fn compare_with_client(
    plan: &CutterTrajectoryPlan,
    initial: &VoxelSet,
    remaining: &VoxelSet,
    metrics: &ProgramMetrics,
) -> CutterDivergence {
    let command_count_matches = plan.executed_command_count == metrics.executed_command_count;

    if plan.expected_result_voxels.is_empty() {
        return CutterDivergence {
            command_count_matches,
            result_voxels_match: None,
            server_only_cuts: 0,
            client_only_cuts: 0,
        };
    }

    let client_remaining: BTreeSet<&str> = plan
        .expected_result_voxels
        .iter()
        .map(String::as_str)
        .collect();
    let server_removed: Vec<String> = initial
        .iter()
        .filter(|coord| !remaining.contains(*coord))
        .map(|coord| format!("{},{},{}", coord.x, coord.y, coord.z))
        .collect();

    let mut server_only_cuts = 0;
    for key in &server_removed {
        if client_remaining.contains(key.as_str()) {
            server_only_cuts += 1;
        }
    }

    let server_removed_set: BTreeSet<&str> = server_removed.iter().map(String::as_str).collect();
    let mut client_only_cuts = 0;
    for coord in initial {
        let key = format!("{},{},{}", coord.x, coord.y, coord.z);
        if !client_remaining.contains(key.as_str()) && !server_removed_set.contains(key.as_str()) {
            client_only_cuts += 1;
        }
    }

    CutterDivergence {
        command_count_matches,
        result_voxels_match: Some(server_only_cuts == 0 && client_only_cuts == 0),
        server_only_cuts,
        client_only_cuts,
    }
}

fn direction_between(start: CutterGridCoord, end: CutterGridCoord) -> Option<CutterGridDirection> {
    let delta = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    [
        CutterGridDirection::Right,
        CutterGridDirection::Left,
        CutterGridDirection::Up,
        CutterGridDirection::Down,
        CutterGridDirection::Forward,
        CutterGridDirection::Backward,
    ]
    .into_iter()
    .find(|direction| direction.delta() == delta)
}

fn apply_delta(coord: CutterGridCoord, delta: CutterGridCoord) -> CutterGridCoord {
    [
        coord[0] + delta[0],
        coord[1] + delta[1],
        coord[2] + delta[2],
    ]
}

fn distance(left: Vec3, right: Vec3) -> f64 {
    let dx = left[0] - right[0];
    let dy = left[1] - right[1];
    let dz = left[2] - right[2];
    libm::sqrt(dx * dx + dy * dy + dz * dz)
}

fn distance_to_segment(point: Vec3, start: Vec3, end: Vec3) -> f64 {
    let direction = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    let length_squared =
        direction[0] * direction[0] + direction[1] * direction[1] + direction[2] * direction[2];
    if length_squared == 0.0 {
        return distance(point, start);
    }
    let offset = [
        point[0] - start[0],
        point[1] - start[1],
        point[2] - start[2],
    ];
    let t = ((offset[0] * direction[0] + offset[1] * direction[1] + offset[2] * direction[2])
        / length_squared)
        .clamp(0.0, 1.0);
    let closest = [
        start[0] + direction[0] * t,
        start[1] + direction[1] * t,
        start[2] + direction[2] * t,
    ];
    distance(point, closest)
}

fn rejection(
    rejection: CutterRejection,
    source_block_id: Option<String>,
    detail: String,
) -> SimError {
    SimError::CutterPlanRejected {
        rejection,
        source_block_id,
        detail,
    }
}

fn reject<T>(
    kind: CutterRejection,
    source_block_id: Option<String>,
    detail: String,
) -> Result<T, SimError> {
    Err(rejection(kind, source_block_id, detail))
}
