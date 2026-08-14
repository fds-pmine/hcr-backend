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
    /// A Cutter Grid trajectory failed verification and the whole submission was
    /// refused.
    ///
    /// Cutter Grid rejects rather than halting: the frontend will not run a
    /// program whose plan fails these checks, so a server that scored a partial
    /// run would be scoring something no client would ever produce
    /// (`docs/backend/08-CUTTER-GRID.md` §4).
    CutterPlanRejected {
        /// Which audit failed.
        rejection: CutterRejection,
        /// Blockly block to highlight, when the failure can be attributed to one.
        /// Absent for whole-plan failures like a signature mismatch.
        source_block_id: Option<String>,
        /// Operator-facing specifics — measured values, expected values.
        detail: String,
    },
    /// Internal invariant violated; indicates a bug in the engine itself.
    Internal(&'static str),
}

/// Why a Cutter Grid trajectory was refused.
///
/// Each variant is a distinct claim the client made that did not survive being
/// re-derived from the joint angles. They map onto stable wire error codes in
/// `hcr_service`, so a frontend can react to the kind without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutterRejection {
    /// Not a V2 ladder plan — wrong `kind`, `version` or planner build.
    UnsupportedPlanVersion,
    /// Planned against a different challenge than the one being scored.
    SignatureMismatch,
    /// Steps do not correspond to the program's own expansion.
    StepMismatch,
    /// The lattice path skips, repeats or teleports between cells.
    CoordDiscontinuity,
    /// A pose exceeds a joint's configured travel.
    JointLimit,
    /// A pose puts part of the arm inside the head.
    HeadCollision,
    /// A step does not open from the pose the previous one closed in.
    PoseDiscontinuity,
    /// The declared tool tip is not where the joint angles put it.
    EndEffectorMismatch,
    /// A single-cell move does not travel one cell along the axis it claims.
    AxisDisplacement,
    /// A waypoint wanders too far from its step's straight path.
    PathDeviation,
    /// Waypoint timestamps are absent, out of order, or disagree with the
    /// step's declared duration.
    TimelineInvalid,
    /// The entry motion removes hair before the program starts.
    EntryCutsHair,
    /// The plan carries more waypoints than the verifier will process.
    TooManyWaypoints,
}

impl CutterRejection {
    /// Stable identifier, used to build the wire error code.
    pub fn as_str(self) -> &'static str {
        match self {
            CutterRejection::UnsupportedPlanVersion => "UNSUPPORTED_PLAN_VERSION",
            CutterRejection::SignatureMismatch => "SIGNATURE_MISMATCH",
            CutterRejection::StepMismatch => "STEP_MISMATCH",
            CutterRejection::CoordDiscontinuity => "COORD_DISCONTINUITY",
            CutterRejection::JointLimit => "JOINT_LIMIT",
            CutterRejection::HeadCollision => "HEAD_COLLISION",
            CutterRejection::PoseDiscontinuity => "POSE_DISCONTINUITY",
            CutterRejection::EndEffectorMismatch => "END_EFFECTOR_MISMATCH",
            CutterRejection::AxisDisplacement => "AXIS_DISPLACEMENT",
            CutterRejection::PathDeviation => "PATH_DEVIATION",
            CutterRejection::TimelineInvalid => "TIMELINE_INVALID",
            CutterRejection::EntryCutsHair => "ENTRY_CUTS_HAIR",
            CutterRejection::TooManyWaypoints => "TOO_MANY_WAYPOINTS",
        }
    }

    /// One sentence a waiting player could be shown.
    pub fn message(self) -> &'static str {
        match self {
            CutterRejection::UnsupportedPlanVersion => {
                "This program was planned by an older build. Reload and try again."
            }
            CutterRejection::SignatureMismatch => {
                "This challenge changed since the program was planned. Reload and try again."
            }
            CutterRejection::EntryCutsHair => {
                "The approach to the starting cell would cut hair before the program begins."
            }
            CutterRejection::HeadCollision => "The planned path would touch the head.",
            CutterRejection::JointLimit => "The planned path needs an angle the arm cannot reach.",
            CutterRejection::TooManyWaypoints => "This program is too large to verify.",
            _ => "The planned trajectory did not pass verification.",
        }
    }
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
            SimError::CutterPlanRejected {
                rejection, detail, ..
            } => write!(
                f,
                "Cutter Grid trajectory rejected ({}): {detail}",
                rejection.as_str()
            ),
            SimError::BudgetExhausted { ticks } => {
                write!(f, "Replay exceeded its budget after {ticks} ticks.")
            }
            SimError::Internal(reason) => write!(f, "Internal simulation error: {reason}"),
        }
    }
}

#[cfg(feature = "std")]
impl std::error::Error for SimError {}
