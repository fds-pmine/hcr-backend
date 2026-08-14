//! Deterministic HCR simulation core.
//!
//! A faithful Rust port of the frontend engine in `src/features/**`, shared by
//! two of the three executors in the system: server-side authoritative replay and
//! (as `no_std + alloc`) device firmware. The browser keeps its own TypeScript
//! copy, which remains the incumbent definition of correct — conformance vectors
//! are generated from it and this crate must match.
//!
//! Determinism properties this crate guarantees:
//!
//! * Replay advances by a fixed [`hcr_contract::SIM_TICK_MS`], never a wall clock,
//!   so a run is a pure function of `(challenge, program)`.
//! * Voxel sets are `BTreeSet`, so iteration order is specified rather than
//!   hash-seed dependent.
//! * `sin`/`cos` come from `libm` in every build, std or not, so the server and
//!   the firmware agree bit-for-bit with each other. Agreement with JavaScript is
//!   to within a few ULP — IEEE-754 does not require correctly-rounded
//!   transcendentals — which is why divergence is measured by Jaccard distance
//!   rather than hash equality (`docs/backend/02-DETERMINISM.md` §4).

#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]

extern crate alloc;

pub mod collision;
pub mod controller;
pub mod cutter;
pub mod engine;
pub mod error;
pub mod executor;
pub mod kinematics;
pub mod program;
pub mod scoring;
pub mod state;
pub mod voxel;

pub use collision::{HeadCollision, RobotCollisionPart, find_robot_head_collision};
pub use controller::{
    BlockedHeadCollision, COLLISION_BISECTION_STEPS, MAX_ANGULAR_STEP_DEG, MoveAdvanceResult,
    RobotController,
};
pub use cutter::{
    CutterDivergence, CutterReplayOptions, CutterReplayOutcome, expand_cutter_program,
    verify_and_replay,
};
pub use engine::{ReplayOptions, ReplayOutcome, replay};
pub use error::{CutterRejection, SimError};
pub use executor::{ExecutorAdvanceResult, Movement, ProgramExecutor};
pub use kinematics::{RobotPose, compute_robot_pose};
pub use program::{estimate_program_duration, expand_program, expand_program_default};
pub use scoring::{calculate_score, validate_scoring_config};
pub use state::JointAngles;
pub use voxel::{
    VoxelSet, calculate_trim_score, coord_to_key, find_swept_voxel_hits, key_to_coord,
    result_voxels_hash, segment_intersects_aabb, voxel_coord_to_world,
};
