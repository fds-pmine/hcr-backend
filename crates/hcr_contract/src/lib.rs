//! `hcr.v1` — the HCR Simulator wire contract.
//!
//! One definition of the wire, shared by the backend service, the replay engine
//! and (as `no_std + alloc`) device firmware. The normative schema this mirrors is
//! `docs/backend/schema/hcr-v1.d.ts`; when the two disagree, the TypeScript file
//! is the contract and this is a bug.
//!
//! Naming: the wire is `camelCase` everywhere, so every type carries
//! `#[serde(rename_all = "camelCase")]` and the TypeScript definitions are literal.

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod api;
pub mod catalog;
pub mod cutter;
pub mod domain;
pub mod round;
pub mod signature;
pub mod wire;

pub use round::{
    MatchChallengeRef, MatchConfig, MatchPhase, MatchPlayer, MatchRejection, MatchResultRow,
    MatchResults, MatchState, MatchSubmissionAck, RankBy, TimeSync,
};

pub use api::{
    ChallengeSummary, ClientPreview, NextItem, ReplayInfo, ResponseOutcome, SessionItemRecord,
    SessionLifecycle, SessionRespond, SessionResultDto, SessionSnapshot, SessionStart,
    SubmissionCreate, SubmissionResult, SubmissionStatus,
};
pub use catalog::{
    CalibrationState, ChallengeDefinitionDto, ChallengeMeta, GeneratorProvenance, ItemId,
    ItemParameters, SkillDimension,
};
pub use cutter::{
    CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION, CUTTER_GRID_LADDER_PLANNER_VERSION,
    CUTTER_GRID_PLAN_API_VERSION, CUTTER_TRAJECTORY_PLAN_V4_VERSION,
    CUTTER_TRAJECTORY_PLAN_VERSION, CutterGridAction, CutterGridBoundsV4,
    CutterGridCertificationV4, CutterGridContactEventV4, CutterGridCoord, CutterGridDirection,
    CutterGridEntryOptionV4, CutterGridJointMotionLimitsV4, CutterGridMotionLimitsV4,
    CutterGridNode, CutterGridNodeProfileV4, CutterGridPlanRequestV1, CutterGridPlanResponseV1,
    CutterGridPlanningDiagnostics, CutterGridPlanningDiagnosticsV4, CutterGridPlanningErrorCodeV4,
    CutterGridPlanningStageV4, CutterGridPositioningPlanV4, CutterGridProfileV4, CutterGridProgram,
    CutterGridProgramV4, CutterGridRoadmapEdgeV4, CutterGridRoadmapNodeV4, CutterGridRoadmapV4,
    CutterGridStaticIkStatusV4, CutterGridSubmission, CutterGridSyncPtpPrimitiveV4,
    CutterGridTrajectoryActionV4, CutterTrajectoryBoundaryStateV4, CutterTrajectoryPlan,
    CutterTrajectoryPlanV4, CutterTrajectoryStep, CutterTrajectoryStepKind,
    CutterTrajectoryWaypoint, ProgrammingMode,
};
pub use domain::{
    AllowedBlockType, Axis, ChallengeDefinition, HairstyleDefinition, JointConfig, JointId, Program,
    ProgramMetrics, ProgramNode, RobotCollisionConfig, RobotCommand, RobotConfig,
    RobotGeometryConfig, ScoreResult, ScoreWeights, ScoringConfig, ServoAxisId, ServoMapping,
    Terminal, TerminalReason, Vec3, VoxelConfig, VoxelCoord,
};
pub use signature::{cutter_grid_challenge_signature_v2, fnv1a64};
pub use wire::{ActorRef, ActorType, Envelope, HcrError, HcrErrorCode, PROTOCOL_VERSION};

/// Canonical simulation tick, milliseconds.
///
/// Replay always advances by exactly this amount so a run is a pure function of
/// `(challenge, program)`. See `docs/backend/02-DETERMINISM.md`.
pub const SIM_TICK_MS: f64 = 5.0;

/// Maximum atomic commands an expanded program may contain.
///
/// Mirrors `MAX_RUNTIME_COMMANDS` in `src/features/blockly/programCompiler.ts:12`.
/// The server enforces this itself rather than trusting the client's expansion.
pub const MAX_RUNTIME_COMMANDS: usize = 500;
