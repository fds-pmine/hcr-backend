//! The service surface: one method per contract operation.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use hcr_contract::ScoreResult;
use hcr_contract::api::{ReplayInfo, ScoreInput};
use hcr_contract::{
    ChallengeDefinitionDto, ChallengeSummary, LessonEventAck, LessonEventCreate, NextItem,
    ProgrammingMode, ResponseOutcome, SessionRespond, SessionResultDto, SessionSnapshot,
    SessionStart, SubmissionCreate, SubmissionResult, SubmissionStatus, TerminalReason,
};
use hcr_contract::{CutterGridPlanRequestV1, CutterGridPlanResponseV1, CutterGridProfileV4};
use hcr_contract::{
    MatchChallengeRef, MatchConfig, MatchResults, MatchState, MatchSubmissionAck, TimeSync,
};
use hcr_qbank::{
    Blueprint, ChallengeGenerator, ExposureController, HcrDynamicBank, OutcomeStore,
    SessionConfig,
};
use hcr_sim::{VoxelSet, calculate_score, key_to_coord};

use crate::catalog::CatalogStore;
use crate::clock::{SharedClock, system_clock};
use crate::cutter_grid_planner::{CutterGridPlannerPool, CutterGridProfileRegistry};
use crate::error::{ServiceError, ServiceResult};
use crate::itemref::ItemRefSigner;
use crate::replay::{ENGINE_VERSION, ReplayPool, diverged};
use crate::rounds::MatchRegistry;
use crate::session::{SessionRegistry, SessionSpec};

/// Longest accepted lesson id.
///
/// Catalogue slugs are `cutter-grid-fixed-axes` and the like; this is roughly
/// three times the longest one that exists. The endpoint writes what it is given
/// into a file, so every free field it carries needs a ceiling.
const LESSON_ID_MAX_BYTES: usize = 64;

/// Refuse a client-chosen id unless it is 1..=MAX_ID_BYTES of `[A-Za-z0-9_-]`.
/// Each becomes a map key, a usage-log field or a roster entry; never echoed (it may be MBs).
pub(crate) fn check_client_id(field: &str, id: &str) -> ServiceResult<()> {
    if (1..=hcr_contract::MAX_ID_BYTES).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Ok(());
    }
    Err(ServiceError::ProgramInvalid {
        message: format!(
            "{field} must be 1 to {} letters, digits, '-' or '_'.",
            hcr_contract::MAX_ID_BYTES
        ),
        field: Some(field.to_string()),
    })
}

/// Highest accepted lesson section index. The longest course has twenty.
const LESSON_SECTION_MAX: u32 = 200;

/// Highest accepted Test count for one lesson. Above this it is a script, not a
/// class, and the number is being used to make a row look busy.
const LESSON_TESTS_MAX: u32 = 10_000;

/// Item ceiling for a practice session.
///
/// Not a measurement budget — a runaway guard. Nobody cuts five hundred
/// hairstyles in a sitting, and a session object that grows without any bound
/// at all is a leak rather than a feature.
const PRACTICE_ITEM_CEILING: usize = 500;

/// Precision a practice session never reaches, so it never stops early.
///
/// Zero would be the honest expression of "do not stop on precision", and
/// arona rejects it — `target_se` must be positive. This is small enough that
/// no attainable amount of information satisfies it.
const PRACTICE_TARGET_SE: f64 = 1e-6;

/// Share of a practice session any one hairstyle may occupy.
const PRACTICE_MAX_EXPOSURE: f64 = 0.1;

/// Administrations before that cap applies.
///
/// Not zero: the first item served would otherwise sit at 100% of a
/// one-administration history and lock itself out immediately.
const PRACTICE_EXPOSURE_WARMUP: u64 = 12;

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
    /// How long a finished round stays readable after its close or anyone's
    /// last look at it, whichever is later.
    pub match_results_retention_ms: u64,
    /// A lobby nobody starts is dropped after this long untouched.
    pub match_lobby_idle_timeout_ms: u64,
    /// Item generator for practice sessions, if any.
    ///
    /// Only consulted when `SessionStart::practice` is set. A measurement
    /// session must not synthesise items, so this is never reached from one.
    pub generator: Option<Arc<dyn ChallengeGenerator>>,
    /// Cap on retained submission results.
    ///
    /// Bounds the idempotency store, which otherwise grows once per scored
    /// program for as long as the process runs — and on a public deployment
    /// anyone can add to it.
    pub max_retained_submissions: usize,
    /// Cap on live adaptive sessions; a start beyond it gets 429.
    pub max_live_sessions: usize,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            session: SessionConfig::default(),
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::default(),
            generator: None,
            seed: 0x5EED,
            session_idle_timeout_ms: 30 * 60 * 1000,
            // Long enough to read a scoreboard and argue about it, short enough
            // that a public server is not storing every round ever played.
            match_results_retention_ms: 15 * 60 * 1000,
            match_lobby_idle_timeout_ms: 30 * 60 * 1000,
            max_retained_submissions: 20_000,
            max_live_sessions: 10_000,
        }
    }
}

/// A scored result plus the submitter it is bound to, so its id cannot be
/// reused under another player, session or challenge.
#[derive(Debug, Clone)]
struct StoredSubmission {
    player_id: Option<String>,
    session_id: Option<String>,
    /// The consent the scoring request carried; a session response inherits it,
    /// because `/sessions/{id}/responses` carries no consent header.
    research_consent: bool,
    result: SubmissionResult,
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
    cutter_grid_planner: Arc<CutterGridPlannerPool>,
    sessions: SessionRegistry,
    signer: ItemRefSigner,
    /// Submission id -> result and who it was scored for. The idempotency record.
    submissions: Mutex<HashMap<String, StoredSubmission>>,
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
        let cutter_grid_profiles = Arc::new(CutterGridProfileRegistry::with_bundled_profiles());
        Self {
            catalog,
            replay,
            cutter_grid_planner: Arc::new(CutterGridPlannerPool::with_default_concurrency(
                cutter_grid_profiles,
            )),
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

    /// Record one client-reported lesson interaction.
    ///
    /// The lessons are a frontend artifact: this service has no lesson
    /// catalogue, replays nothing and scores nothing here, so there is no way to
    /// check that the reported lesson exists or that any of it happened. What it
    /// can do is refuse to write a row that is not the shape of a lesson event —
    /// the endpoint appends to a file and needs no authentication, so the bounds
    /// below are the whole defence, and they are deliberately tight.
    ///
    /// Returns `recorded: false` when the deployment collects no usage, which is
    /// the default and not an error, or when `research_consent` is false.
    pub fn record_lesson_event(
        &self,
        request: LessonEventCreate,
        player_id: Option<&str>,
        research_consent: bool,
    ) -> ServiceResult<LessonEventAck> {
        let id = request.lesson_id.as_str();
        if id.is_empty() || id.len() > LESSON_ID_MAX_BYTES {
            return Err(ServiceError::ProgramInvalid {
                message: format!(
                    "A lesson id must be 1 to {LESSON_ID_MAX_BYTES} characters, not {}.",
                    id.len()
                ),
                field: Some("lessonId".to_string()),
            });
        }
        if !id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(ServiceError::ProgramInvalid {
                message: "A lesson id is a catalogue slug: lower-case letters, digits and hyphens."
                    .to_string(),
                field: Some("lessonId".to_string()),
            });
        }
        if request.section > LESSON_SECTION_MAX {
            return Err(ServiceError::ProgramInvalid {
                message: format!("A lesson section index must not exceed {LESSON_SECTION_MAX}."),
                field: Some("section".to_string()),
            });
        }
        if request.tests.is_some_and(|tests| tests > LESSON_TESTS_MAX) {
            return Err(ServiceError::ProgramInvalid {
                message: format!("A lesson test count must not exceed {LESSON_TESTS_MAX}."),
                field: Some("tests".to_string()),
            });
        }

        if self.usage.is_none() || !research_consent {
            return Ok(LessonEventAck { recorded: false });
        }
        self.record(crate::usage::UsageEvent::from_lesson_event(
            self.now(),
            player_id.map(str::to_owned),
            request,
        ));
        Ok(LessonEventAck { recorded: true })
    }

    /// The catalog store.
    pub fn catalog(&self) -> &Arc<CatalogStore> {
        &self.catalog
    }

    /// The replay pool.
    pub fn replay_pool(&self) -> &Arc<ReplayPool> {
        &self.replay
    }

    /// V4 server-side compact planning pool. It is separate from replay and
    /// never participates in scoring, Sessions, Matches, or ArmDock.
    pub fn cutter_grid_planner_pool(&self) -> &Arc<CutterGridPlannerPool> {
        &self.cutter_grid_planner
    }

    /// Register an immutable server-owned V4 Profile during bootstrap.
    pub fn register_cutter_grid_profile(
        &self,
        challenge_id: impl Into<String>,
        challenge_version: u32,
        profile: CutterGridProfileV4,
    ) -> ServiceResult<()> {
        self.cutter_grid_planner
            .profiles()
            .register(challenge_id, challenge_version, profile)
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

    /// Produce a server-authoritative V4 Cutter Grid plan for Practice only.
    ///
    /// This endpoint does not submit, score, write a Session response, join a
    /// Match, or make any physical-arm request. Those remain explicitly
    /// Servo/V2-only until separately authorized.
    pub async fn plan_cutter_grid(
        &self,
        request: CutterGridPlanRequestV1,
    ) -> ServiceResult<CutterGridPlanResponseV1> {
        let challenge = self
            .catalog
            .get(&request.challenge_id, Some(request.challenge_version))?;
        Ok(self
            .cutter_grid_planner
            .plan(&challenge, &request.program)
            .await?
            .response)
    }

    // -- submissions -----------------------------------------------------

    /// Score a program, authoritatively.
    ///
    /// Idempotent on `submission_id`: a repeat returns the stored result rather
    /// than re-scoring. That matters because QoS 1 is at-least-once and HTTP
    /// clients retry, so the same submission genuinely does arrive twice. The
    /// key is scoped to the submitter (player, session) and challenge; any other
    /// reuse of the id is `ProgramInvalid`.
    ///
    /// For in-process callers, so it records usage as consented; HTTP goes
    /// through [`Self::create_submission_for`] with the request's answer.
    pub async fn create_submission(
        &self,
        request: SubmissionCreate,
    ) -> ServiceResult<SubmissionResult> {
        self.create_submission_for(request, None, true).await
    }

    /// Score a program, recording who submitted it.
    ///
    /// Separate from [`Self::create_submission`] rather than an added parameter
    /// so the many callers that have no identity to offer stay unchanged. The
    /// player goes to the usage log and scopes the idempotency key; it is still
    /// not authenticated, because on a public deployment it is whatever the
    /// client claimed. Without `research_consent` no usage row is written, now
    /// or for a session response that later names this submission.
    pub async fn create_submission_for(
        &self,
        request: SubmissionCreate,
        player_id: Option<&str>,
        research_consent: bool,
    ) -> ServiceResult<SubmissionResult> {
        check_client_id("submissionId", &request.submission_id)?;
        if let Some(id) = &request.session_id {
            check_client_id("sessionId", id)?;
        }
        if let Some(id) = &request.match_id {
            check_client_id("matchId", id)?;
        }
        if let Some(existing) = self.stored_submission(&request.submission_id) {
            // shortcut: the owner is client-claimed (X-HCR-Player is spoofable, F28); this stops reuse under another identity, not impersonation; upgrade when players authenticate.
            // shortcut: check-then-store is not atomic (Q8): concurrent same-id requests both replay and log, and the last store owns the id; add an in-flight map if a client starts retrying before the first reply.
            if existing.player_id.as_deref() != player_id
                || existing.session_id != request.session_id
                || existing.result.challenge_id != request.challenge_id
                || existing.result.challenge_version != request.challenge_version
            {
                return Err(ServiceError::ProgramInvalid {
                    message: "This submissionId was already used for a different submission; send a new id.".into(),
                    field: None,
                });
            }
            return Ok(existing.result);
        }

        let dto = self
            .catalog
            .get(&request.challenge_id, Some(request.challenge_version))?;
        // Captured before `request` is partly moved into the result below.
        let (match_id, session_id) = (request.match_id.clone(), request.session_id.clone());

        // Two ways to say "Cutter Grid" would be one way too many: they would
        // answer with different motions for the same program, and whichever the
        // server preferred would be a rule nobody wrote down.
        if request.cutter_grid.is_some() && request.cutter_grid_v4.is_some() {
            return Err(ServiceError::ProgramInvalid {
                message: "A submission carries either a planned Cutter Grid trajectory or a V4 program, not both.".into(),
                field: None,
            });
        }

        // Which engine runs is decided by the submission, not by configuration:
        // a Cutter Grid program has no joint commands to replay, and a servo
        // program has no trajectory to verify.
        let (outcome, mode, diverged_from_client) = if let Some(program) =
            request.cutter_grid_v4.as_ref()
        {
            /*
                V4: the server plans the route itself.

                The client sent the lattice program and nothing else, so there
                is no trajectory to audit and no signature to compare — the
                plan is this machine's, produced from a Profile only this
                machine holds. `plan` fails closed when no certified Profile is
                registered for the Challenge, which is the same answer the
                lobby already gave when the round was opened.
            */
            let planned = self.cutter_grid_planner.plan(&dto, program).await?;
            let outcome = hcr_sim::replay_planned_v4(
                &dto.challenge,
                program.scored_block_count(),
                &planned.response.plan,
            )?;
            (
                outcome,
                crate::usage::ProgrammingMode::CutterGrid,
                // Nothing to diverge from: the client reported no plan and no
                // preview this path could be compared against.
                false,
            )
        } else {
            match request.cutter_grid.as_ref() {
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
            programming_mode: mode,
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

        self.store_submission(
            &result,
            player_id.map(str::to_owned),
            session_id.clone(),
            research_consent,
        );
        // `dto` is the version that was actually replayed against, so its
        // scoring block is the one that produced these scores — not whatever
        // the catalog holds for this challenge by the time anyone reads the log.
        if research_consent {
            self.record(crate::usage::UsageEvent::from_submission(
                self.now(),
                player_id.map(str::to_owned),
                &result,
                match_id,
                session_id,
                mode,
                &dto.challenge.scoring,
            ));
        }
        Ok(result)
    }

    /// Fetch a previously scored submission.
    pub fn get_submission(&self, submission_id: &str) -> ServiceResult<SubmissionResult> {
        self.stored_submission(submission_id)
            .map(|stored| stored.result)
            .ok_or(ServiceError::Internal("no such submission"))
    }

    fn stored_submission(&self, submission_id: &str) -> Option<StoredSubmission> {
        self.submissions.lock().ok()?.get(submission_id).cloned()
    }

    fn store_submission(
        &self,
        result: &SubmissionResult,
        player_id: Option<String>,
        session_id: Option<String>,
        research_consent: bool,
    ) {
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
                // shortcut: ~20k distinct ids flush every stored result, so an in-flight session respond gets ITEM_REF_INVALID; switch to FIFO eviction if that is ever seen in class.
                store.clear();
            }
            store.insert(
                result.submission_id.clone(),
                StoredSubmission {
                    player_id,
                    session_id,
                    research_consent,
                    result: result.clone(),
                },
            );
        }
    }

    /// Number of retained submission results.
    pub fn retained_submissions(&self) -> usize {
        self.submissions.lock().map(|s| s.len()).unwrap_or(0)
    }

    // -- sessions --------------------------------------------------------

    /// Open an adaptive session.
    pub async fn start_session(&self, request: SessionStart) -> ServiceResult<SessionSnapshot> {
        // shortcut: one global cap; a flooder that keeps polling its own sessions (GET refreshes last_seen) locks out new starts indefinitely instead of OOM-killing the process, and check-then-insert can overshoot by in-flight requests. Per-client limits are nginx's (F21); move to per-player caps if authentication lands.
        if self.sessions.len().await >= self.config.max_live_sessions {
            return Err(ServiceError::RateLimited);
        }

        // Only items playable in this mode, so adaptive selection chooses from
        // what it can actually serve — and "playable" includes this process
        // holding a Profile to plan the route with, not merely the catalog
        // advertising the mode. Without that an adaptive Cutter Grid session
        // would serve an item whose every submission fails `planner-not-ready`,
        // and the learner would be the one to find out.
        let snapshot = self
            .catalog
            .snapshot_for_playable(request.programming_mode, |dto| {
                self.plannable_in_mode(dto, request.programming_mode).is_ok()
            })?;
        if snapshot.is_empty() {
            return Err(ServiceError::BankExhausted);
        }

        let ordinal = self.counter.fetch_add(1, Ordering::Relaxed);
        // Random, not counter-derived: the id is the only capability on a session (F6), and a restart must not reissue old ids (F14).
        let session_id = format!("s-{:016x}", rand::random::<u64>());
        let seed = self.config.seed.wrapping_add(ordinal);

        let outcomes = OutcomeStore::new();
        let mut bank = HcrDynamicBank::new(snapshot, outcomes.clone(), seed)
            .with_blueprint(self.config.blueprint.clone())
            .with_exposure(self.config.exposure.clone());

        // A practice session is not trying to converge on a number, so the two
        // rules that end a measurement session are wrong for it: stopping once
        // the standard error is small enough, and the item ceiling. Both are
        // lifted here, which is what makes the session endless rather than
        // twelve items long.
        let session_config = if request.practice {
            SessionConfig {
                target_se: PRACTICE_TARGET_SE,
                max_items: PRACTICE_ITEM_CEILING,
                ..self.config.session
            }
        } else {
            self.config.session
        };

        if request.practice {
            // Provisional items are fine here: nothing this session reports is
            // an authoritative measurement.
            bank = bank.allow_uncalibrated(true);

            // Practice wants variety, and the default cap does not deliver it:
            // at a fifth of all administrations an item may come back eight
            // times in forty. Tightening the cap is also what *reaches* the
            // generator — selection only synthesises when nothing in the pool
            // is servable, so the cap biting is the trigger.
            bank = bank.with_exposure(
                ExposureController::new(PRACTICE_MAX_EXPOSURE)
                    .with_warmup(PRACTICE_EXPOSURE_WARMUP),
            );

            // Generation is Servo-only, and deliberately so. The generator
            // emits `Cap Trim` items, which ship no certified Cutter Grid
            // profile — synthesising one into a Cutter Grid session would serve
            // a challenge that cannot be planned, let alone finished.
            if request.programming_mode == ProgrammingMode::Servo {
                if let Some(generator) = self.config.generator.clone() {
                    bank = bank.with_generator(generator);
                }
            }
        }

        let handle = self
            .sessions
            .create(
                SessionSpec {
                    session_id,
                    initial_theta: request.initial_theta.unwrap_or(0.0),
                    config: session_config,
                    seed,
                    programming_mode: request.programming_mode,
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
        self.sessions
            .get(session_id, self.now())
            .await?
            .snapshot()
            .await
    }

    /// Serve the next item.
    pub async fn next_item(&self, session_id: &str) -> ServiceResult<NextItem> {
        self.sessions
            .get(session_id, self.now())
            .await?
            .next_item()
            .await
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

        let stored =
            self.stored_submission(&request.submission_id)
                .ok_or(ServiceError::ItemRefInvalid(
                    "the referenced submission has not been scored",
                ))?;
        if stored.session_id.as_deref() != Some(request.session_id.as_str()) {
            return Err(ServiceError::ItemRefInvalid(
                "the submission was scored for another session",
            ));
        }
        let submission = stored.result;

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
            .respond(
                claims,
                request.submission_id,
                raw_score,
                submission.programming_mode,
            )
            .await?;

        // The ability trajectory: one row per response, so a session can be
        // replayed offline to check the estimator against its own history.
        if stored.research_consent {
            self.record(crate::usage::UsageEvent::SessionResponse {
                ts: self.now(),
                session_id: request.session_id.clone(),
                challenge_id,
                // The session refused any other mode above, so this is the
                // session's mode as much as the submission's.
                mode: submission.programming_mode,
                raw_score: outcome.raw_score,
                correct: outcome.correct,
                theta: outcome.theta,
                standard_error: outcome.standard_error,
            });
        }
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

    /// Whether *this* server can produce the motion a round in `mode` needs.
    ///
    /// Servo always: replay is the engine, and every challenge has one. Cutter
    /// Grid only where a certified V4 Profile is registered, because the server
    /// plans the route itself now (`SubmissionCreate::cutter_grid_v4`) and a
    /// route it cannot plan is a round nobody can enter.
    fn plannable_in_mode(
        &self,
        dto: &ChallengeDefinitionDto,
        mode: ProgrammingMode,
    ) -> ServiceResult<()> {
        if mode != ProgrammingMode::CutterGrid {
            return Ok(());
        }
        self.cutter_grid_planner
            .profiles()
            .profile_for(dto)
            .map(|_| ())
            .map_err(|_| ServiceError::ProgramInvalid {
                message: format!(
                    "This server holds no certified Cutter Grid Profile for challenge \"{}\".",
                    dto.challenge.id
                ),
                field: None,
            })
    }

    /// Choose a challenge a round in `mode` can actually be played on.
    ///
    /// `pick_for_match_excluding` answers from the catalog, which advertises
    /// Cutter Grid for every item that ships a Profile with the frontend. This
    /// process holds fewer of those, so an item it cannot plan is skipped and
    /// the next one asked for, rather than becoming a round nobody can enter.
    fn pick_playable_excluding(
        &self,
        mode: ProgrammingMode,
        exclude: &[String],
    ) -> ServiceResult<(String, u32)> {
        let mut refused = exclude.to_vec();
        loop {
            let (challenge_id, version) =
                self.catalog.pick_for_match_excluding(mode, &refused)?;
            let dto = self.catalog.get(&challenge_id, Some(version))?;
            if self.plannable_in_mode(&dto, mode).is_ok() {
                return Ok((challenge_id, version));
            }
            refused.push(challenge_id);
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
                let dto = self
                    .catalog
                    .get(&pinned.challenge_id, Some(pinned.version))?;
                // Same reasoning, one step further: a round pinned to a
                // challenge that cannot be played in the round's mode is a lobby
                // nobody can submit into, and the players would only find that
                // out at T0 when the challenge is finally revealed.
                if !dto.meta.supports(config.programming_mode) {
                    return Err(ServiceError::ProgramInvalid {
                        message: format!(
                            "Challenge \"{}\" cannot be played in {} mode.",
                            pinned.challenge_id,
                            config.programming_mode.as_str()
                        ),
                        field: None,
                    });
                }
                // And one step further still, because the catalog's answer and
                // this server's are not the same answer: an item advertises the
                // mode, a *Profile* is what lets this process plan it. A round
                // opened on the difference would take submissions it could only
                // refuse, one per player, at the end of the round.
                self.plannable_in_mode(&dto, config.programming_mode)?;
                pinned.clone()
            }
            None => {
                // Not `list()[0]`: a listing is ordered for humans, and taking
                // its head made the item a round ran on an accident of the
                // alphabet. `pick_for_match` chooses for a reason.
                //
                let (challenge_id, version) =
                    self.pick_playable_excluding(config.programming_mode, &[])?;
                MatchChallengeRef {
                    challenge_id,
                    version,
                }
            }
        };
        self.matches.create(config, challenge)
    }

    /// Reopen a finished round on a different challenge, keeping the roster.
    ///
    /// The challenge is chosen the same way `create_match` chooses an unpinned
    /// one, then rejected if it is the one the room just played: a rematch that
    /// served the same hairstyle again would be a memory test, and
    /// `pick_for_match` is deterministic, so without the check it would serve
    /// exactly that every time. A catalog holding a single playable item cannot
    /// satisfy that and says so rather than silently repeating itself.
    pub fn rematch(&self, match_id: &str) -> ServiceResult<MatchState> {
        let state = self.matches.state(match_id)?;
        let mode = state.config.programming_mode;
        let played = self.matches.played_challenges(match_id)?;

        // Everything this room has already played is refused, so a session
        // works through the bank instead of flipping between two items. When
        // the bank runs out the rotation starts again, still refusing an
        // immediate repeat — a room that has played everything should carry on
        // rather than stop.
        let (challenge_id, version, wrapped) =
            match self.pick_playable_excluding(mode, &played) {
                Ok((challenge_id, version)) => (challenge_id, version, false),
                Err(ServiceError::BankExhausted) => {
                    let last = played.last().cloned().into_iter().collect::<Vec<_>>();
                    match self.pick_playable_excluding(mode, &last) {
                        Ok((challenge_id, version)) => (challenge_id, version, true),
                        // One playable item, which is where a Cutter Grid round
                        // stands until more Profiles are registered here. The
                        // room replays it rather than being told it cannot
                        // reopen: the same hairstyle twice is a worse round, and
                        // no round at all is not a round.
                        Err(ServiceError::BankExhausted) => {
                            let (challenge_id, version) =
                                self.pick_playable_excluding(mode, &[])?;
                            (challenge_id, version, true)
                        }
                        Err(other) => return Err(other),
                    }
                }
                Err(other) => return Err(other),
            };

        self.matches.rematch(
            match_id,
            MatchChallengeRef {
                challenge_id,
                version,
            },
            wrapped,
        )
    }

    /// Put the room's players into crews. Lobby only.
    pub fn set_match_crews(
        &self,
        match_id: &str,
        crews: &std::collections::HashMap<String, String>,
    ) -> ServiceResult<MatchState> {
        for crew in crews.values() {
            check_client_id("crews", crew)?;
        }
        self.matches.set_crews(match_id, crews)
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
        research_consent: bool,
    ) -> ServiceResult<MatchSubmissionAck> {
        let result = self
            .create_submission_for(request, Some(player_id), research_consent)
            .await?;
        self.matches.submit(match_id, player_id, &result)
    }

    /// Final standings, once the round has closed.
    pub fn match_results(&self, match_id: &str) -> ServiceResult<MatchResults> {
        let (results, first_publication) = self.matches.results_once(match_id)?;
        // Recorded on first publication only. Every client in the room polls
        // this endpoint once the round closes, so recording per call recorded
        // per *reader*: a seventeen-player round wrote fifteen identical rows
        // and "how many rounds were played" came out multiplied by however many
        // people happened to be looking. The registry decides which call is the
        // first, under its own lock.
        //
        // Rows written before this fix still carry those duplicates, and no
        // rewrite will reach them — the log is append-only. Deduplicating on
        // (matchId, challengeId) reads both eras correctly, which is what
        // `docs/DEPLOY.md` §3 now tells an analysis to do.
        if first_publication {
            self.record(crate::usage::UsageEvent::MatchResults {
                ts: self.now(),
                match_id: results.match_id.clone(),
                challenge_id: results.challenge_id.clone(),
                challenge_version: results.challenge_version,
                mode: results.programming_mode,
                players: results.rows.len(),
                submitted: results
                    .rows
                    .iter()
                    .filter(|row| row.submission_id.is_some())
                    .count(),
                top_completion: results.rows.first().map_or(0.0, |row| row.completion_score),
            });
        }
        Ok(results)
    }

    /// Abandon a round.
    pub fn cancel_match(&self, match_id: &str) -> ServiceResult<MatchState> {
        self.matches.cancel(match_id)
    }
}
