//! Request and response payloads.
//!
//! Mirrors sections 4–5 of `docs/backend/schema/hcr-v1.d.ts`. These are the
//! bodies carried by both bindings: as MQTT envelope payloads, and as HTTP
//! request/response bodies.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::cutter::ProgrammingMode;
use crate::domain::{Program, ProgramMetrics, ScoreResult, ScoringConfig, Terminal};
use crate::wire::HcrError;

/// Input to direct scoring.
///
/// Mirrors the v1 frontend's `ScoreInput`. Voxel sets travel as `"x,y,z"` key
/// strings because that is the v1 `VoxelKey` and JSON has no set type; the server
/// parses them back into lattice coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScoreInput {
    /// The hairstyle before the program ran.
    ///
    /// Completion is scored on the *cut* — which hair came off against which
    /// hair should have — and that cannot be reconstructed from the target and
    /// the result alone.
    pub initial_voxels: Vec<String>,
    /// The hairstyle being aimed for.
    pub target_voxels: Vec<String>,
    /// What the program actually left standing.
    pub result_voxels: Vec<String>,
    /// Program size and timing.
    pub program_metrics: ProgramMetrics,
    /// Scoring parameters.
    pub scoring: ScoringConfig,
}

/// Catalog listing entry. Identical to the v1 frontend type.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeSummary {
    /// Stable identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Display description.
    pub description: String,
}

/// What the browser computed locally. Advisory only.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientPreview {
    /// The client's own score.
    pub score_result: ScoreResult,
    /// Hash over its sorted result voxels.
    pub result_voxels_hash: String,
    /// Which engine build produced it.
    pub engine_version: String,
    /// Tick size it used.
    pub tick_ms: f64,
}

/// A program submitted for authoritative scoring.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionCreate {
    /// Client-generated ULID. The idempotency key: resubmitting returns the
    /// first result rather than re-scoring.
    pub submission_id: String,
    /// Which challenge.
    pub challenge_id: String,
    /// Which version of it. Pinned so recalibration cannot move the score.
    pub challenge_version: u32,
    /// Program IR. Never a pre-expanded command list — the server expands
    /// `repeat` itself so the command cap means something.
    ///
    /// Still required for a Cutter Grid submission, which sets `cutterGrid` as
    /// well: this field then carries an empty servo program. Making it optional
    /// would ripple through every existing client and reader for the benefit of
    /// one mode.
    pub program: Program,
    /// Present when the submission was written in Cutter Grid rather than with
    /// joint angles.
    ///
    /// Additive and optional: a servo submission is byte-identical to what it
    /// was before this field existed, and a server that ignores it still speaks
    /// a complete v1. When set, the server verifies the carried trajectory and
    /// scores *that* instead of replaying `program`
    /// (`docs/backend/08-CUTTER-GRID.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cutter_grid: Option<crate::cutter::CutterGridSubmission>,
    /// Set when this submission belongs to an adaptive session.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Set when it belongs to a competitive round.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub match_id: Option<String>,
    /// Advisory client-side result, used only for divergence telemetry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_preview: Option<ClientPreview>,
}

/// Terminal disposition of a scored submission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SubmissionStatus {
    /// The program ran to completion.
    Completed,
    /// It was halted — by the head constraint, the command cap, or a budget.
    Error,
}

/// Provenance of the authoritative replay.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayInfo {
    /// Engine build that produced the score.
    pub engine_version: String,
    /// Tick size used.
    pub tick_ms: f64,
    /// Simulated milliseconds consumed.
    pub simulated_ms: f64,
    /// Whether the client's preview disagreed beyond tolerance.
    ///
    /// A conformance alarm for operators, not a user-facing error: telling a
    /// learner their simulation disagreed with the server is noise they cannot
    /// act on.
    pub diverged_from_client: bool,
}

/// The authoritative result of scoring a submission.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubmissionResult {
    /// Echoes the request.
    pub submission_id: String,
    /// Which challenge was scored.
    pub challenge_id: String,
    /// At which version.
    pub challenge_version: u32,
    /// Completed or halted.
    pub status: SubmissionStatus,
    /// Which editor wrote the program that was scored.
    ///
    /// Skipped when servo, so a servo result is byte-identical to what earlier
    /// builds returned. A round reads it to refuse a submission written in a
    /// mode the round is not being played in.
    #[serde(default, skip_serializing_if = "ProgrammingMode::is_default")]
    pub programming_mode: ProgrammingMode,
    /// The score of record.
    pub score: ScoreResult,
    /// Program size and timing.
    pub metrics: ProgramMetrics,
    /// Canonical hash of the remaining voxels.
    pub result_voxels_hash: String,
    /// Why the run ended, with attribution.
    pub terminal: Terminal,
    /// Replay provenance.
    pub replay: ReplayInfo,
    /// Present when the submission failed validation outright.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<HcrError>,
}

/// Request to open an adaptive session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStart {
    /// Content blueprint to apply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint_id: Option<String>,
    /// Starting ability estimate.
    ///
    /// Must come from the **same** programming mode this session runs in. The
    /// two modes measure different abilities, so seeding a Cutter Grid session
    /// with a servo θ would start the search in the wrong place and take several
    /// items to recover — the exact cost adaptive selection exists to avoid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_theta: Option<f64>,
    /// Which editor this session is practised in.
    ///
    /// A session runs in one mode throughout. The ability it estimates belongs
    /// to that mode and to no other, mirroring the rule that match play never
    /// touches θ_solo ([`07-CALIBRATION.md`] §2): same library, same scale,
    /// different condition.
    ///
    /// Only items declaring support are served, which today means a Cutter Grid
    /// session has a very small bank — one challenge ships with a certified
    /// planner profile.
    #[serde(default, skip_serializing_if = "ProgrammingMode::is_default")]
    pub programming_mode: ProgrammingMode,
}

/// Where a session is in its lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SessionLifecycle {
    /// Ready to serve the next item.
    Active,
    /// An item has been served and is awaiting a response.
    AwaitingResponse,
    /// The terminator has fired; no further items.
    Terminated,
    /// Finalized; the result has been issued.
    Finalized,
}

/// Public view of a session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSnapshot {
    /// Session identity.
    pub session_id: String,
    /// Ability estimate on the logit scale.
    pub theta: f64,
    /// Standard error. `None` before the first response, where arona reports
    /// infinity and JSON has no way to say so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub standard_error: Option<f64>,
    /// Responses recorded.
    pub response_count: u32,
    /// Items the terminator expects to still need.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_remaining: Option<u32>,
    /// Lifecycle position.
    pub state: SessionLifecycle,
    /// Why it terminated, when it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<String>,
}

/// The next item to attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NextItem {
    /// Opaque signed token binding this issue to the session and bank index.
    pub item_ref: String,
    /// Which challenge to load.
    pub challenge_id: String,
    /// At which version.
    pub challenge_version: u32,
    /// Items the terminator expects to still need.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_remaining: Option<u32>,
}

/// Report a scored submission against a served item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRespond {
    /// Which session.
    pub session_id: String,
    /// The token issued with the item.
    pub item_ref: String,
    /// The submission that was scored.
    pub submission_id: String,
}

/// Effect of a response on the ability estimate.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseOutcome {
    /// arona's dichotomised verdict, after the mastery-threshold remap.
    pub correct: bool,
    /// Raw normalized score, retained for future polytomous models.
    pub raw_score: f64,
    /// Updated ability estimate.
    pub theta: f64,
    /// Updated standard error.
    pub standard_error: f64,
    /// Whether the session has now terminated.
    pub terminated: bool,
    /// Why, when it has.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub termination_reason: Option<String>,
}

/// One item's contribution to a finished session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionItemRecord {
    /// Which challenge.
    pub challenge_id: String,
    /// At which version.
    pub challenge_version: u32,
    /// Raw normalized score.
    pub raw_score: f64,
    /// Whether it counted as mastery.
    pub correct: bool,
    /// Ability before the response.
    pub theta_before: f64,
    /// Ability after.
    pub theta_after: f64,
}

/// The result of a completed session.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionResultDto {
    /// Session identity.
    pub session_id: String,
    /// Final ability estimate.
    pub final_theta: f64,
    /// Final standard error.
    pub standard_error: f64,
    /// Items administered.
    pub total_items: u32,
    /// Wall-clock duration.
    pub duration_ms: u64,
    /// Terminator's stated reason.
    pub termination_reason: String,
    /// Per-item history.
    pub items: Vec<SessionItemRecord>,
}

/// What a lesson section asks of the learner.
///
/// The same six kinds the lesson cards print, so a row can be grouped by the
/// activity rather than only by position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LessonActivity {
    /// Read an explanation.
    Read,
    /// Predict an outcome before running anything.
    Predict,
    /// Build a program.
    Build,
    /// Watch what the arm does.
    Observe,
    /// A drill: repair or change a route that is already on the canvas.
    Challenge,
    /// Recap of what the lesson established.
    Recap,
}

/// What happened in a lesson.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LessonOutcome {
    /// It was opened. `section` is where it resumed, which need not be 0.
    Opened,
    /// A section's own gate was met and the learner moved past it.
    SectionPassed,
    /// The closed-book quiz was answered correctly.
    QuizPassed,
    /// Quiz and practical both passed.
    Completed,
    /// The learner left before finishing; `section` is where they stopped.
    Abandoned,
}

/// One thing a learner did in a lesson.
///
/// **Client-asserted.** The lessons run and score in the browser — Cutter Grid is
/// outside server-side scoring by design (`docs/08-CUTTER-GRID.md` §0) and the
/// servo lessons never needed a server — so the service records what the client
/// reports and can verify none of it. It answers "are the lessons used, and where
/// do people stop"; it is not evidence of attainment, and nothing calibrates
/// against it. That remains [`SubmissionCreate`], which the server replays.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LessonEventCreate {
    /// Which lesson, as the frontend catalogue names it (`cutter-grid-…`).
    ///
    /// The server holds no lesson catalogue — lessons are a frontend artifact —
    /// so this is checked for shape and length, not membership.
    pub lesson_id: String,
    /// Zero-based section index within that lesson.
    pub section: u32,
    /// What that section asks for. Absent on whole-lesson outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<LessonActivity>,
    /// What happened.
    pub outcome: LessonOutcome,
    /// Successful Test runs in this lesson so far. Reported effort, not verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tests: Option<u32>,
    /// Which editor the lesson teaches. Absent means `servo`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<ProgrammingMode>,
}

/// Acknowledgement for a recorded lesson event.
///
/// `recorded` is false when the deployment collects no usage at all, which is
/// the default. The client does nothing either way — it is fire-and-forget — but
/// a false here is how an operator checking by hand learns the log is off rather
/// than broken.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LessonEventAck {
    /// Whether a row reached the usage log.
    pub recorded: bool,
}
