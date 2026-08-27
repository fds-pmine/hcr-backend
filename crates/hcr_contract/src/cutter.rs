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

/// Which editor a program was written in.
///
/// Not a rendering detail — it changes what a command *is*. One servo command
/// drives a joint to an angle; one Cutter Grid command crosses a single lattice
/// cell. The same challenge attempted in the two modes is two different tasks
/// with two different difficulties, which is why SPEC v0.3 §15.1 says their
/// scores are not to be compared for fairness, and why this has to travel on the
/// wire rather than be inferred.
///
/// `Servo` is the reference: it is the default, it has all the history, and
/// every row and round that predates Cutter Grid means it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProgrammingMode {
    /// Joint angles. The default.
    #[default]
    Servo,
    /// Tool tip through the lattice.
    CutterGrid,
}

impl ProgrammingMode {
    /// Every mode, in a stable order.
    pub const ALL: [ProgrammingMode; 2] = [ProgrammingMode::Servo, ProgrammingMode::CutterGrid];

    /// Stable identifier, matching the wire form.
    pub fn as_str(self) -> &'static str {
        match self {
            ProgrammingMode::Servo => "servo",
            ProgrammingMode::CutterGrid => "cutter-grid",
        }
    }

    /// Whether this is the reference mode, which is also what an absent value
    /// means everywhere the field is skipped.
    pub fn is_default(&self) -> bool {
        matches!(self, ProgrammingMode::Servo)
    }
}

/// Planner build that produced a V2 plan.
///
/// Pinned rather than free text: `cutter-grid-dls-v1` and `cutter-grid-ladder-v2`
/// disagree about which programs are reachable, so accepting a V1 asset as a V2
/// asset would silently score against the wrong feasibility rules.
pub const CUTTER_GRID_LADDER_PLANNER_VERSION: &str = "cutter-grid-ladder-v2";

/// Wire version of the trajectory plans this server accepts.
pub const CUTTER_TRAJECTORY_PLAN_VERSION: u32 = 2;

/// Planner build for compact synchronized point-to-point Cutter Grid plans.
///
/// This is deliberately separate from [`CUTTER_GRID_LADDER_PLANNER_VERSION`]:
/// V4 plans group a visible `Move N` into one action and carry analytic PTP
/// primitives, so treating a V2 plan as V4 would change both motion and cuts.
pub const CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION: &str = "cutter-grid-compact-ptp-v4";

/// Wire version of a compact PTP Cutter Grid trajectory.
pub const CUTTER_TRAJECTORY_PLAN_V4_VERSION: u32 = 4;

/// Version of the server-side Cutter Grid planning request/response pair.
pub const CUTTER_GRID_PLAN_API_VERSION: u32 = 1;

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

// ---------------------------------------------------------------------------
// Compact PTP V4 contracts
// ---------------------------------------------------------------------------
//
// The V4 family is intentionally additive. The legacy V2 submission above is
// still read by the historical trajectory verifier; a V4 request asks the
// server to plan from IR and never carries a client-owned Profile.

/// V4 uses the same player-visible source tree as [`CutterGridProgram`].
///
/// The planner version is checked by the V4 compiler rather than encoded in a
/// second JSON shape, preserving the frontend's `CutterGridProgramV4 extends
/// CutterGridProgramV1` contract.
pub type CutterGridProgramV4 = CutterGridProgram;

/// A finite bounds box in Cutter Grid's logical coordinate frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridBoundsV4 {
    /// Inclusive lower coordinate.
    pub min: CutterGridCoord,
    /// Inclusive upper coordinate.
    pub max: CutterGridCoord,
}

/// A static safe-IK result for a grid node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CutterGridStaticIkStatusV4 {
    /// At least one safe pose was certified for this coordinate.
    SafeCandidateKnown,
    /// The fixed search budget found no safe pose.
    NoSafeCandidateFound,
}

/// Static profile data for one lattice node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridNodeProfileV4 {
    /// Logical coordinate.
    pub coord: CutterGridCoord,
    /// Corresponding world point.
    pub world_position: Vec3,
    /// Whether a static safe IK candidate is known.
    pub static_ik_status: CutterGridStaticIkStatusV4,
    /// Candidate count at the certified search budget.
    pub candidate_count: u32,
    /// Halton seed budget used for the node certificate.
    pub seed_budget: u32,
}

/// A five-joint boundary state for a synchronized PTP primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryBoundaryStateV4 {
    /// Servo degrees per joint.
    pub joint_angles: BTreeMap<JointId, f64>,
    /// Servo degrees per second per joint.
    pub joint_velocities_deg_per_sec: BTreeMap<JointId, f64>,
    /// Servo degrees per second squared per joint.
    pub joint_accelerations_deg_per_sec2: BTreeMap<JointId, f64>,
}

/// One compact synchronized quintic point-to-point primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridSyncPtpPrimitiveV4 {
    /// Always `"sync-ptp"`.
    pub kind: String,
    /// Always `"synchronized-quintic"`.
    pub interpolation: String,
    /// Shared absolute duration for every joint.
    pub duration_ms: f64,
    /// State at primitive start.
    pub start: CutterTrajectoryBoundaryStateV4,
    /// State at primitive end.
    pub end: CutterTrajectoryBoundaryStateV4,
}

/// Hair-contact event proven by the compact primitive certificate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridContactEventV4 {
    /// Milliseconds from the owning action start.
    pub time_ms: f64,
    /// Sorted Hair voxel keys reached by this event.
    pub voxel_keys: Vec<String>,
}

/// A visible repeat-expanded action in a V4 trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CutterGridTrajectoryActionV4 {
    /// A single visible `Move N`, realized by one or two compact primitives.
    #[serde(rename_all = "camelCase")]
    Move {
        /// Stable occurrence inside the repeat-expanded program.
        occurrence_id: String,
        /// Blockly source block.
        source_block_id: String,
        /// Fixed world-axis direction.
        direction: CutterGridDirection,
        /// Whole-cell distance, 1 through 12.
        distance: u32,
        /// Logical action start.
        start_coord: CutterGridCoord,
        /// Logical action end.
        end_coord: CutterGridCoord,
        /// Logical player command cost, equal to `distance`.
        logical_command_count: u32,
        /// Direct motion, optionally followed by one obstacle detour segment.
        primitives: Vec<CutterGridSyncPtpPrimitiveV4>,
        /// Actual sphere-sweep contacts in action-relative time.
        contact_events: Vec<CutterGridContactEventV4>,
        /// Sorted union of this action's contacts.
        expected_cut_voxels: Vec<String>,
    },
    /// A visible hold action.
    #[serde(rename_all = "camelCase")]
    Wait {
        /// Stable occurrence inside the repeat-expanded program.
        occurrence_id: String,
        /// Blockly source block.
        source_block_id: String,
        /// Hold duration in milliseconds.
        duration_ms: f64,
        /// Always one logical command.
        logical_command_count: u32,
        /// Must remain empty: waiting never cuts.
        expected_cut_voxels: Vec<String>,
    },
}

/// System-only positioning movement to the selected logical origin.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPositioningPlanV4 {
    /// Certified origin configuration selected for this complete program.
    pub entry_option_id: String,
    /// One or more compact PTP primitives; never charged to the player.
    pub primitives: Vec<CutterGridSyncPtpPrimitiveV4>,
    /// Integrity signature for the positioning segment.
    pub trajectory_signature: String,
}

/// Per-joint hard dynamic limits and nominal values.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridJointMotionLimitsV4 {
    /// Nominal velocity before the requested speed scale.
    pub nominal_velocity_deg_per_sec: f64,
    /// Nominal acceleration before the requested speed scale.
    pub nominal_acceleration_deg_per_sec2: f64,
    /// Nominal jerk before the requested speed scale.
    pub nominal_jerk_deg_per_sec3: f64,
    /// Hard velocity limit.
    pub max_velocity_deg_per_sec: f64,
    /// Hard acceleration limit.
    pub max_acceleration_deg_per_sec2: f64,
    /// Hard jerk limit.
    pub max_jerk_deg_per_sec3: f64,
}

/// Dynamic limits shared by every V4 primitive.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridMotionLimitsV4 {
    /// Requested dynamic scale. V4's shipped value is `1.0`.
    pub requested_speed_scale: f64,
    /// Limits indexed by stable joint id.
    pub joints: BTreeMap<JointId, CutterGridJointMotionLimitsV4>,
}

/// Deterministic compact-planner diagnostics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPlanningDiagnosticsV4 {
    /// Number of endpoint layers, excluding waits.
    pub endpoint_layer_count: u32,
    /// Candidates retained at each endpoint layer.
    pub candidate_counts: Vec<u32>,
    /// First action expanded beyond its initial candidate budget, when any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expanded_action_index: Option<u32>,
    /// Selected direct PTP primitive count.
    pub direct_primitive_count: u32,
    /// Selected detour PTP primitive count.
    pub detour_primitive_count: u32,
    /// Minimum certified head clearance.
    pub minimum_head_clearance: f64,
    /// Tightest certified joint-limit margin.
    pub minimum_joint_limit_margin: f64,
    /// Largest normalized joint change between connected states.
    pub maximum_normalized_joint_step: f64,
    /// Largest chord deviation of a selected primitive.
    pub maximum_end_effector_chord_deviation: f64,
    /// Requested speed scale.
    pub requested_speed_scale: f64,
    /// Realized speed scale after hard-limit clamping.
    pub actual_speed_scale: f64,
    /// Maximum measured velocity ratio.
    pub maximum_velocity_ratio: f64,
    /// Maximum measured acceleration ratio.
    pub maximum_acceleration_ratio: f64,
    /// Maximum measured jerk ratio.
    pub maximum_jerk_ratio: f64,
    /// Internal certification sample count; samples themselves are never serialized.
    pub adaptive_validation_sample_count: u32,
}

/// The compact, frozen V4 trajectory returned by a planner.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryPlanV4 {
    /// Always `"cutter-grid-trajectory"`.
    pub kind: String,
    /// Must be [`CUTTER_TRAJECTORY_PLAN_V4_VERSION`].
    pub version: u32,
    /// Must be [`CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION`].
    pub planner_version: String,
    /// Signature of the Challenge used by the planner.
    pub challenge_signature: String,
    /// System-only origin positioning.
    pub positioning: CutterGridPositioningPlanV4,
    /// Logical player start, always the origin.
    pub start_coord: CutterGridCoord,
    /// Logical player end.
    pub end_coord: CutterGridCoord,
    /// One entry per visible expanded Move or Wait.
    pub actions: Vec<CutterGridTrajectoryActionV4>,
    /// Sorted remaining Hair voxel keys after actual sweeps.
    pub expected_result_voxels: Vec<String>,
    /// Player movement and wait time only; excludes system positioning.
    pub estimated_duration_ms: f64,
    /// Expanded logical command count.
    pub executed_command_count: u32,
    /// Limits used to certify the plan.
    pub motion_limits: CutterGridMotionLimitsV4,
    /// Stable signature of [`Self::motion_limits`].
    pub motion_limits_signature: String,
    /// Compact planning and certification evidence.
    pub diagnostics: CutterGridPlanningDiagnosticsV4,
    /// Integrity signature for this plan; not an authentication primitive.
    pub trajectory_signature: String,
}

/// Certified V4 entry configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridEntryOptionV4 {
    /// Stable profile entry id.
    pub id: String,
    /// Safe origin joint state.
    pub joint_angles: BTreeMap<JointId, f64>,
    /// System positioning primitive from the Servo initial pose.
    pub positioning_primitive: CutterGridSyncPtpPrimitiveV4,
    /// Signature of the positioning primitive.
    pub positioning_signature: String,
    /// Certified minimum head clearance.
    pub minimum_head_clearance: f64,
}

/// A deterministic joint-space roadmap node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridRoadmapNodeV4 {
    /// Stable node id.
    pub id: String,
    /// Certified collision-free joint state.
    pub joint_angles: BTreeMap<JointId, f64>,
    /// Static head-clearance evidence.
    pub minimum_head_clearance: f64,
}

/// A directed roadmap edge.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridRoadmapEdgeV4 {
    /// Origin node id.
    pub from_node_id: String,
    /// Destination node id.
    pub to_node_id: String,
}

/// Static safe detour graph owned by the certified Profile.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridRoadmapV4 {
    /// Fixed Halton roadmap nodes.
    pub nodes: Vec<CutterGridRoadmapNodeV4>,
    /// Deterministic nearest-safe neighbor edges.
    pub edges: Vec<CutterGridRoadmapEdgeV4>,
    /// Integrity signature for the roadmap.
    pub signature: String,
}

/// Evidence that a Profile can safely expose Cutter Grid.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridCertificationV4 {
    /// Every required certificate passed.
    pub passed: bool,
    /// System positioning cuts no Hair voxel.
    pub entry_zero_contact: bool,
    /// Completion produced by the reference program.
    pub reference_completion: f64,
    /// Reference-program cuts.
    pub reference_cut_voxels: Vec<String>,
    /// Reference-program collateral cuts; must be empty for the shipped profile.
    pub reference_extra_cut_voxels: Vec<String>,
    /// Directions with at least one legal edge from the origin.
    pub certified_directions: Vec<CutterGridDirection>,
    /// V2 authenticated entry options carried into V4.
    pub authenticated_entry_option_ids: Vec<String>,
    /// Whether the reference trajectory was certified before V4 upgrade.
    pub reference_trajectory_certified: bool,
}

/// Server-owned V4 planning profile for one Challenge signature.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridProfileV4 {
    /// Must be `4`.
    pub version: u32,
    /// Must be [`CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION`].
    pub planner_version: String,
    /// Signature of the Challenge this profile certifies.
    pub challenge_signature: String,
    /// Hair-lattice coordinate selected as logical origin.
    pub origin_hair_coord: CutterGridCoord,
    /// World position of the logical origin.
    pub origin_world_position: Vec3,
    /// Finite logical grid bounds.
    pub bounds: CutterGridBoundsV4,
    /// Program-wide candidate entry states.
    pub entry_options: Vec<CutterGridEntryOptionV4>,
    /// Static lattice diagnostics for the overlay.
    pub nodes: Vec<CutterGridNodeProfileV4>,
    /// Certified perfect-cut program.
    pub reference_program: CutterGridProgramV4,
    /// Signature of the historical certified reference trajectory.
    pub reference_trajectory_signature: String,
    /// Static and reference-program gate evidence.
    pub certification: CutterGridCertificationV4,
    /// Dynamic hard limits.
    pub motion_limits: CutterGridMotionLimitsV4,
    /// Signature of the dynamic limits.
    pub motion_limits_signature: String,
    /// Fixed fallback detour graph.
    pub roadmap: CutterGridRoadmapV4,
    /// Signature covering the complete V4 Profile.
    pub profile_signature: String,
}

/// Stable V4 planner failure categories.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CutterGridPlanningErrorCodeV4 {
    /// The server has no certified planner/Profile for this challenge.
    PlannerNotReady,
    /// Client-side cancellation reached a cooperative planner.
    PlanningCancelled,
    /// Profile version or signature did not match the Challenge.
    ProfileV4Mismatch,
    /// A requested endpoint leaves the finite grid.
    OutOfBounds,
    /// No initial DLS seed reached the endpoint.
    EndpointIkNotConverged,
    /// The deterministic IK candidate budget was exhausted.
    EndpointIkSearchExhausted,
    /// Endpoint candidates exist but no safe compact PTP edge connects them.
    EndpointPtpDisconnected,
    /// A route would need more than two primitives for one visible Move.
    MotionPrimitiveBudgetExhausted,
    /// A compact PTP primitive intersects the head.
    PtpCollision,
    /// A compact PTP safety certificate could not be proven.
    PtpCertificateFailed,
    /// Actual tool sweep certification failed.
    ActualSweepCertificationFailed,
    /// The assembled plan failed its own integrity check.
    PlanSignatureMismatch,
}

impl CutterGridPlanningErrorCodeV4 {
    /// Stable wire spelling without relying on debug formatting.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PlannerNotReady => "planner-not-ready",
            Self::PlanningCancelled => "planning-cancelled",
            Self::ProfileV4Mismatch => "profile-v4-mismatch",
            Self::OutOfBounds => "out-of-bounds",
            Self::EndpointIkNotConverged => "endpoint-ik-not-converged",
            Self::EndpointIkSearchExhausted => "endpoint-ik-search-exhausted",
            Self::EndpointPtpDisconnected => "endpoint-ptp-disconnected",
            Self::MotionPrimitiveBudgetExhausted => "motion-primitive-budget-exhausted",
            Self::PtpCollision => "ptp-collision",
            Self::PtpCertificateFailed => "ptp-certificate-failed",
            Self::ActualSweepCertificationFailed => "actual-sweep-certification-failed",
            Self::PlanSignatureMismatch => "plan-signature-mismatch",
        }
    }
}

/// The planning pipeline stage that produced a failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CutterGridPlanningStageV4 {
    /// Profile loading or validation.
    Profile,
    /// Endpoint candidate generation.
    Endpoint,
    /// Direct compact PTP edge validation.
    PtpEdge,
    /// Joint-space roadmap fallback.
    Roadmap,
    /// Dynamic motion certification.
    MotionCertificate,
    /// Actual sweep certification.
    SweepCertificate,
    /// Canonical plan serialization and signature.
    Serialization,
}

impl CutterGridPlanningStageV4 {
    /// Stable wire spelling without relying on debug formatting.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Profile => "profile",
            Self::Endpoint => "endpoint",
            Self::PtpEdge => "ptp-edge",
            Self::Roadmap => "roadmap",
            Self::MotionCertificate => "motion-certificate",
            Self::SweepCertificate => "sweep-certificate",
            Self::Serialization => "serialization",
        }
    }
}

/// Input to `POST /api/v1/cutter-grid/plans`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPlanRequestV1 {
    /// Challenge id selected by the player.
    pub challenge_id: String,
    /// Pinned Challenge version.
    pub challenge_version: u32,
    /// Player-authored V4 Cutter Grid program.
    pub program: CutterGridProgramV4,
}

/// Successful result of server-side compact PTP planning.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPlanResponseV1 {
    /// Always `"cutter-grid-plan-result"`.
    pub kind: String,
    /// Must be [`CUTTER_GRID_PLAN_API_VERSION`].
    pub version: u32,
    /// Always `"hcr-sim-rust"` for this implementation.
    pub planner_implementation: String,
    /// Service build identifier, diagnostic only.
    pub planner_build: String,
    /// Selected server-owned Profile signature.
    pub profile_signature: String,
    /// Wall-clock request duration, diagnostic only and outside plan signatures.
    pub planning_duration_ms: f64,
    /// Frozen, executable compact PTP plan.
    pub plan: CutterTrajectoryPlanV4,
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

    #[test]
    fn v4_plan_request_keeps_the_v1_program_shape_and_uses_camel_case() {
        let request = CutterGridPlanRequestV1 {
            challenge_id: "neat-short-cap".into(),
            challenge_version: 1,
            program: CutterGridProgram {
                kind: "cutter-grid".into(),
                version: 1,
                planner_version: CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION.into(),
                nodes: vec![CutterGridNode::Move {
                    direction: CutterGridDirection::Up,
                    distance: 6,
                    source_block_id: "up-6".into(),
                }],
                source_block_count: 1,
            },
        };

        let json = serde_json::to_string(&request).unwrap();
        assert_eq!(
            json,
            r#"{"challengeId":"neat-short-cap","challengeVersion":1,"program":{"kind":"cutter-grid","version":1,"plannerVersion":"cutter-grid-compact-ptp-v4","nodes":[{"type":"move","direction":"up","distance":6,"sourceBlockId":"up-6"}],"sourceBlockCount":1}}"#
        );
    }

    #[test]
    fn v4_planning_failure_codes_match_the_frontend_spellings() {
        assert_eq!(
            serde_json::to_string(&CutterGridPlanningErrorCodeV4::EndpointPtpDisconnected)
                .unwrap(),
            "\"endpoint-ptp-disconnected\""
        );
        assert_eq!(
            CutterGridPlanningStageV4::MotionCertificate.as_str(),
            "motion-certificate"
        );
    }
}
