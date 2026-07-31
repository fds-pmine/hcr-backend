//! Failures the simulation core can report.

use alloc::string::String;
use core::fmt;

/// Everything that can go wrong while validating or replaying a program.
#[derive(Debug, Clone, PartialEq)]
pub enum SimError {
    /// A joint required by the kinematic chain has no angle, or a non-finite one.
    MissingJoint {
        /// The joint that was missing.
        joint_id: String,
    },
    /// A command referenced a joint the challenge does not define.
    UnknownJoint {
        /// The joint that was referenced.
        joint_id: String,
    },
    /// A commanded angle fell outside the joint's configured travel.
    AngleOutOfRange {
        /// The joint being driven.
        joint_id: String,
        /// The angle that was requested.
        angle_deg: f64,
        /// Lower limit.
        min_angle_deg: f64,
        /// Upper limit.
        max_angle_deg: f64,
    },
    /// A wait duration was negative or non-finite.
    InvalidWait {
        /// The duration that was requested.
        duration_ms: f64,
    },
    /// Expansion produced more atomic commands than the cap allows.
    CommandLimitExceeded {
        /// The cap that was exceeded.
        limit: usize,
        /// Block that pushed it over, for editor attribution.
        source_block_id: String,
    },
    /// The program contained no executable commands.
    EmptyProgram,
    /// The challenge's scoring block is not usable.
    InvalidScoring(&'static str),
    /// The replay exceeded its tick budget — a guard against pathological input.
    BudgetExhausted {
        /// Ticks executed before giving up.
        ticks: u64,
    },
    /// Internal invariant violated; indicates a bug in the engine itself.
    Internal(&'static str),
}

impl fmt::Display for SimError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SimError::MissingJoint { joint_id } => {
                write!(f, "Missing or invalid angle for joint \"{joint_id}\".")
            }
            SimError::UnknownJoint { joint_id } => write!(f, "Unknown joint \"{joint_id}\"."),
            SimError::AngleOutOfRange {
                joint_id,
                angle_deg,
                ..
            } => write!(
                f,
                "Angle {angle_deg} is outside the range for \"{joint_id}\"."
            ),
            SimError::InvalidWait { .. } => {
                write!(f, "Wait duration must be a finite non-negative number.")
            }
            SimError::CommandLimitExceeded { limit, .. } => {
                write!(f, "The expanded program exceeds {limit} atomic commands.")
            }
            SimError::EmptyProgram => {
                write!(f, "The program does not contain an executable command.")
            }
            SimError::InvalidScoring(reason) => write!(f, "{reason}"),
            SimError::BudgetExhausted { ticks } => {
                write!(f, "Replay exceeded its budget after {ticks} ticks.")
            }
            SimError::Internal(reason) => write!(f, "Internal simulation error: {reason}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SimError {}
