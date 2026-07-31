//! HCR backend service.
//!
//! Catalog, authoritative replay and adaptive sessions, with **no transport**.
//!
//! # Why transport-agnostic
//!
//! [`HcrService`] takes typed requests and returns typed responses. It opens no
//! sockets and knows nothing about MQTT, HTTP or WebSocket framing. The bindings
//! described in `docs/backend/01-CONTRACT.md` are thin adapters over this: decode
//! a request, call a method, encode the reply.
//!
//! That split is what lets the whole surface — idempotency, item-reference
//! forgery, replay backpressure, session ordering — be tested without a broker,
//! and it keeps the service insulated from churn in the MQTT layer.
//!
//! # The ordering that matters
//!
//! `QuestionContent::score` is synchronous and infallible
//! (`arona/src/content/traits.rs:134`), but scoring an HCR response means
//! replaying a program. So the sequence is fixed:
//!
//! 1. [`HcrService::create_submission`] replays and stores the authoritative score.
//! 2. [`HcrService::respond`] looks that score up and hands it to the session.
//! 3. The session records it in the [`hcr_qbank::OutcomeStore`] *before* calling
//!    `submit_response`, so arona finds it waiting.
//!
//! Step 2 reads the stored score rather than accepting one from the caller. A
//! client that could supply its own score would make server-side replay
//! decorative.

#![forbid(unsafe_code)]

pub mod binding;
#[cfg(feature = "hotaru")]
pub mod hotaru_binding;
pub mod catalog;
pub mod clock;
pub mod error;
pub mod itemref;
pub mod replay;
pub mod rounds;
pub mod service;
pub mod session;

pub use binding::{HttpCall, HttpReply, Method, Router, status_for};
pub use catalog::CatalogStore;
pub use clock::{Clock, ManualClock, SharedClock, SystemClock, system_clock};
pub use rounds::MatchRegistry;
pub use error::{ServiceError, ServiceResult};
pub use itemref::{ItemRefClaims, ItemRefSigner};
pub use replay::{ENGINE_VERSION, ReplayPool, diverged, program_hash};
pub use service::{HcrService, ServiceConfig};
pub use session::{SessionHandle, SessionRegistry, SessionSpec};
