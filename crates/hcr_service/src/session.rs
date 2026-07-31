//! Adaptive sessions, one actor apiece.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use arona::qbank::QBankError;
use arona::session::SessionError;
use arona::Session;
use hcr_contract::{
    NextItem, ResponseOutcome, SessionItemRecord, SessionLifecycle, SessionResultDto,
    SessionSnapshot,
};
use hcr_qbank::{
    HcrDynamicBank, OutcomeStore, SessionConfig, SharedServedLog, build_session, raw_from_remapped,
};
use tokio::sync::{RwLock, mpsc, oneshot};

use crate::error::{ServiceError, ServiceResult};
use crate::itemref::{ItemRefClaims, ItemRefSigner};

/// Mailbox depth. Sessions are driven by a human at human pace, so anything
/// beyond a handful of queued commands means something has gone wrong.
const MAILBOX_DEPTH: usize = 8;

enum Command {
    Next(oneshot::Sender<ServiceResult<NextItem>>),
    Respond {
        claims: Box<ItemRefClaims>,
        submission_id: String,
        raw_score: f64,
        reply: oneshot::Sender<ServiceResult<ResponseOutcome>>,
    },
    Snapshot(oneshot::Sender<ServiceResult<SessionSnapshot>>),
    Finalize(oneshot::Sender<ServiceResult<SessionResultDto>>),
}

/// How a session is created.
#[derive(Debug, Clone)]
pub struct SessionSpec {
    /// Session identity.
    pub session_id: String,
    /// Starting ability estimate.
    pub initial_theta: f64,
    /// Adaptive settings.
    pub config: SessionConfig,
    /// Seed for the bank's selection RNG, so the session is reproducible.
    pub seed: u64,
}

/// A live session.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    session_id: String,
    tx: mpsc::Sender<Command>,
}

impl std::fmt::Debug for Command {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SessionCommand")
    }
}

impl SessionHandle {
    /// Session identity.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    async fn call<T>(
        &self,
        make: impl FnOnce(oneshot::Sender<ServiceResult<T>>) -> Command,
    ) -> ServiceResult<T> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .await
            .map_err(|_| ServiceError::SessionNotFound(self.session_id.clone()))?;
        rx.await
            .map_err(|_| ServiceError::Internal("session actor dropped the reply"))?
    }

    /// Serve the next item.
    pub async fn next_item(&self) -> ServiceResult<NextItem> {
        self.call(Command::Next).await
    }

    /// Record a scored response.
    pub async fn respond(
        &self,
        claims: ItemRefClaims,
        submission_id: String,
        raw_score: f64,
    ) -> ServiceResult<ResponseOutcome> {
        self.call(|reply| Command::Respond {
            claims: Box::new(claims),
            submission_id,
            raw_score,
            reply,
        })
        .await
    }

    /// Current state.
    pub async fn snapshot(&self) -> ServiceResult<SessionSnapshot> {
        self.call(Command::Snapshot).await
    }

    /// Close the session and issue its result.
    pub async fn finalize(&self) -> ServiceResult<SessionResultDto> {
        self.call(Command::Finalize).await
    }
}

struct Actor {
    session_id: String,
    /// `Option` because `Session::finalize` consumes it.
    session: Option<Session>,
    outcomes: OutcomeStore,
    served: SharedServedLog,
    signer: ItemRefSigner,
    /// The item issued and not yet answered, with the token minted for it.
    awaiting: Option<(ItemRefClaims, NextItem)>,
    history: Vec<SessionItemRecord>,
    started_at: Instant,
    finalized: bool,
}

impl Actor {
    fn session(&mut self) -> ServiceResult<&mut Session> {
        self.session
            .as_mut()
            .ok_or(ServiceError::Internal("session already finalized"))
    }

    fn next_item(&mut self, now_ms: u64) -> ServiceResult<NextItem> {
        // Re-asking before responding returns the same item rather than burning
        // a fresh one: a retried request must not silently consume the bank.
        if let Some((_, item)) = &self.awaiting {
            return Ok(item.clone());
        }
        if self.finalized {
            return Err(ServiceError::SessionTerminated);
        }

        let session_id = self.session_id.clone();
        let session = self.session()?;

        if session.should_terminate() {
            return Err(ServiceError::SessionTerminated);
        }

        // The returned &Question borrows the session; drop it before reading the
        // index back out.
        session.next_question().map_err(map_session_error)?;

        let index = session
            .last_selected_index()
            .ok_or(ServiceError::Internal("bank reported no selected index"))?;
        let expected_remaining = session.expected_remaining().map(|n| n as u32);

        let served = self
            .served
            .get(index)
            .ok_or(ServiceError::Internal("serve log missing the selected item"))?;

        let claims = ItemRefClaims {
            session_id,
            bank_index: index,
            item_id: served.id.clone(),
            challenge_version: served.version,
            issued_at: now_ms,
        };
        let item = NextItem {
            item_ref: self.signer.sign(&claims)?,
            challenge_id: served.id,
            challenge_version: served.version,
            expected_remaining,
        };

        self.awaiting = Some((claims, item.clone()));
        Ok(item)
    }

    fn respond(
        &mut self,
        claims: &ItemRefClaims,
        submission_id: String,
        raw_score: f64,
    ) -> ServiceResult<ResponseOutcome> {
        if claims.session_id != self.session_id {
            return Err(ServiceError::ItemRefInvalid("issued to another session"));
        }

        let (expected, _) = self
            .awaiting
            .as_ref()
            .ok_or(ServiceError::SessionNotAwaitingResponse)?;

        // A validly signed token for some *other* item is still the wrong answer:
        // without this the client could respond against whichever served item it
        // preferred.
        if expected.bank_index != claims.bank_index
            || expected.item_id != claims.item_id
            || expected.challenge_version != claims.challenge_version
        {
            return Err(ServiceError::ItemRefInvalid(
                "does not match the item currently awaiting a response",
            ));
        }

        let item_id = expected.item_id.clone();
        let version = expected.challenge_version;

        // Record BEFORE submitting: arona's `score()` is synchronous and
        // infallible, so it reads this map during `submit_response`.
        self.outcomes.record(&submission_id, raw_score);

        let session = self.session()?;
        let theta_before = session.state().ability.0;
        let result = session
            .submit_response(&submission_id)
            .map_err(map_session_error)?;

        let terminated = session.should_terminate();
        let termination_reason = session.termination_reason();

        self.history.push(SessionItemRecord {
            challenge_id: item_id,
            challenge_version: version,
            raw_score,
            correct: result.correct,
            theta_before,
            theta_after: result.new_ability.0,
        });
        self.awaiting = None;
        self.outcomes.remove(&submission_id);

        Ok(ResponseOutcome {
            correct: result.correct,
            raw_score,
            theta: result.new_ability.0,
            standard_error: result.new_se.0,
            terminated,
            termination_reason,
        })
    }

    fn snapshot(&mut self) -> ServiceResult<SessionSnapshot> {
        let finalized = self.finalized;
        let awaiting = self.awaiting.is_some();
        let session_id = self.session_id.clone();
        let session = self.session()?;

        let state = session.state();
        let se = state.standard_error.0;
        let terminated = session.should_terminate();

        Ok(SessionSnapshot {
            session_id,
            theta: state.ability.0,
            // arona starts at `StandardError::initial()`, which is infinity;
            // JSON has no way to express that, so report absence instead.
            standard_error: se.is_finite().then_some(se),
            response_count: state.response_count() as u32,
            expected_remaining: session.expected_remaining().map(|n| n as u32),
            state: if finalized {
                SessionLifecycle::Finalized
            } else if terminated {
                SessionLifecycle::Terminated
            } else if awaiting {
                SessionLifecycle::AwaitingResponse
            } else {
                SessionLifecycle::Active
            },
            termination_reason: session.termination_reason(),
        })
    }

    fn finalize(&mut self) -> ServiceResult<SessionResultDto> {
        let session = self
            .session
            .take()
            .ok_or(ServiceError::Internal("session already finalized"))?;

        let result = session.finalize();
        self.finalized = true;

        Ok(SessionResultDto {
            session_id: self.session_id.clone(),
            final_theta: result.final_ability.0,
            standard_error: result.standard_error.0,
            total_items: result.total_items as u32,
            // arona's own duration is derived from response timestamps it
            // regenerates on restore, so the service keeps its own clock.
            duration_ms: self.started_at.elapsed().as_millis() as u64,
            termination_reason: result.termination_reason,
            items: std::mem::take(&mut self.history),
        })
    }
}

fn map_session_error(error: SessionError) -> ServiceError {
    match error {
        SessionError::AlreadyTerminated => ServiceError::SessionTerminated,
        SessionError::NoCurrentQuestion => ServiceError::SessionNotAwaitingResponse,
        SessionError::QBankError(QBankError::NoQuestionAvailable) => ServiceError::BankExhausted,
        SessionError::QBankError(_) => ServiceError::Internal("question bank rejected the request"),
    }
}

#[derive(Debug)]
struct Entry {
    handle: SessionHandle,
    /// Server time of the last operation, for idle eviction.
    last_seen: AtomicU64,
}

/// Tracks live sessions.
#[derive(Debug, Default)]
pub struct SessionRegistry {
    sessions: RwLock<HashMap<String, Entry>>,
}

impl SessionRegistry {
    /// An empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Spawn a session actor and register it.
    pub async fn create(
        &self,
        spec: SessionSpec,
        bank: HcrDynamicBank,
        outcomes: OutcomeStore,
        signer: ItemRefSigner,
        created_at_ms: u64,
    ) -> SessionHandle {
        // Take the log handle before the bank disappears into the Session.
        let served = bank.served_log();
        let session = build_session(bank, spec.config, spec.initial_theta);

        let (tx, mut rx) = mpsc::channel(MAILBOX_DEPTH);
        let handle = SessionHandle {
            session_id: spec.session_id.clone(),
            tx,
        };

        let mut actor = Actor {
            session_id: spec.session_id.clone(),
            session: Some(session),
            outcomes,
            served,
            signer,
            awaiting: None,
            history: Vec::new(),
            started_at: Instant::now(),
            finalized: false,
        };

        // One task per session: arona's `Session` is driven through `&mut` and is
        // not internally synchronized, so single ownership removes all locking
        // from the adaptive path.
        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    Command::Next(reply) => {
                        let _ = reply.send(actor.next_item(now_ms()));
                    }
                    Command::Respond {
                        claims,
                        submission_id,
                        raw_score,
                        reply,
                    } => {
                        let _ = reply.send(actor.respond(&claims, submission_id, raw_score));
                    }
                    Command::Snapshot(reply) => {
                        let _ = reply.send(actor.snapshot());
                    }
                    Command::Finalize(reply) => {
                        let outcome = actor.finalize();
                        let done = outcome.is_ok();
                        let _ = reply.send(outcome);
                        if done {
                            break;
                        }
                    }
                }
            }
        });

        self.sessions.write().await.insert(
            spec.session_id,
            Entry {
                handle: handle.clone(),
                last_seen: AtomicU64::new(created_at_ms),
            },
        );
        handle
    }

    /// Look a session up, marking it as active at `now_ms`.
    pub async fn get(&self, session_id: &str, now_ms: u64) -> ServiceResult<SessionHandle> {
        let sessions = self.sessions.read().await;
        let entry = sessions
            .get(session_id)
            .ok_or_else(|| ServiceError::SessionNotFound(session_id.to_string()))?;
        entry.last_seen.store(now_ms, Ordering::Relaxed);
        Ok(entry.handle.clone())
    }

    /// Forget a session.
    pub async fn remove(&self, session_id: &str) {
        self.sessions.write().await.remove(session_id);
    }

    /// Drop sessions idle for longer than `idle_timeout_ms`.
    ///
    /// Dropping the handle closes the actor's mailbox, so its task ends on the
    /// next `recv()` and the arona `Session` — with its bank, estimator and
    /// response history — is freed. Without this, an abandoned browser tab pins
    /// that memory forever.
    ///
    /// Returns the evicted session ids.
    pub async fn evict_idle(&self, now_ms: u64, idle_timeout_ms: u64) -> Vec<String> {
        let mut sessions = self.sessions.write().await;
        let stale: Vec<String> = sessions
            .iter()
            .filter(|(_, entry)| {
                now_ms.saturating_sub(entry.last_seen.load(Ordering::Relaxed)) > idle_timeout_ms
            })
            .map(|(id, _)| id.clone())
            .collect();

        for id in &stale {
            sessions.remove(id);
        }
        stale
    }

    /// Live session count.
    pub async fn len(&self) -> usize {
        self.sessions.read().await.len()
    }

    /// Whether no sessions are live.
    pub async fn is_empty(&self) -> bool {
        self.len().await == 0
    }
}

/// Recover the raw score arona stored, for reporting.
pub fn raw_score_of(remapped: f64, mastery_threshold: f64) -> f64 {
    raw_from_remapped(remapped, mastery_threshold)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
