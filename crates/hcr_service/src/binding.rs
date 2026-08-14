//! HTTP binding.
//!
//! A transport-neutral router: bytes and a path in, status and bytes out. It
//! performs no IO, so it can sit behind hotaru's `Endpoint::<HTTP>`, behind any
//! other server, or in a test with no runtime at all.
//!
//! Routes are those reserved in `docs/HCR_Simulator_SPEC_v0.3.md:528-531` and
//! elaborated in `docs/backend/01-CONTRACT.md` §3.2.
//!
//! # Wiring it to hotaru
//!
//! `hotaru/examples/starter_manual` shows the shape: build the app, construct
//! `Endpoint::<HTTP>::endpoint(path, name, handler)` and `insert` it. The handler
//! becomes a two-line adapter — read method/path/body off the `HttpContext`, call
//! [`Router::dispatch`], write [`HttpReply::status`] and [`HttpReply::body`] back.
//! Everything that can be got wrong (routing, codecs, status mapping) lives here,
//! where it is tested.

use std::sync::Arc;

use hcr_contract::api::ScoreInput;
use hcr_contract::{
    HcrErrorCode, MatchConfig, SessionRespond, SessionStart, SubmissionCreate,
};
use serde::Serialize;

use crate::error::{ServiceError, ServiceResult};
use crate::service::HcrService;

/// The verbs this binding uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Safe, idempotent reads.
    Get,
    /// Everything else.
    Post,
}

/// An inbound request, already stripped of transport concerns.
#[derive(Debug, Clone)]
pub struct HttpCall {
    /// Verb.
    pub method: Method,
    /// Path, including the `/api/v1` prefix. Query strings are not used.
    pub path: String,
    /// Raw body.
    pub body: Vec<u8>,
    /// Player identity, supplied by the authentication layer.
    ///
    /// Never read from the body: a client that could name itself could submit as
    /// somebody else.
    pub player_id: Option<String>,
    /// Display name for the roster and the leaderboard.
    ///
    /// Cosmetic, and separate from [`Self::player_id`] on purpose. Identity
    /// decides what a caller may *do* and must come from the auth layer; a label
    /// decides only what other players read, so letting the client choose it
    /// costs nothing. Falls back to the player id when absent.
    pub display_name: Option<String>,
}

impl HttpCall {
    /// A GET.
    pub fn get(path: impl Into<String>) -> Self {
        Self {
            method: Method::Get,
            path: path.into(),
            body: Vec::new(),
            player_id: None,
            display_name: None,
        }
    }

    /// A POST carrying JSON.
    pub fn post(path: impl Into<String>, body: impl Serialize) -> Self {
        Self {
            method: Method::Post,
            path: path.into(),
            body: serde_json::to_vec(&body).unwrap_or_default(),
            player_id: None,
            display_name: None,
        }
    }

    /// Attach the authenticated player.
    pub fn as_player(mut self, player_id: impl Into<String>) -> Self {
        self.player_id = Some(player_id.into());
        self
    }

    /// Attach the authenticated player together with a display name.
    pub fn as_player_named(
        mut self,
        player_id: impl Into<String>,
        display_name: impl Into<String>,
    ) -> Self {
        self.player_id = Some(player_id.into());
        self.display_name = Some(display_name.into());
        self
    }
}

/// The reply, ready for whatever transport is carrying it.
#[derive(Debug, Clone)]
pub struct HttpReply {
    /// HTTP status.
    pub status: u16,
    /// JSON body.
    pub body: Vec<u8>,
}

impl HttpReply {
    /// Body decoded as UTF-8, for logging and tests.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Decode the body as `T`.
    pub fn json<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_slice(&self.body).ok()
    }

    fn ok(value: &impl Serialize) -> Self {
        match serde_json::to_vec(value) {
            Ok(body) => Self { status: 200, body },
            Err(_) => Self::from_error(&ServiceError::Internal("failed to encode response")),
        }
    }

    fn from_error(error: &ServiceError) -> Self {
        let status = status_for(error.code());
        let body = serde_json::to_vec(&serde_json::json!({ "error": error.to_wire() }))
            .unwrap_or_else(|_| b"{\"error\":{\"code\":\"INTERNAL\"}}".to_vec());
        Self { status, body }
    }
}

/// Map a wire error code to an HTTP status, per `01-CONTRACT.md` §7.
pub fn status_for(code: HcrErrorCode) -> u16 {
    match code {
        HcrErrorCode::Unauthorized => 401,
        HcrErrorCode::Forbidden => 403,
        HcrErrorCode::ChallengeNotFound | HcrErrorCode::SessionNotFound => 404,
        // 422 alongside the other content failures: the request is well-formed
        // JSON describing something the server will not accept, and no amount of
        // retrying the identical bytes changes that.
        HcrErrorCode::ProgramInvalid
        | HcrErrorCode::ProgramTooLarge
        | HcrErrorCode::WeightsInvalid
        | HcrErrorCode::TrajectoryRejected => 422,
        HcrErrorCode::ItemRefInvalid
        | HcrErrorCode::SessionTerminated
        | HcrErrorCode::MatchNotReady
        | HcrErrorCode::BankExhausted
        | HcrErrorCode::DeviceOffline
        | HcrErrorCode::DeviceBusy => 409,
        HcrErrorCode::RateLimited => 429,
        HcrErrorCode::ReplayTimeout => 504,
        HcrErrorCode::Internal => 500,
    }
}

/// Routes requests to the service.
#[derive(Debug, Clone)]
pub struct Router {
    service: Arc<HcrService>,
}

impl Router {
    /// Wrap a service.
    pub fn new(service: Arc<HcrService>) -> Self {
        Self { service }
    }

    /// Handle one request.
    pub async fn dispatch(&self, call: HttpCall) -> HttpReply {
        match self.route(&call).await {
            Ok(reply) => reply,
            Err(error) => HttpReply::from_error(&error),
        }
    }

    async fn route(&self, call: &HttpCall) -> ServiceResult<HttpReply> {
        let segments: Vec<&str> = call
            .path
            .trim_matches('/')
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        // Everything lives under /api/v1.
        let rest = match segments.as_slice() {
            ["api", "v1", rest @ ..] => rest,
            _ => return Err(not_found()),
        };

        let service = &self.service;
        match (call.method, rest) {
            // -- catalog --
            (Method::Get, ["challenges"]) => Ok(HttpReply::ok(&service.list_challenges()?)),
            (Method::Get, ["challenges", id]) => {
                let dto = service.get_challenge(id, None)?;
                Ok(HttpReply::ok(dto.as_ref()))
            }
            (Method::Get, ["challenges", id, version]) => {
                let version = version.parse().map_err(|_| not_found())?;
                let dto = service.get_challenge(id, Some(version))?;
                Ok(HttpReply::ok(dto.as_ref()))
            }

            // -- direct scoring, for v1 ScoreProvider parity --
            (Method::Post, ["score"]) => {
                let input: ScoreInput = decode(&call.body)?;
                Ok(HttpReply::ok(&service.score(&input)?))
            }

            // -- submissions --
            (Method::Post, ["submissions"]) => {
                let request: SubmissionCreate = decode(&call.body)?;
                // The player is passed for the usage log only. It grants
                // nothing: solo scoring is the same for every caller, and on a
                // public deployment this value is whatever the client claimed.
                Ok(HttpReply::ok(
                    &service
                        .create_submission_for(request, call.player_id.as_deref())
                        .await?,
                ))
            }
            (Method::Get, ["submissions", id]) => Ok(HttpReply::ok(&service.get_submission(id)?)),

            // -- adaptive sessions --
            (Method::Post, ["sessions"]) => {
                let request: SessionStart = decode_or_default(&call.body)?;
                Ok(HttpReply::ok(&service.start_session(request).await?))
            }
            (Method::Get, ["sessions", id]) => {
                Ok(HttpReply::ok(&service.session_snapshot(id).await?))
            }
            (Method::Post, ["sessions", id, "next"]) => {
                Ok(HttpReply::ok(&service.next_item(id).await?))
            }
            (Method::Post, ["sessions", id, "responses"]) => {
                let mut request: SessionRespond = decode(&call.body)?;
                // The path is authoritative for identity; a body that disagreed
                // would otherwise let a caller act on another session.
                request.session_id = (*id).to_string();
                Ok(HttpReply::ok(&service.respond(request).await?))
            }
            (Method::Post, ["sessions", id, "finalize"]) => {
                Ok(HttpReply::ok(&service.finalize_session(id).await?))
            }

            // -- competitive rounds --
            (Method::Get, ["time"]) => Ok(HttpReply::ok(&service.time_sync(0))),
            (Method::Post, ["matches"]) => {
                let config: MatchConfig = decode_or_default(&call.body)?;
                Ok(HttpReply::ok(&service.create_match(config)?))
            }
            (Method::Get, ["matches", id]) => Ok(HttpReply::ok(&service.match_state(id)?)),
            (Method::Get, ["matches", id, "challenge"]) => {
                let dto = service.match_challenge(id)?;
                Ok(HttpReply::ok(dto.as_ref()))
            }
            (Method::Get, ["matches", id, "results"]) => {
                Ok(HttpReply::ok(&service.match_results(id)?))
            }
            (Method::Post, ["matches", id, "join"]) => {
                let player = call.player_id.as_deref().ok_or(ServiceError::Internal(
                    "join requires an authenticated player",
                ))?;
                let display_name = call.display_name.as_deref().unwrap_or(player);
                Ok(HttpReply::ok(&service.join_match(id, player, display_name)?))
            }
            (Method::Post, ["matches", id, "start"]) => {
                Ok(HttpReply::ok(&service.start_match(id)?))
            }
            (Method::Post, ["matches", id, "submissions"]) => {
                let player = call.player_id.as_deref().ok_or(ServiceError::Internal(
                    "submitting requires an authenticated player",
                ))?;
                let request: SubmissionCreate = decode(&call.body)?;
                Ok(HttpReply::ok(
                    &service.submit_to_match(id, player, request).await?,
                ))
            }

            _ => Err(not_found()),
        }
    }
}

fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> ServiceResult<T> {
    serde_json::from_slice(body).map_err(|error| ServiceError::ProgramInvalid {
        message: format!("Malformed request body: {error}"),
        field: None,
    })
}

fn decode_or_default<T: serde::de::DeserializeOwned + Default>(body: &[u8]) -> ServiceResult<T> {
    if body.is_empty() {
        return Ok(T::default());
    }
    decode(body)
}

fn not_found() -> ServiceError {
    ServiceError::ChallengeNotFound {
        challenge_id: "route".to_string(),
        version: None,
    }
}
