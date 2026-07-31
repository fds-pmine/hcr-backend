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
pub mod domain;
pub mod round;
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
pub use domain::{
    AllowedBlockType, Axis, ChallengeDefinition, HairstyleDefinition, JointConfig, JointId, Program,
    ProgramMetrics, ProgramNode, RobotCollisionConfig, RobotCommand, RobotConfig,
    RobotGeometryConfig, ScoreResult, ScoreWeights, ScoringConfig, Terminal, TerminalReason, Vec3,
    VoxelConfig, VoxelCoord,
};
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
