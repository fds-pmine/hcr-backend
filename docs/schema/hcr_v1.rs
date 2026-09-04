//! `hcr.v1` — Rust DTO mirror of `hcr-v1.d.ts`.
//!
//! Reference sketch for the `hcr_contract` crate. Every type is `no_std + alloc`
//! compatible so the firmware and the server share one definition of the wire.
//!
//! Conventions that make the TS file literal:
//!   * `#[serde(rename_all = "camelCase")]` on every struct.
//!   * TS discriminated unions -> `#[serde(tag = "type"|"kind"|"op", rename_all = "kebab-case")]`.
//!   * TS `number` -> `f64`, except counts/versions which are `u32`/`u64`.
//!   * TS optional (`?`) -> `Option<T>` with `#[serde(skip_serializing_if = "Option::is_none")]`.
//!
//! Encoding: JSON for browsers, CBOR (`minicbor`/`ciborium`) for MCUs. Both derive
//! from these types, so there is exactly one schema regardless of encoding.

#![cfg_attr(not(feature = "std"), no_std)]
extern crate alloc;

use alloc::{collections::BTreeMap, string::String, vec::Vec};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// 1. Frozen v1 domain types (mirror src/types/domain.ts — do not edit alone)
// ---------------------------------------------------------------------------

pub type JointId = String;
/// Canonical form `"{x},{y},{z}"`. Kept as a string so hashing matches the TS engine
/// byte-for-byte; see 02-DETERMINISM.md §5.
pub type VoxelKey = String;
pub type Vec3 = [f64; 3];

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Axis { X, Y, Z }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoxelCoord { pub x: i32, pub y: i32, pub z: i32 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JointConfig {
    pub id: JointId,
    pub name: String,
    pub axis: Axis,
    pub min_angle_deg: f64,
    pub max_angle_deg: f64,
    pub initial_angle_deg: f64,
    pub speed_deg_per_sec: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobotCollisionConfig {
    pub link_radius: f64,
    pub joint_radius: f64,
    pub tool_shaft_radius: f64,
    pub head_clearance: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RobotGeometryConfig {
    pub base_position: Vec3,
    pub shoulder_height: f64,
    pub upper_arm_length: f64,
    pub forearm_length: f64,
    pub tool_length: f64,
    pub tool_radius: f64,
    pub collision: RobotCollisionConfig,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoxelConfig {
    pub origin: Vec3,
    pub size: f64,
    pub head_center: Vec3,
    pub head_scale: Vec3,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreWeights { pub completion: f64, pub efficiency: f64, pub time: f64 }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoringConfig {
    pub weights: ScoreWeights,
    pub reference_program_cost: f64,
    pub reference_time_ms: f64,
    pub command_weight: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProgramMetrics {
    pub source_block_count: u32,
    pub executed_command_count: u32,
    pub estimated_duration_ms: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreResult {
    pub completion_score: f64,
    pub efficiency_score: f64,
    pub time_score: f64,
    pub final_score: f64,
    pub program_cost: f64,
}

/// Program IR. The contract's centre of gravity — browser sim, server replay and
/// firmware all execute this exact structure.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum RobotCommand {
    #[serde(rename_all = "camelCase")]
    SetJointAngle { joint_id: JointId, angle_deg: f64, source_block_id: String },
    #[serde(rename_all = "camelCase")]
    Wait { duration_ms: f64, source_block_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum ProgramNode {
    #[serde(rename_all = "camelCase")]
    SetJointAngle { joint_id: JointId, angle_deg: f64, source_block_id: String },
    #[serde(rename_all = "camelCase")]
    Wait { duration_ms: f64, source_block_id: String },
    #[serde(rename_all = "camelCase")]
    Repeat { count: u32, body: Vec<ProgramNode>, source_block_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Program { pub nodes: Vec<ProgramNode>, pub source_block_count: u32 }

// ---------------------------------------------------------------------------
// 2. Envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope<P> {
    pub v: u8,                       // always 1
    pub id: String,                  // ULID; idempotency key
    pub kind: String,
    pub ts: u64,                     // sender epoch-ms, informational only
    #[serde(skip_serializing_if = "Option::is_none")]
    pub corr: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub src: Option<ActorRef>,
    pub payload: P,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorRef {
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    pub id: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorType { User, Device, Service }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HcrError {
    pub code: HcrErrorCode,
    pub message: String,
    pub retryable: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, serde_json::Value>>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HcrErrorCode {
    Unauthorized, Forbidden,
    ChallengeNotFound,
    ProgramInvalid, ProgramTooLarge, WeightsInvalid,
    ItemRefInvalid,
    SessionNotFound, SessionTerminated, BankExhausted,
    MatchNotReady,
    /// A Cutter Grid trajectory failed verification; `details.rejection` names
    /// which audit. See `08-CUTTER-GRID.md` §6.
    TrajectoryRejected,
    /// A valid compact V4 Cutter Grid program could not be planned. The
    /// structured `plannerCode` and `stage` fields live in `details`.
    TrajectoryPlanningFailed,
    DeviceOffline, DeviceBusy,
    ReplayTimeout, RateLimited, Internal,
}

// ---------------------------------------------------------------------------
// 3. Catalog
// ---------------------------------------------------------------------------

/// Mirrors arona's `IRTParameters`, but serializable — arona's own type has no
/// serde derives (`arona/src/core/irt.rs:82-87`), so the backend owns this DTO and
/// converts at the boundary via `IRTParameters::new(Discrimination(a), Difficulty(b),
/// GuessingParam(c))`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemParameters { pub discrimination: f64, pub difficulty: f64, pub guessing: f64 }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum CalibrationState { Provisional, Online, Calibrated, Retired }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum SkillDimension { Kinematics, Sequencing, Iteration, Precision, Safety }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeMeta {
    pub version: u32,
    pub irt: ItemParameters,
    pub calibration: CalibrationState,
    pub response_count: u32,
    pub dimensions: Vec<SkillDimension>,
    pub mastery_threshold: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<GeneratorProvenance>,
    pub hardware_compatible: bool,
    /// Editors this item can be attempted in. Defaults to servo alone.
    #[serde(default)]
    pub programming_modes: Vec<ProgrammingMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratorProvenance {
    pub family_id: String,
    pub seed: u64,
    pub params: BTreeMap<String, f64>,
    pub version: String,
}

// ---------------------------------------------------------------------------
// 4. Submission & scoring
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionCreate {
    pub submission_id: String,
    pub challenge_id: String,
    pub challenge_version: u32,
    pub program: Program,
    /// Set when the program was written in Cutter Grid. See `08-CUTTER-GRID.md`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cutter_grid: Option<CutterGridSubmission>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub item_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub client_preview: Option<ClientPreview>,
}

// --- Cutter Grid -----------------------------------------------------------
//
// Implemented for real in `crates/hcr_contract/src/cutter.rs`; this is the
// sketch. Additive — the frozen `Program` and `RobotCommand` are untouched.

/// Which editor a program was written in.
///
/// One servo command drives a joint; one Cutter Grid command crosses a lattice
/// cell. The same challenge is a different task with a different difficulty in
/// each, so this travels rather than being inferred. `Servo` is the reference.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProgrammingMode {
    #[default]
    Servo,
    CutterGrid,
}

impl ProgrammingMode {
    /// Whether this is the reference mode, which is also what an absent value
    /// means everywhere the field is skipped.
    pub fn is_default(&self) -> bool {
        matches!(self, ProgrammingMode::Servo)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CutterGridDirection {
    Right,
    Left,
    Up,
    Down,
    Forward,
    Backward,
}

/// Logical lattice coordinate; `[0, 0, 0]` is the certified entry cell.
pub type CutterGridCoord = [i32; 3];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CutterGridNode {
    #[serde(rename_all = "camelCase")]
    Move {
        direction: CutterGridDirection,
        distance: u32,
        source_block_id: String,
    },
    #[serde(rename_all = "camelCase")]
    Wait {
        duration_ms: f64,
        source_block_id: String,
    },
    #[serde(rename_all = "camelCase")]
    Repeat {
        count: u32,
        body: Vec<CutterGridNode>,
        source_block_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridProgram {
    pub kind: String,
    pub version: u32,
    pub planner_version: String,
    pub nodes: Vec<CutterGridNode>,
    pub source_block_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryWaypoint {
    pub time_ms: f64,
    /// Servo degrees per joint.
    pub joint_angles: BTreeMap<String, f64>,
    /// Playback only; the server samples at waypoints.
    #[serde(default)]
    pub joint_velocities_deg_per_sec: BTreeMap<String, f64>,
    /// Checked against forward kinematics, never trusted.
    pub end_effector: Vec3,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CutterTrajectoryStepKind {
    MoveCell,
    Wait,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryStep {
    pub index: u32,
    pub kind: CutterTrajectoryStepKind,
    pub source_block_id: String,
    pub start_coord: CutterGridCoord,
    pub end_coord: CutterGridCoord,
    pub duration_ms: f64,
    pub waypoints: Vec<CutterTrajectoryWaypoint>,
    #[serde(default)]
    pub expected_cut_voxels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPlanningDiagnostics {
    pub entry_option_id: String,
    pub cartesian_layer_count: u32,
    #[serde(default)]
    pub candidate_counts: Vec<u32>,
    pub seed_budget_used: u32,
    pub minimum_head_clearance: f64,
    pub minimum_joint_limit_margin: f64,
    pub maximum_normalized_joint_step: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryPlan {
    pub kind: String,
    pub version: u32,
    pub planner_version: String,
    /// Recomputed server-side from its own copy of the challenge.
    pub challenge_signature: String,
    pub entry_option_id: String,
    /// Cuts nothing, costs no commands, charged no time — all three verified.
    #[serde(default)]
    pub positioning_trajectory: Vec<CutterTrajectoryWaypoint>,
    pub start_coord: CutterGridCoord,
    pub end_coord: CutterGridCoord,
    pub steps: Vec<CutterTrajectoryStep>,
    #[serde(default)]
    pub expected_result_voxels: Vec<String>,
    pub estimated_duration_ms: f64,
    pub executed_command_count: u32,
    pub diagnostics: CutterGridPlanningDiagnostics,
    /// Integrity, not authenticity — a forger can recompute it.
    pub trajectory_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridSubmission {
    pub program: CutterGridProgram,
    pub plan: CutterTrajectoryPlan,
}

// --- Cutter Grid V4 server planning ---------------------------------------
//
// V2 above remains the verification-only compatibility shape. V4 sends only a
// program and Challenge reference to the server; the server owns the Profile
// and produces the compact PTP plan below.

pub type CutterGridProgramV4 = CutterGridProgram;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridBoundsV4 {
    pub min: CutterGridCoord,
    pub max: CutterGridCoord,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CutterGridStaticIkStatusV4 { SafeCandidateKnown, NoSafeCandidateFound }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridNodeProfileV4 {
    pub coord: CutterGridCoord,
    pub world_position: Vec3,
    pub static_ik_status: CutterGridStaticIkStatusV4,
    pub candidate_count: u32,
    pub seed_budget: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryBoundaryStateV4 {
    pub joint_angles: BTreeMap<JointId, f64>,
    pub joint_velocities_deg_per_sec: BTreeMap<JointId, f64>,
    pub joint_accelerations_deg_per_sec2: BTreeMap<JointId, f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridSyncPtpPrimitiveV4 {
    pub kind: String,
    pub interpolation: String,
    pub duration_ms: f64,
    pub start: CutterTrajectoryBoundaryStateV4,
    pub end: CutterTrajectoryBoundaryStateV4,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridContactEventV4 { pub time_ms: f64, pub voxel_keys: Vec<String> }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum CutterGridTrajectoryActionV4 {
    #[serde(rename_all = "camelCase")]
    Move {
        occurrence_id: String,
        source_block_id: String,
        direction: CutterGridDirection,
        distance: u32,
        start_coord: CutterGridCoord,
        end_coord: CutterGridCoord,
        logical_command_count: u32,
        /// Exactly one direct primitive or one detour represented by two.
        primitives: Vec<CutterGridSyncPtpPrimitiveV4>,
        contact_events: Vec<CutterGridContactEventV4>,
        expected_cut_voxels: Vec<String>,
    },
    #[serde(rename_all = "camelCase")]
    Wait {
        occurrence_id: String,
        source_block_id: String,
        duration_ms: f64,
        logical_command_count: u32,
        expected_cut_voxels: Vec<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPositioningPlanV4 {
    pub entry_option_id: String,
    pub primitives: Vec<CutterGridSyncPtpPrimitiveV4>,
    pub trajectory_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridJointMotionLimitsV4 {
    pub nominal_velocity_deg_per_sec: f64,
    pub nominal_acceleration_deg_per_sec2: f64,
    pub nominal_jerk_deg_per_sec3: f64,
    pub max_velocity_deg_per_sec: f64,
    pub max_acceleration_deg_per_sec2: f64,
    pub max_jerk_deg_per_sec3: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridMotionLimitsV4 {
    pub requested_speed_scale: f64,
    pub joints: BTreeMap<JointId, CutterGridJointMotionLimitsV4>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPlanningDiagnosticsV4 {
    pub endpoint_layer_count: u32,
    pub candidate_counts: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expanded_action_index: Option<u32>,
    pub direct_primitive_count: u32,
    pub detour_primitive_count: u32,
    pub minimum_head_clearance: f64,
    pub minimum_joint_limit_margin: f64,
    pub maximum_normalized_joint_step: f64,
    pub maximum_end_effector_chord_deviation: f64,
    pub requested_speed_scale: f64,
    pub actual_speed_scale: f64,
    pub maximum_velocity_ratio: f64,
    pub maximum_acceleration_ratio: f64,
    pub maximum_jerk_ratio: f64,
    pub adaptive_validation_sample_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterTrajectoryPlanV4 {
    pub kind: String,
    pub version: u32,
    pub planner_version: String,
    pub challenge_signature: String,
    pub positioning: CutterGridPositioningPlanV4,
    pub start_coord: CutterGridCoord,
    pub end_coord: CutterGridCoord,
    pub actions: Vec<CutterGridTrajectoryActionV4>,
    pub expected_result_voxels: Vec<String>,
    pub estimated_duration_ms: f64,
    pub executed_command_count: u32,
    pub motion_limits: CutterGridMotionLimitsV4,
    pub motion_limits_signature: String,
    pub diagnostics: CutterGridPlanningDiagnosticsV4,
    pub trajectory_signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridEntryOptionV4 {
    pub id: String,
    pub joint_angles: BTreeMap<JointId, f64>,
    pub positioning_primitive: CutterGridSyncPtpPrimitiveV4,
    pub positioning_signature: String,
    pub minimum_head_clearance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridRoadmapNodeV4 {
    pub id: String,
    pub joint_angles: BTreeMap<JointId, f64>,
    pub minimum_head_clearance: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridRoadmapEdgeV4 { pub from_node_id: String, pub to_node_id: String }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridRoadmapV4 {
    pub nodes: Vec<CutterGridRoadmapNodeV4>,
    pub edges: Vec<CutterGridRoadmapEdgeV4>,
    pub signature: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridCertificationV4 {
    pub passed: bool,
    pub entry_zero_contact: bool,
    pub reference_completion: f64,
    pub reference_cut_voxels: Vec<String>,
    pub reference_extra_cut_voxels: Vec<String>,
    pub certified_directions: Vec<CutterGridDirection>,
    pub authenticated_entry_option_ids: Vec<String>,
    pub reference_trajectory_certified: bool,
}

/// A startup/CI asset. It is deliberately not in any client request body.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridProfileV4 {
    pub version: u32,
    pub planner_version: String,
    pub challenge_signature: String,
    pub origin_hair_coord: CutterGridCoord,
    pub origin_world_position: Vec3,
    pub bounds: CutterGridBoundsV4,
    pub entry_options: Vec<CutterGridEntryOptionV4>,
    pub nodes: Vec<CutterGridNodeProfileV4>,
    pub reference_program: CutterGridProgramV4,
    pub reference_trajectory_signature: String,
    pub certification: CutterGridCertificationV4,
    pub motion_limits: CutterGridMotionLimitsV4,
    pub motion_limits_signature: String,
    pub roadmap: CutterGridRoadmapV4,
    pub profile_signature: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CutterGridPlanningErrorCodeV4 {
    PlannerNotReady, PlanningCancelled, ProfileV4Mismatch, OutOfBounds,
    EndpointIkNotConverged, EndpointIkSearchExhausted, EndpointPtpDisconnected,
    MotionPrimitiveBudgetExhausted, PtpCollision, PtpCertificateFailed,
    ActualSweepCertificationFailed, PlanSignatureMismatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum CutterGridPlanningStageV4 {
    Profile, Endpoint, PtpEdge, Roadmap, MotionCertificate, SweepCertificate, Serialization,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPlanRequestV1 {
    pub challenge_id: String,
    pub challenge_version: u32,
    pub program: CutterGridProgramV4,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CutterGridPlanResponseV1 {
    pub kind: String,
    pub version: u32,
    pub planner_implementation: String,
    pub planner_build: String,
    pub profile_signature: String,
    pub planning_duration_ms: f64,
    pub plan: CutterTrajectoryPlanV4,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientPreview {
    pub score_result: ScoreResult,
    pub result_voxels_hash: String,
    pub engine_version: String,
    pub tick_ms: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum TerminalReason { Completed, HeadCollision, CommandLimit, Invalid, Timeout }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Terminal {
    pub reason: TerminalReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joint_id: Option<JointId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub safe_angle_deg: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_block_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub part_label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayInfo {
    pub engine_version: String,
    pub tick_ms: f64,
    pub simulated_ms: f64,
    pub diverged_from_client: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionResult {
    /// Which engine produced this score. Skipped when servo.
    #[serde(default, skip_serializing_if = "ProgrammingMode::is_default")]
    pub programming_mode: ProgrammingMode,
    pub submission_id: String,
    pub challenge_id: String,
    pub challenge_version: u32,
    pub status: SubmissionStatus,
    pub score: ScoreResult,
    pub metrics: ProgramMetrics,
    pub result_voxels_hash: String,
    pub terminal: Terminal,
    pub replay: ReplayInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<HcrError>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SubmissionStatus { Completed, Error }

// ---------------------------------------------------------------------------
// 5. Adaptive session (CAT)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    pub session_id: String,
    pub theta: f64,
    /// arona reports `StandardError::initial()` = infinity before the first response;
    /// serialize that as `null`, since JSON has no Infinity.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub standard_error: Option<f64>,
    pub response_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_remaining: Option<u32>,
    pub state: SessionState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum SessionState { Active, AwaitingResponse, Terminated, Finalized }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextItem {
    pub item_ref: String,
    pub challenge_id: String,
    pub challenge_version: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expected_remaining: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseOutcome {
    pub correct: bool,
    pub raw_score: f64,
    pub theta: f64,
    pub standard_error: f64,
    pub terminated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// 6. Device
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum AxisId { X, Y, Z, B, E, T }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxisConfig {
    pub axis: AxisId,
    /// `None` for hardware-only axes (the gripper `E` has no simulator counterpart).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub joint_id: Option<JointId>,
    pub min_deg: f64,
    pub max_deg: f64,
    pub home_deg: f64,
    pub center_deg: f64,
    pub direction: i8,     // +1 or -1
    pub offset_deg: f64,
    pub speed_deg_per_sec: f64,
}

impl AxisConfig {
    /// Simulator joint angle -> servo angle. The single place this conversion exists.
    pub fn to_servo_deg(&self, joint_deg: f64) -> f64 {
        let raw = self.center_deg + (self.direction as f64) * (joint_deg - self.offset_deg);
        raw.clamp(self.min_deg, self.max_deg)
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DeviceStatus { Offline, Idle, Running, Paused, Faulted, Estopped, Uncalibrated }

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum WireFormat { Json, Cbor, Text }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceState {
    pub status: DeviceStatus,
    pub firmware: String,
    pub wire_format: WireFormat,
    pub axes: Vec<AxisState>,
    pub queued_commands: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rssi: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fault: Option<HcrError>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AxisState { pub axis: AxisId, pub angle_deg: f64 }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceTelemetry {
    /// Device-monotonic ms since boot. Never a wall clock — the ESP8266 only has NTP
    /// when the router is reachable (`ESP8266.ino:506-513`).
    pub t_mono: u64,
    pub angles: Vec<f64>,
    pub busy: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum DeviceCommand {
    #[serde(rename_all = "camelCase")]
    Run { corr: String, program: Program },
    #[serde(rename_all = "camelCase")]
    Home { corr: String },
    #[serde(rename_all = "camelCase")]
    Stop { corr: String },
    #[serde(rename_all = "camelCase")]
    Resume { corr: String },
    #[serde(rename_all = "camelCase")]
    Query { corr: String },
    /// Pre-translated seller command language, for kit firmware that cannot parse IR.
    #[serde(rename_all = "camelCase")]
    Text { corr: String, text_cmd: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAck {
    pub corr: String,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<HcrError>,
}

// ---------------------------------------------------------------------------
// 9. Lesson telemetry (HTTP only)
// ---------------------------------------------------------------------------
//
// Client-asserted usage rows for the browser-side lessons. The server verifies
// none of it; the calibration datum remains `SubmissionCreate`.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LessonActivity {
    Read,
    Predict,
    Build,
    Observe,
    Challenge,
    Recap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LessonOutcome {
    Opened,
    SectionPassed,
    QuizPassed,
    Completed,
    Abandoned,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LessonEventCreate {
    pub lesson_id: String,
    pub section: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub activity: Option<LessonActivity>,
    pub outcome: LessonOutcome,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tests: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<ProgrammingMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LessonEventAck {
    pub recorded: bool,
}
