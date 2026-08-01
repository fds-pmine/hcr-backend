//! Transport-neutral envelope and error model.
//!
//! Every MQTT payload is one [`Envelope`]; nothing is ever published bare. The
//! HTTP binding carries the same metadata in headers and the payload as the body.
//! See `docs/backend/01-CONTRACT.md` §2.

use alloc::{collections::BTreeMap, string::String};
use serde::{Deserialize, Serialize};

/// Major protocol version, present in both the topic tree and every envelope.
pub const PROTOCOL_VERSION: u8 = 1;

/// Who sent a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ActorType {
    /// A human using a browser.
    User,
    /// A robot arm.
    Device,
    /// The backend itself.
    Service,
}

/// Identity of a message sender.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActorRef {
    /// Kind of actor.
    #[serde(rename = "type")]
    pub actor_type: ActorType,
    /// Actor identifier within its kind.
    pub id: String,
}

/// The universal message wrapper.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope<P> {
    /// Protocol major version. Always [`PROTOCOL_VERSION`].
    pub v: u8,
    /// ULID, unique per message. Doubles as the idempotency key.
    pub id: String,
    /// Message discriminator, e.g. `"session.next.req"`.
    pub kind: String,
    /// Sender epoch-ms. Informational only — **never** an ordering key, because
    /// device clocks are unreliable and browser clocks are attacker-controlled.
    pub ts: u64,
    /// `id` of the request this answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub corr: Option<String>,
    /// Topic the responder must publish the reply to. The backend must verify it
    /// lies inside the caller's own subtree before honouring it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Sender identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub src: Option<ActorRef>,
    /// The message body.
    pub payload: P,
}

impl<P> Envelope<P> {
    /// Build an envelope at the current protocol version.
    pub fn new(id: String, kind: String, ts: u64, payload: P) -> Self {
        Self {
            v: PROTOCOL_VERSION,
            id,
            kind,
            ts,
            corr: None,
            reply_to: None,
            src: None,
            payload,
        }
    }

    /// Mark this envelope as answering `request_id`.
    pub fn correlate(mut self, request_id: String) -> Self {
        self.corr = Some(request_id);
        self
    }
}

/// Stable machine-readable failure codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HcrErrorCode {
    /// Missing or invalid credentials.
    Unauthorized,
    /// Authenticated but not permitted.
    Forbidden,
    /// No such challenge, or no such version.
    ChallengeNotFound,
    /// Program failed validation; `field` locates the offending block.
    ProgramInvalid,
    /// Expansion exceeded the atomic-command cap.
    ProgramTooLarge,
    /// Score weights did not sum to 1, or a reference value was non-positive.
    WeightsInvalid,
    /// `itemRef` was forged, stale, or issued for a different session.
    ItemRefInvalid,
    /// No such session.
    SessionNotFound,
    /// Session already terminated.
    SessionTerminated,
    /// The round has not reached the stage the request needs.
    ///
    /// Asking for the challenge during the lobby, or for results while the round
    /// is still running. Both are refusals by design rather than faults — the
    /// caller should try again later, not differently — and neither is a session
    /// that terminated or a reference that was forged, which is what they had to
    /// borrow a code from before this existed.
    MatchNotReady,
    /// The question bank could not supply an item.
    BankExhausted,
    /// Device is not connected.
    DeviceOffline,
    /// Device is executing something else.
    DeviceBusy,
    /// Replay exceeded its budget.
    ReplayTimeout,
    /// Caller is being throttled.
    RateLimited,
    /// Unexpected server fault.
    Internal,
}

impl HcrErrorCode {
    /// Whether retrying the same request could plausibly succeed.
    pub fn retryable(self) -> bool {
        matches!(
            self,
            HcrErrorCode::DeviceOffline
                | HcrErrorCode::DeviceBusy
                | HcrErrorCode::ReplayTimeout
                | HcrErrorCode::RateLimited
                | HcrErrorCode::Internal
        )
    }
}

/// The single error shape used by both bindings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HcrError {
    /// Stable code.
    pub code: HcrErrorCode,
    /// English, human-readable explanation.
    pub message: String,
    /// Whether a retry could succeed.
    pub retryable: bool,
    /// Field path for validation errors. Lets the editor highlight a block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub field: Option<String>,
    /// Additional context.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<BTreeMap<String, String>>,
}

impl HcrError {
    /// Build an error, deriving `retryable` from the code.
    pub fn new(code: HcrErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: code.retryable(),
            field: None,
            details: None,
        }
    }

    /// Attach the field path that failed validation.
    pub fn with_field(mut self, field: impl Into<String>) -> Self {
        self.field = Some(field.into());
        self
    }
}
