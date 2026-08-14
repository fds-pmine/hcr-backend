//! The service surface: one method per contract operation.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use hcr_contract::{
    ChallengeDefinitionDto, ChallengeSummary, NextItem, ResponseOutcome, SessionRespond,
    SessionResultDto, SessionSnapshot, SessionStart, SubmissionCreate, SubmissionResult,
    SubmissionStatus, TerminalReason,
};
use hcr_contract::api::{ReplayInfo, ScoreInput};
use hcr_contract::ScoreResult;
use hcr_sim::{VoxelSet, calculate_score, key_to_coord};
use hcr_contract::{
    MatchChallengeRef, MatchConfig, MatchResults, MatchState, MatchSubmissionAck, TimeSync,
};
use hcr_qbank::{
    Blueprint, ExposureController, HcrDynamicBank, OutcomeStore, SessionConfig,
};

use crate::catalog::CatalogStore;
use crate::clock::{SharedClock, system_clock};
use crate::rounds::MatchRegistry;
use crate::error::{ServiceError, ServiceResult};
use crate::itemref::ItemRefSigner;
use crate::replay::{ENGINE_VERSION, ReplayPool, diverged};
use crate::session::{SessionRegistry, SessionSpec};

/// Service-wide configuration.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Adaptive session settings.
    pub session: SessionConfig,
    /// Content blueprint applied to every session.
    pub blueprint: Blueprint,
    /// Exposure policy applied to every session.
    pub exposure: ExposureController,
    /// Base seed; each session derives a distinct, reproducible seed from it.
    pub seed: u64,
    /// Sessions untouched for longer than this are evicted.
    pub session_idle_timeout_ms: u64,
    /// How long a finished round stays readable after anyone last looked at it.
    pub match_results_retention_ms: u64,
    /// A lobby nobody starts is dropped after this long untouched.
    pub match_lobby_idle_timeout_ms: u64,
    /// Cap on retained submission results.
    ///
    /// Bounds the idempotency store, which otherwise grows once per scored
    /// program for as long as the process runs — and on a public deployment
    /// anyone can add to it.
    pub max_retained_submissions: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            session: SessionConfig::default(),
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::default(),
            seed: 0x5EED,
            session_idle_timeout_ms: 30 * 60 * 1000,
            // Long enough to read a scoreboard and argue about it, short enough
            // that a public server is not storing every round ever played.
            match_results_retention_ms: 15 * 60 * 1000,
            match_lobby_idle_timeout_ms: 30 * 60 * 1000,
            max_retained_submissions: 20_000,
        }
    }
}

/// The backend, minus any transport.
///
/// Every method here is a pure function of the request and the service's own
/// state — nothing touches a socket. The MQTT and HTTP bindings are thin adapters
/// that decode a request, call one of these, and encode the reply, which is what
/// lets the whole surface be exercised without a broker or a runtime harness.
#[derive(Debug)]
pub struct HcrService {
    catalog: Arc<CatalogStore>,
    replay: Arc<ReplayPool>,
    sessions: SessionRegistry,
    signer: ItemRefSigner,
    /// Submission id -> result. The idempotency record.
    submissions: Mutex<HashMap<String, SubmissionResult>>,
    config: ServiceConfig,
    counter: AtomicU64,
    clock: SharedClock,
    matches: MatchRegistry,
    usage: Option<Arc<crate::usage::UsageLog>>,
}

impl HcrService {
    /// Assemble a service.
    pub fn new(
        catalog: Arc<CatalogStore>,
        replay: Arc<ReplayPool>,
        signer: ItemRefSigner,
        config: ServiceConfig,
    ) -> Self {
        Self::with_clock(catalog, replay, signer, config, system_clock())
    }

    /// Assemble a service over an explicit clock.
    ///
    /// Round deadlines are judged by this clock, so tests substitute a manual one
    /// rather than waiting out a five-minute round.
    pub fn with_clock(
        catalog: Arc<CatalogStore>,
        replay: Arc<ReplayPool>,
        signer: ItemRefSigner,
        config: ServiceConfig,
        clock: SharedClock,
    ) -> Self {
        Self {
            catalog,
            replay,
            sessions: SessionRegistry::new(),
            signer,
            submissions: Mutex::new(HashMap::new()),
            config,
            counter: AtomicU64::new(0),
            clock: clock.clone(),
            matches: MatchRegistry::new(clock),
            usage: None,
        }
    }

    /// Attach a usage log.
    ///
    /// Off unless a deployment asks for it, so tests and the development server
    /// write nothing to disk. See [`crate::usage`] for what is recorded.
    #[must_use]
    pub fn with_usage_log(mut self, log: Arc<crate::usage::UsageLog>) -> Self {
        self.usage = Some(log);
        self
    }

    fn record(&self, event: crate::usage::UsageEvent) {
        if let Some(log) = &self.usage {
            log.record(&event);
        }
    }

    /// The catalog store.
    pub fn catalog(&self) -> &Arc<CatalogStore> {
        &self.catalog
    }

    /// The replay pool.
    pub fn replay_pool(&self) -> &Arc<ReplayPool> {
        &self.replay
    }

    // -- catalog ---------------------------------------------------------

    /// List the latest version of every challenge.
    pub fn list_challenges(&self) -> ServiceResult<Vec<ChallengeSummary>> {
        self.catalog.list()
    }

    /// Fetch a challenge, at a specific version or the latest.
    pub fn get_challenge(
        &self,
        challenge_id: &str,
        version: Option<u32>,
    ) -> ServiceResult<Arc<ChallengeDefinitionDto>> {
        self.catalog.get(challenge_id, version)
    }

    /// Score a finished run directly, without replaying a program.
    ///
    /// Exists for parity with the v1 frontend's `ScoreProvider`, so an HTTP
    /// implementation of that interface can drop in with no UI or engine change.
    /// It is **not** authoritative — the caller supplies the voxel sets — so it
    /// must never be used to score a competitive round or an adaptive item.
    pub fn score(&self, input: &ScoreInput) -> ServiceResult<ScoreResult> {
        let parse = |keys: &[String]| -> ServiceResult<VoxelSet> {
            keys.iter()
                .map(|key| {
                    key_to_coord(key).ok_or_else(|| ServiceError::ProgramInvalid {
                        message: format!("Malformed voxel key \"{key}\"."),
                        field: Some(key.clone()),
                    })
                })
                .collect()
        };

        let initial = parse(&input.initial_voxels)?;
        let target = parse(&input.target_voxels)?;
        let result = parse(&input.result_voxels)?;
        Ok(calculate_score(
            &initial,
            &target,
            &result,
            &input.program_metrics,
            &input.scoring,
        )?)
    }

    // -- submissions -----------------------------------------------------

    /// Score a program, authoritatively.
    ///
    /// Idempotent on `submission_id`: a repeat returns the stored result rather
    /// than re-scoring. That matters because QoS 1 is at-least-once and HTTP
    /// clients retry, so the same submission genuinely does arrive twice.
    pub async fn create_submission(
        &self,
        request: SubmissionCreate,
    ) -> ServiceResult<SubmissionResult> {
        self.create_submission_for(request, None).await
    }

    /// Score a program, recording who submitted it.
    ///
    /// Separate from [`Self::create_submission`] rather than an added parameter
    /// so the many callers that have no identity to offer stay unchanged. The
    /// player is used **only** for the usage log — it grants nothing and is
    /// never checked, because on a public deployment it is whatever the client
    /// claimed.
    pub async fn create_submission_for(
        &self,
        request: SubmissionCreate,
        player_id: Option<&str>,
    ) -> ServiceResult<SubmissionResult> {
        if let Some(existing) = self.stored_submission(&request.submission_id) {
            return Ok(existing);
        }

        let dto = self
            .catalog
            .get(&request.challenge_id, Some(request.challenge_version))?;
        // Captured before `request` is partly moved into the result below.
        let (match_id, session_id) = (request.match_id.clone(), request.session_id.clone());

        // Which engine runs is decided by the submission, not by configuration:
        // a Cutter Grid program has no joint commands to replay, and a servo
        // program has no trajectory to verify.
        let (outcome, mode, diverged_from_client) = match request.cutter_grid.as_ref() {
            Some(cutter) => {
                let verified = self.replay.verify_cutter(&dto, cutter).await?;
                let diverged = verified.divergence.diverged();
                (
                    verified.replay,
                    crate::usage::ProgrammingMode::CutterGrid,
                    diverged,
                )
            }
            None => {
                // The server expands `repeat` itself inside replay; a
                // client-supplied command list is never trusted, which is what
                // gives the 500-command cap any force.
                let outcome = self.replay.replay(&dto, &request.program).await?;
                let diverged = diverged(request.client_preview.as_ref(), &outcome);
                (
                    (*outcome).clone(),
                    crate::usage::ProgrammingMode::Servo,
                    diverged,
                )
            }
        };

        let status = match outcome.terminal.reason {
            TerminalReason::Completed => SubmissionStatus::Completed,
            _ => SubmissionStatus::Error,
        };

        let result = SubmissionResult {
            submission_id: request.submission_id.clone(),
            challenge_id: request.challenge_id,
            challenge_version: request.challenge_version,
            status,
            score: outcome.score,
            metrics: outcome.metrics,
            result_voxels_hash: outcome.result_voxels_hash.clone(),
            terminal: outcome.terminal.clone(),
            replay: ReplayInfo {
                engine_version: ENGINE_VERSION.to_string(),
                tick_ms: self.replay.options().tick_ms,
                simulated_ms: outcome.simulated_ms,
                diverged_from_client,
            },
            error: None,
        };

        self.store_submission(&result);
        self.record(crate::usage::UsageEvent::from_submission(
            self.now(),
            player_id.map(str::to_owned),
            &result,
            match_id,
            session_id,
            mode,
        ));
        Ok(result)
    }

    /// Fetch a previously scored submission.
    pub fn get_submission(&self, submission_id: &str) -> ServiceResult<SubmissionResult> {
        self.stored_submission(submission_id)
            .ok_or(ServiceError::Internal("no such submission"))
    }

    fn stored_submission(&self, submission_id: &str) -> Option<SubmissionResult> {
        self.submissions
            .lock()
            .ok()?
            .get(submission_id)
            .cloned()
    }

    fn store_submission(&self, result: &SubmissionResult) {
        if let Ok(mut store) = self.submissions.lock() {
            // Crude, like the replay cache's, and for the same reason: a full
            // clear bounds memory without pulling in an LRU. What is lost is
            // idempotency for submissions older than the cap — a client that
            // retried one would have it re-scored, and replay is deterministic
            // so it would get the identical result back. A session that then
            // referenced an evicted submission gets a clean `ItemRefInvalid`
            // rather than a wrong score. Both are acceptable; unbounded growth
            // is not.
            if store.len() >= self.config.max_retained_submissions {
                store.clear();
            }
            store.insert(result.submission_id.clone(), result.clone());
        }
    }

    /// Number of retained submission results.
    pub fn retained_submissions(&self) -> usize {
        self.submissions.lock().map(|s| s.len()).unwrap_or(0)
    }

    // -- sessions --------------------------------------------------------

    /// Open an adaptive session.
    pub async fn start_session(&self, request: SessionStart) -> ServiceResult<SessionSnapshot> {
        let snapshot = self.catalog.snapshot()?;
        if snapshot.is_empty() {
            return Err(ServiceError::BankExhausted);
        }

        let ordinal = self.counter.fetch_add(1, Ordering::Relaxed);
        let session_id = format!("s-{:016x}", self.config.seed ^ (ordinal.wrapping_mul(0x9E37_79B9_7F4A_7C15)));
        let seed = self.config.seed.wrapping_add(ordinal);

        let outcomes = OutcomeStore::new();
        let bank = HcrDynamicBank::new(snapshot, outcomes.clone(), seed)
            .with_blueprint(self.config.blueprint.clone())
            .with_exposure(self.config.exposure.clone());

        let handle = self
            .sessions
            .create(
                SessionSpec {
                    session_id,
                    initial_theta: request.initial_theta.unwrap_or(0.0),
                    config: self.config.session,
                    seed,
                },
                bank,
                outcomes,
                self.signer.clone(),
                self.now(),
            )
            .await;

        handle.snapshot().await
    }

    /// Current state of a session.
    pub async fn session_snapshot(&self, session_id: &str) -> ServiceResult<SessionSnapshot> {
        self.sessions.get(session_id, self.now()).await?.snapshot().await
    }

    /// Serve the next item.
    pub async fn next_item(&self, session_id: &str) -> ServiceResult<NextItem> {
        self.sessions.get(session_id, self.now()).await?.next_item().await
    }

    /// Report a scored submission against the item currently awaiting a response.
    ///
    /// The submission must already have been scored: this reads the authoritative
    /// score rather than accepting one from the caller, which is the whole point
    /// of scoring server-side.
    pub async fn respond(&self, request: SessionRespond) -> ServiceResult<ResponseOutcome> {
        let claims = self.signer.verify(&request.item_ref)?;
        if claims.session_id != request.session_id {
            return Err(ServiceError::ItemRefInvalid("issued to another session"));
        }

        let submission = self
            .stored_submission(&request.submission_id)
            .ok_or(ServiceError::ItemRefInvalid(
                "the referenced submission has not been scored",
            ))?;

        // Guard against answering item A with a program written for item B.
        if submission.challenge_id != claims.item_id
            || submission.challenge_version != claims.challenge_version
        {
            return Err(ServiceError::ItemRefInvalid(
                "the submission scored a different challenge",
            ));
        }

        let raw_score = (submission.score.final_score / 100.0).clamp(0.0, 1.0);
        let challenge_id = submission.challenge_id.clone();

        let outcome = self
            .sessions
            .get(&request.session_id, self.now())
            .await?
            .respond(claims, request.submission_id, raw_score)
            .await?;

        // The ability trajectory: one row per response, so a session can be
        // replayed offline to check the estimator against its own history.
        self.record(crate::usage::UsageEvent::SessionResponse {
            ts: self.now(),
            session_id: request.session_id.clone(),
            challenge_id,
            raw_score: outcome.raw_score,
            correct: outcome.correct,
            theta: outcome.theta,
            standard_error: outcome.standard_error,
        });
        Ok(outcome)
    }

    /// Close a session and issue its result.
    pub async fn finalize_session(&self, session_id: &str) -> ServiceResult<SessionResultDto> {
        let handle = self.sessions.get(session_id, self.now()).await?;
        let result = handle.finalize().await?;
        self.sessions.remove(session_id).await;
        Ok(result)
    }

    /// Live session count.
    pub async fn live_sessions(&self) -> usize {
        self.sessions.len().await
    }

    /// Drop everything nobody is using: idle sessions and finished rounds.
    ///
    /// Must be called periodically. Nothing in the service does it on its own —
    /// [`crate::deploy::serve`] starts the sweeper that does, and a deployment
    /// that skips it leaks memory for as long as it runs.
    ///
    /// Returns `(sessions_evicted, rounds_evicted)`.
    pub async fn evict_idle(&self) -> (usize, usize) {
        let sessions = self.evict_idle_sessions().await.len();
        let rounds = self
            .matches
            .evict_idle(
                self.now(),
                self.config.match_results_retention_ms,
                self.config.match_lobby_idle_timeout_ms,
            )
            .len();
        (sessions, rounds)
    }

    /// Drop sessions idle beyond the configured timeout.
    ///
    /// Call periodically. An abandoned browser tab otherwise pins an arona
    /// `Session` — bank, estimator and full response history — indefinitely.
    pub async fn evict_idle_sessions(&self) -> Vec<String> {
        self.sessions
            .evict_idle(self.now(), self.config.session_idle_timeout_ms)
            .await
    }

    // -- competitive rounds ----------------------------------------------

    /// Server clock, for deadline arithmetic and client offset estimation.
    pub fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    /// Answer a clock-synchronisation probe.
    pub fn time_sync(&self, client_sent_at: u64) -> TimeSync {
        TimeSync {
            client_sent_at,
            server_time: self.now(),
        }
    }

    /// Open a round.
    ///
    /// The challenge is chosen now but **not revealed** until the round starts:
    /// handing it out during the lobby would give early joiners a head start.
    pub fn create_match(&self, config: MatchConfig) -> ServiceResult<MatchState> {
        let challenge = match &config.challenge_ref {
            Some(pinned) => {
                // Fail now if it does not exist, rather than at reveal time.
                self.catalog
                    .get(&pinned.challenge_id, Some(pinned.version))?;
                pinned.clone()
            }
            None => {
                // Not `list()[0]`: a listing is ordered for humans, and taking
                // its head made the item a round ran on an accident of the
                // alphabet. `pick_for_match` chooses for a reason.
                let (challenge_id, version) = self.catalog.pick_for_match()?;
                MatchChallengeRef {
                    challenge_id,
                    version,
                }
            }
        };
        self.matches.create(config, challenge)
    }

    /// Join a round's lobby.
    pub fn join_match(
        &self,
        match_id: &str,
        player_id: &str,
        display_name: &str,
    ) -> ServiceResult<MatchState> {
        self.matches.join(match_id, player_id, display_name)
    }

    /// Start a round: fix the roster, set the deadline, reveal the challenge.
    pub fn start_match(&self, match_id: &str) -> ServiceResult<MatchState> {
        self.matches.start(match_id)
    }

    /// Current round state.
    pub fn match_state(&self, match_id: &str) -> ServiceResult<MatchState> {
        self.matches.state(match_id)
    }

    /// The challenge a round is running, once revealed.
    pub fn match_challenge(&self, match_id: &str) -> ServiceResult<Arc<ChallengeDefinitionDto>> {
        let reference = self.matches.challenge_ref(match_id)?;
        self.catalog
            .get(&reference.challenge_id, Some(reference.version))
    }

    /// Score a program and enter it into a round.
    ///
    /// The reply carries no score. Acceptance is decided purely by server receive
    /// time against the deadline, so a client clock cannot buy extra seconds.
    pub async fn submit_to_match(
        &self,
        match_id: &str,
        player_id: &str,
        request: SubmissionCreate,
    ) -> ServiceResult<MatchSubmissionAck> {
        let result = self.create_submission_for(request, Some(player_id)).await?;
        self.matches.submit(match_id, player_id, &result)
    }

    /// Final standings, once the round has closed.
    pub fn match_results(&self, match_id: &str) -> ServiceResult<MatchResults> {
        let results = self.matches.results(match_id)?;
        // Recorded on first publication only: `results()` refuses until the
        // round closes, and afterwards the room is evicted rather than replayed,
        // so repeated reads of the same standings are cheap duplicates at worst.
        self.record(crate::usage::UsageEvent::MatchResults {
            ts: self.now(),
            match_id: results.match_id.clone(),
            challenge_id: results.challenge_id.clone(),
            challenge_version: results.challenge_version,
            players: results.rows.len(),
            submitted: results
                .rows
                .iter()
                .filter(|row| row.submission_id.is_some())
                .count(),
            top_completion: results
                .rows
                .first()
                .map_or(0.0, |row| row.completion_score),
        });
        Ok(results)
    }

    /// Abandon a round.
    pub fn cancel_match(&self, match_id: &str) -> ServiceResult<MatchState> {
        self.matches.cancel(match_id)
    }
}
