//! Cutter Grid: the player drives the tool tip through a fixed world-axis
//! lattice instead of commanding joints directly.
//!
//! These mirror `src/features/cutter-grid/types.ts` on the frontend. They are a
//! **separate, additive** family: the frozen v1 [`crate::domain::Program`] and
//! [`crate::domain::RobotCommand`] are untouched, exactly as SPEC v0.3 §15.4
//! requires. A server that ignores this module still speaks a complete v1.
//!
//! # Two representations, and why both travel
//!
//! A Cutter Grid submission carries the player's IR ([`CutterGridProgram`]) *and*
//! the frozen trajectory the browser planned from it ([`CutterTrajectoryPlan`]).
//! That looks redundant and is not:
//!
//! * The **IR** is what the player wrote. It is small, it is what the command cap
//!   applies to, and the server re-expands it rather than trusting any
//!   client-supplied expansion (`docs/backend/README.md` decision D3).
//! * The **trajectory** is how the arm actually got there. Cutter Grid resolves
//!   inverse kinematics at compile time in a Web Worker, and the hair that comes
//!   off is swept along the *realised* joint path, not along the ideal lattice
//!   line. Without the trajectory the server would be scoring a different motion
//!   from the one the learner watched.
//!
//! # The trajectory is evidence, not authority
//!
//! `trajectorySignature` is `fnv1a64` over the plan itself, so a client that
//! fabricates a plan can trivially compute a matching signature. It detects
//! corruption, never forgery. Everything that matters — joint limits, head
//! clearance, that each step really moves one cell along the axis it claims, that
//! the declared end-effector is where forward kinematics actually puts it — is
//! re-derived by [`hcr_sim`](../hcr_sim/index.html) from the joint angles alone.
//! See `docs/backend/08-CUTTER-GRID.md` §4.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::domain::{JointId, Vec3};

/// Planner build that produced a V2 plan.
///
/// Pinned rather than free text: `cutter-grid-dls-v1` and `cutter-grid-ladder-v2`
/// disagree about which programs are reachable, so accepting a V1 asset as a V2
/// asset would silently score against the wrong feasibility rules.
pub const CUTTER_GRID_LADDER_PLANNER_VERSION: &str = "cutter-grid-ladder-v2";

/// Wire version of the trajectory plans this server accepts.
pub const CUTTER_TRAJECTORY_PLAN_VERSION: u32 = 2;

/// Integer lattice coordinate in Cutter Grid's own logical frame.
///
/// Logical, not hair-lattice: the origin is wherever the certified entry pose put
/// the tool, so `[0, 0, 0]` is the start of every program. The server never needs
/// to resolve it to a hair coordinate — world positions come from forward
/// kinematics — so the offset stays a client-side concern.
pub type CutterGridCoord = [i32; 3];

/// The six fixed world-axis directions a move may take.
///
/// Fixed to the world, never to the camera or the tool: `Forward` is `-Z`
/// whichever way the arm happens to be pointing. That is what makes the mode
/// teachable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CutterGridDirection {
    /// `+X`.
    Right,
    /// `-X`.
    Left,
    /// `+Y`.
    Up,
    /// `-Y`.
    Down,
    /// `-Z`.
    Forward,
    /// `+Z`.
    Backward,
}

impl CutterGridDirection {
    /// Lattice step this direction takes, one cell.
    ///
    /// Mirrors `CUTTER_GRID_DIRECTION_DELTA` in `features/cutter-grid/grid.ts`.
    pub fn delta(self) -> CutterGridCoord {
        match self {
            CutterGridDirection::Right => [1, 0, 0],
            CutterGridDirection::Left => [-1, 0, 0],
            CutterGridDirection::Up => [0, 1, 0],
            CutterGridDirection::Down => [0, -1, 0],
            CutterGridDirection::Forward => [0, 0, -1],
            CutterGridDirection::Backward => [0, 0, 1],
        }
    }

    /// Which world axis this direction moves along, as an index into a [`Vec3`].
    pub fn axis(self) -> usize {
        match self {
            CutterGridDirection::Right | CutterGridDirection::Left => 0,
            CutterGridDirection::Up | CutterGridDirection::Down => 1,
            CutterGridDirection::Forward | CutterGridDirection::Backward => 2,
        }
    }

    /// `+1` or `-1` along [`Self::axis`].
    pub fn sign(self) -> f64 {
        match self {
            CutterGridDirection::Right
            | CutterGridDirection::Up
            | CutterGridDirection::Backward => 1.0,
            CutterGridDirection::Left
            | CutterGridDirection::Down
            | CutterGridDirection::Forward => -1.0,
        }
    }
}

/// A node of the player's Cutter Grid program.
///
/// The `Repeat` body is expanded away by the server before anything is checked,
/// so the atomic-command cap applies to what actually runs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CutterGridNode {
    /// Travel `distance` whole cells along a fixed world axis.
    #[serde(rename_all = "camelCase")]
    Move {
        /// Which way.
        direction: CutterGridDirection,
        /// Cells to travel, 1..=12 as enforced by the editor.
        distance: u32,
        /// Originating Blockly block, for error attribution.
        source_block_id: String,
    },
    /// Hold position.
    #[serde(rename_all = "camelCase")]
    Wait {
        /// Duration in milliseconds, 0..=5000.
        duration_ms: f64,
        /// Originating Blockly block.
        source_block_id: String,
    },
    /// Repeat `body` `count` times.
    #[serde(rename_all = "camelCase")]
    Repeat {
        /// Iteration count, 1..=20.
        count: u32,
        /// Nested nodes.
        body: Vec<CutterGridNode>,
        /// Originating Blockly block.
        source_block_id: String,
    },
}

impl CutterGridNode {
    /// Blockly block this node came from.
    pub fn source_block_id(&self) -> &str {
        match self {
            CutterGridNode::Move { source_block_id, .. }
            | CutterGridNode::Wait { source_block_id, .. }
            | CutterGridNode::Repeat { source_block_id, .. } => source_block_id,
        }
    }
}

/// One indivisible thing the arm does: cross one cell, or wait.
///
/// The product of expanding a [`CutterGridProgram`]. `Move { distance: 3 }`
/// becomes three [`CutterGridAction::MoveCell`]s, which is also why it counts
/// three against the command budget.
#[derive(Debug, Clone, PartialEq)]
pub enum CutterGridAction {
    /// Cross one cell in a fixed direction.
    MoveCell {
        /// Which way.
        direction: CutterGridDirection,
        /// Originating Blockly block.
        source_block_id: String,
    },
    /// Hold position.
    Wait {
        /// Duration in milliseconds.
        duration_ms: f64,
        /// Originating Blockly block.
        source_block_id: String,
    },
}

impl CutterGridAction {
    /// Blockly block this action came from.
    pub fn source_block_id(&self) -> &str {
        match self {
            CutterGridAction::MoveCell { source_block_id, .. }
            | CutterGridAction::Wait { source_block_id, .. } => source_block_id,
        }
    }
}

/// The player's program, as written.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridProgram {
    /// Always `"cutter-grid"`. Present so a reader can tell the two program
    /// families apart without inspecting the nodes.
    pub kind: String,
    /// IR version. `1` — the player-facing language did not change when the
    /// planner went to V2.
    pub version: u32,
    /// Planner build the client compiled against.
    pub planner_version: String,
    /// Top-level nodes.
    pub nodes: Vec<CutterGridNode>,
    /// Enabled, non-shadow blocks in the source workspace.
    pub source_block_count: u32,
}

/// One sample of the frozen trajectory.
///
/// `jointAngles` are servo degrees, as everywhere else on the wire.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryWaypoint {
    /// Milliseconds from the start of the step that owns this waypoint.
    pub time_ms: f64,
    /// Servo degrees per joint.
    ///
    /// `BTreeMap` rather than a struct: joint ids come from the challenge, and a
    /// fixed struct would have to be edited every time the arm gains an axis.
    /// The ordering is a determinism nicety — iteration order is stable.
    pub joint_angles: BTreeMap<JointId, f64>,
    /// Servo degrees per second per joint. Carried for the client's Hermite
    /// playback; the server samples at waypoints and does not read it.
    #[serde(default)]
    pub joint_velocities_deg_per_sec: BTreeMap<JointId, f64>,
    /// Where the client says the tool tip is.
    ///
    /// **Checked, never trusted.** The server recomputes it from `joint_angles`
    /// and rejects the plan if the two disagree beyond tolerance — otherwise a
    /// client could declare a harmless path while claiming a pose that cuts.
    pub end_effector: Vec3,
}

/// What a trajectory step does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CutterTrajectoryStepKind {
    /// Cross one cell.
    MoveCell,
    /// Hold position.
    Wait,
}

/// One expanded action, with the motion that realises it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryStep {
    /// Position in the step list. Echoed by the client; the server checks it
    /// rather than relying on it.
    pub index: u32,
    /// Move or wait.
    pub kind: CutterTrajectoryStepKind,
    /// Originating Blockly block.
    pub source_block_id: String,
    /// Logical cell the step starts in.
    pub start_coord: CutterGridCoord,
    /// Logical cell it ends in. Equal to `start_coord` for a wait.
    pub end_coord: CutterGridCoord,
    /// Step duration in milliseconds.
    pub duration_ms: f64,
    /// Samples along the motion, `timeMs` ascending from 0.
    pub waypoints: Vec<CutterTrajectoryWaypoint>,
    /// Voxel keys the client expects this step to remove. Advisory — the server
    /// carves from its own sweep and only compares.
    #[serde(default)]
    pub expected_cut_voxels: Vec<String>,
}

/// Planner telemetry. Recorded, never acted on.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPlanningDiagnostics {
    /// Entry pose the plan was built from.
    pub entry_option_id: String,
    /// Cartesian layers the ladder search expanded.
    pub cartesian_layer_count: u32,
    /// Surviving IK candidates per layer.
    #[serde(default)]
    pub candidate_counts: Vec<u32>,
    /// Halton seed budget the search settled on.
    pub seed_budget_used: u32,
    /// Closest approach to the head over the whole plan.
    pub minimum_head_clearance: f64,
    /// Tightest joint-limit margin over the whole plan.
    pub minimum_joint_limit_margin: f64,
    /// Largest normalized joint step between adjacent waypoints.
    pub maximum_normalized_joint_step: f64,
}

/// The frozen trajectory a Cutter Grid program compiles to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryPlan {
    /// Always `"cutter-grid-trajectory"`.
    pub kind: String,
    /// Plan version. Must be [`CUTTER_TRAJECTORY_PLAN_VERSION`].
    pub version: u32,
    /// Planner build. Must be [`CUTTER_GRID_LADDER_PLANNER_VERSION`].
    pub planner_version: String,
    /// `fnv1a64` over the challenge the client planned against. The server
    /// recomputes it from its own copy; a mismatch means the two are not talking
    /// about the same arm, lattice or hairstyle.
    pub challenge_signature: String,
    /// Which certified entry pose the plan starts from.
    pub entry_option_id: String,
    /// Getting the tool from its rest pose to the lattice origin.
    ///
    /// Not part of the program: it cuts nothing, costs no commands and is not
    /// charged to the player's time. The server verifies all three claims.
    #[serde(default)]
    pub positioning_trajectory: Vec<CutterTrajectoryWaypoint>,
    /// Logical cell the program starts in. Always the origin.
    pub start_coord: CutterGridCoord,
    /// Logical cell it ends in.
    pub end_coord: CutterGridCoord,
    /// One entry per expanded action.
    pub steps: Vec<CutterTrajectoryStep>,
    /// Hair the client expects to be left standing. Advisory; used only for
    /// divergence telemetry.
    #[serde(default)]
    pub expected_result_voxels: Vec<String>,
    /// Client's estimate of the run length.
    pub estimated_duration_ms: f64,
    /// Client's expansion size. Re-derived server-side and compared.
    pub executed_command_count: u32,
    /// Planner telemetry.
    pub diagnostics: CutterGridPlanningDiagnostics,
    /// `fnv1a64` over the plan without this field. Integrity, not authenticity.
    pub trajectory_signature: String,
}

/// The Cutter Grid half of a submission.
///
/// Rides on [`crate::api::SubmissionCreate`] as an optional field, so a servo
/// submission is byte-identical to what it was before this existed.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridSubmission {
    /// What the player wrote.
    pub program: CutterGridProgram,
    /// What the browser planned from it.
    pub plan: CutterTrajectoryPlan,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directions_agree_with_the_frontend_delta_table() {
        // `features/cutter-grid/grid.ts`. Forward is -Z and Backward is +Z: the
        // pair that is easiest to get backwards, and the one a sign error would
        // hide behind for longest, since a mirrored cut still scores non-zero.
        assert_eq!(CutterGridDirection::Right.delta(), [1, 0, 0]);
        assert_eq!(CutterGridDirection::Left.delta(), [-1, 0, 0]);
        assert_eq!(CutterGridDirection::Up.delta(), [0, 1, 0]);
        assert_eq!(CutterGridDirection::Down.delta(), [0, -1, 0]);
        assert_eq!(CutterGridDirection::Forward.delta(), [0, 0, -1]);
        assert_eq!(CutterGridDirection::Backward.delta(), [0, 0, 1]);
    }

    #[test]
    fn axis_and_sign_reconstruct_the_delta() {
        for direction in [
            CutterGridDirection::Right,
            CutterGridDirection::Left,
            CutterGridDirection::Up,
            CutterGridDirection::Down,
            CutterGridDirection::Forward,
            CutterGridDirection::Backward,
        ] {
            let mut rebuilt = [0i32; 3];
            rebuilt[direction.axis()] = direction.sign() as i32;
            assert_eq!(rebuilt, direction.delta(), "{direction:?}");
        }
    }

    #[test]
    fn directions_serialize_lowercase() {
        let json = serde_json::to_string(&CutterGridDirection::Backward).unwrap();
        assert_eq!(json, "\"backward\"");
    }

    #[test]
    fn nodes_use_the_frontend_tag_and_field_names() {
        let node = CutterGridNode::Move {
            direction: CutterGridDirection::Right,
            distance: 3,
            source_block_id: "block-1".into(),
        };
        let json = serde_json::to_string(&node).unwrap();
        assert_eq!(
            json,
            r#"{"type":"move","direction":"right","distance":3,"sourceBlockId":"block-1"}"#
        );
    }

    #[test]
    fn a_waypoint_without_velocities_still_parses() {
        // The field is playback-only, so a client that strips it to save
        // bandwidth must not be rejected.
        let json = r#"{
            "timeMs": 0,
            "jointAngles": { "baseYaw": 45 },
            "endEffector": [1.0, 2.0, 3.0]
        }"#;
        let waypoint: CutterTrajectoryWaypoint = serde_json::from_str(json).unwrap();
        assert!(waypoint.joint_velocities_deg_per_sec.is_empty());
        assert_eq!(waypoint.end_effector, [1.0, 2.0, 3.0]);
    }
}
