//! Service failures, and how they become wire errors.

use hcr_contract::{HcrError, HcrErrorCode};
use hcr_sim::SimError;

/// Everything the service layer can fail with.
#[derive(Debug, Clone, PartialEq)]
pub enum ServiceError {
    /// No such challenge, or no such version of it.
    ChallengeNotFound {
        /// Which challenge was requested.
        challenge_id: String,
        /// Which version, when a specific one was asked for.
        version: Option<u32>,
    },
    /// The program failed validation. `field` locates the offending block so the
    /// editor can highlight it.
    ProgramInvalid {
        /// Human-readable reason.
        message: String,
        /// Originating Blockly block, when known.
        field: Option<String>,
    },
    /// Expansion exceeded the atomic-command cap.
    ProgramTooLarge {
        /// The cap.
        limit: usize,
    },
    /// The challenge's scoring block is unusable.
    WeightsInvalid(&'static str),
    /// The item reference was forged, stale, or issued for something else.
    ItemRefInvalid(&'static str),
    /// No such session.
    SessionNotFound(String),
    /// The session has already terminated.
    SessionTerminated,
    /// The session is not expecting a response right now.
    SessionNotAwaitingResponse,
    /// The round has not reached the stage the request needs.
    MatchNotReady(&'static str),
    /// The bank could not supply an item.
    BankExhausted,
    /// Replay capacity is saturated.
    RateLimited,
    /// Replay exceeded its tick budget.
    ReplayTimeout,
    /// An invariant broke inside the service.
    Internal(&'static str),
}

impl ServiceError {
    /// Map to the wire error code.
    pub fn code(&self) -> HcrErrorCode {
        match self {
            ServiceError::ChallengeNotFound { .. } => HcrErrorCode::ChallengeNotFound,
            ServiceError::ProgramInvalid { .. } => HcrErrorCode::ProgramInvalid,
            ServiceError::ProgramTooLarge { .. } => HcrErrorCode::ProgramTooLarge,
            ServiceError::WeightsInvalid(_) => HcrErrorCode::WeightsInvalid,
            ServiceError::ItemRefInvalid(_) => HcrErrorCode::ItemRefInvalid,
            ServiceError::SessionNotFound(_) => HcrErrorCode::SessionNotFound,
            ServiceError::SessionTerminated | ServiceError::SessionNotAwaitingResponse => {
                HcrErrorCode::SessionTerminated
            }
            ServiceError::MatchNotReady(_) => HcrErrorCode::MatchNotReady,
            ServiceError::BankExhausted => HcrErrorCode::BankExhausted,
            ServiceError::RateLimited => HcrErrorCode::RateLimited,
            ServiceError::ReplayTimeout => HcrErrorCode::ReplayTimeout,
            ServiceError::Internal(_) => HcrErrorCode::Internal,
        }
    }

    /// Render as the contract's error shape.
    pub fn to_wire(&self) -> HcrError {
        let error = HcrError::new(self.code(), self.to_string());
        match self {
            ServiceError::ProgramInvalid {
                field: Some(field), ..
            } => error.with_field(field.clone()),
            _ => error,
        }
    }
}

impl std::fmt::Display for ServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ServiceError::ChallengeNotFound {
                challenge_id,
                version,
            } => match version {
                Some(v) => write!(f, "Challenge \"{challenge_id}\" version {v} was not found."),
                None => write!(f, "Challenge \"{challenge_id}\" was not found."),
            },
            ServiceError::ProgramInvalid { message, .. } => write!(f, "{message}"),
            ServiceError::ProgramTooLarge { limit } => {
                write!(f, "The expanded program exceeds {limit} atomic commands.")
            }
            ServiceError::WeightsInvalid(reason) => write!(f, "{reason}"),
            ServiceError::ItemRefInvalid(reason) => write!(f, "Invalid item reference: {reason}."),
            ServiceError::SessionNotFound(id) => write!(f, "Session \"{id}\" was not found."),
            ServiceError::SessionTerminated => write!(f, "The session has already terminated."),
            ServiceError::SessionNotAwaitingResponse => {
                write!(f, "The session is not awaiting a response.")
            }
            // Shown to a waiting player, so it says what to do, not what broke.
            ServiceError::MatchNotReady(reason) => write!(f, "{reason}"),
            ServiceError::BankExhausted => {
                write!(f, "The question bank has no suitable item available.")
            }
            ServiceError::RateLimited => {
                write!(f, "Replay capacity is saturated; retry shortly.")
            }
            ServiceError::ReplayTimeout => write!(f, "Replay exceeded its budget."),
            ServiceError::Internal(reason) => write!(f, "Internal service error: {reason}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl From<SimError> for ServiceError {
    /// Translate engine failures into client-facing ones.
    ///
    /// Note what is *absent*: a head collision is not an error here. It is a
    /// legitimate terminal state that still produces a score, matching the
    /// frontend, which stops at the last safe pose rather than silently
    /// correcting.
    fn from(error: SimError) -> Self {
        match error {
            SimError::CommandLimitExceeded { limit, .. } => {
                ServiceError::ProgramTooLarge { limit }
            }
            SimError::EmptyProgram => ServiceError::ProgramInvalid {
                message: "The program does not contain an executable command.".to_string(),
                field: None,
            },
            SimError::UnknownJoint { ref joint_id } => ServiceError::ProgramInvalid {
                message: error.to_string(),
                field: Some(joint_id.clone()),
            },
            SimError::AngleOutOfRange { ref joint_id, .. } => ServiceError::ProgramInvalid {
                message: error.to_string(),
                field: Some(joint_id.clone()),
            },
            SimError::InvalidWait { .. } | SimError::MissingJoint { .. } => {
                ServiceError::ProgramInvalid {
                    message: error.to_string(),
                    field: None,
                }
            }
            SimError::InvalidScoring(reason) => ServiceError::WeightsInvalid(reason),
            SimError::BudgetExhausted { .. } => ServiceError::ReplayTimeout,
            SimError::Internal(reason) => ServiceError::Internal(reason),
        }
    }
}

/// Convenience alias.
pub type ServiceResult<T> = Result<T, ServiceError>;
