//! Usage collection: an append-only event log.
//!
//! # What this is for
//!
//! Two things at once, which is why it earns its place rather than being
//! bolted-on analytics:
//!
//! 1. **The response data the item bank needs.** `07-CALIBRATION.md` describes
//!    refitting item difficulty from real responses, and until now nothing
//!    persisted any. A `submission` row is exactly an IRT datum — one person,
//!    one item, one outcome — so the log *is* the calibration input.
//! 2. **Knowing whether the thing gets used.** Rounds played, programs written,
//!    where people give up.
//!
//! # What is recorded, and what deliberately is not
//!
//! Each line carries a `playerId`, which is a random identifier the browser
//! generates and keeps in `localStorage` (`features/match/identity.ts`). It is
//! not an account, and the server has never checked it — on a public deployment
//! anyone can claim any value. Treat it as "probably the same browser", nothing
//! stronger.
//!
//! A fourth kind, `lesson`, answers only the second of those. It is
//! client-asserted — the lessons never reach the scorer, so there is nothing to
//! replay them against — and it carries the same identifiers the rest of the log
//! does plus a lesson id and a section index. No free text, and nothing that
//! claims to be a measurement. See [`UsageEvent::Lesson`].
//!
//! Deliberately absent:
//!
//! * **Display names.** Free text a player typed, which is where a real name or
//!   an email would end up. Grouping by `playerId` answers every analytical
//!   question a name would, so the name is not worth the exposure.
//! * **IP addresses.** Never recorded here.
//! * **Program source.** A submitted program is a learner's work; the shape
//!   metrics (`blocks`, `commands`) carry the analysis value without archiving
//!   the thing itself.
//!
//! # Reading logs written by an older build
//!
//! The log is append-only and never rewritten, so a file on a deployed server
//! holds rows from every version that has ever run there. Anything that reads it
//! — the calibration refit above all — must therefore parse rows this build did
//! not write.
//!
//! Two rules keep that possible, and both are load-bearing:
//!
//! 1. **New fields are optional and skipped when absent.** A row written before
//!    a field existed parses with that field defaulted; a row written after it
//!    is unchanged for readers that ignore it. `mode` is the first such field —
//!    an old `submission` row has none, and means `servo`, which is what
//!    [`ProgrammingMode::default`] returns. `scoringDigest` is the second, and
//!    absent means "not recorded", not any particular weighting: see
//!    [`scoring_digest`].
//! 2. **Existing fields never change meaning.** Adding a Cutter Grid mode does
//!    not get to redefine what `commands` counts, because two years of rows
//!    already answer the old question and no migration can reach them.
//!
//! `UsageEvent` derives `Deserialize` purely so this is testable —
//! [`tests::rows_written_before_this_build_still_parse`] parses a fixture of
//! real pre-change lines. Without it "backwards compatible" would be a claim in
//! a comment rather than something CI can fail on.
//!
//! # Failure is not fatal
//!
//! A log that cannot be written must never take the service down with it, so
//! every error is reported once and then swallowed. Losing a row of telemetry is
//! a worse outcome than losing a round only if you value the telemetry more than
//! the users, which would be an odd position for a teaching tool.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use hcr_contract::{
    LessonActivity, LessonEventCreate, LessonOutcome, ProgramMetrics, ScoreResult, ScoringConfig,
    SubmissionResult, TerminalReason,
};
use sha2::{Digest, Sha256};
use serde::{Deserialize, Serialize};

/// Which editor produced the program a row describes.
///
/// Recorded because the two modes are not comparable: a Cutter Grid command is
/// one cell of tool travel and a servo command is one joint move, so pooling
/// them would fit item difficulty against a mixture of two different tasks and
/// call the result one number. SPEC v0.3 §15.1 already says the scores are not
/// to be compared for fairness; this is what lets an analysis honour that.
///
/// Re-exported rather than defined here: it is on the wire now (rounds declare
/// one, sessions declare one, results report one), and a second definition would
/// be free to drift from the contract's.
pub use hcr_contract::ProgrammingMode;

/// One recorded interaction.
///
/// `kind` is the discriminator; every variant carries `ts` (epoch ms) and, where
/// one is known, `playerId`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum UsageEvent {
    /// A program was replayed and scored. The calibration datum.
    #[serde(rename_all = "camelCase")]
    Submission {
        /// Epoch milliseconds.
        ts: u64,
        /// Client-asserted player identifier, when one was supplied.
        #[serde(skip_serializing_if = "Option::is_none")]
        player_id: Option<String>,
        /// Which item.
        challenge_id: String,
        /// At which version — difficulty moves between versions, so a response
        /// is only interpretable against the version it was given.
        challenge_version: u32,
        /// Set when the submission belonged to a round.
        #[serde(skip_serializing_if = "Option::is_none")]
        match_id: Option<String>,
        /// Set when it belonged to an adaptive session.
        #[serde(skip_serializing_if = "Option::is_none")]
        session_id: Option<String>,
        /// Which editor wrote the program.
        ///
        /// Absent on rows written before Cutter Grid reached the backend, where
        /// it is unambiguously `servo` — that was the only mode a submission
        /// could arrive in. Skipped when servo so those rows and these stay byte
        /// for byte identical, which keeps a diff of two log files readable.
        #[serde(default, skip_serializing_if = "is_servo")]
        mode: ProgrammingMode,
        /// How the run ended.
        terminal: TerminalReason,
        /// Similarity to the target — the outcome IRT is fitted on.
        completion_score: f64,
        /// Weighted score.
        final_score: f64,
        /// Program shape.
        blocks: u32,
        /// Atomic commands actually executed.
        commands: u32,
        /// Estimated wall-clock length of the program.
        duration_ms: f64,
        /// Which scoring block produced `final_score`. See [`scoring_digest`].
        ///
        /// Last in the row rather than next to `challenge_version`, which is
        /// where it belongs by meaning: appending it leaves every field written
        /// by an older build at the position it already had, so a diff of two
        /// log files across this deploy shows one new key and not a reordering
        /// of thirteen old ones.
        ///
        /// Absent on rows written before this existed. That means "not
        /// recorded" — not "the current one" — and those rows have to be
        /// resolved against `ts` and the deploy history instead.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scoring_digest: Option<String>,
    },
    /// An adaptive session recorded a response, moving the ability estimate.
    #[serde(rename_all = "camelCase")]
    SessionResponse {
        /// Epoch milliseconds.
        ts: u64,
        /// Which session.
        session_id: String,
        /// Which item was answered.
        challenge_id: String,
        /// Which editor the session is practised in.
        ///
        /// θ is per-mode, so a row without this is a servo row — the only kind
        /// that existed when the field did not.
        #[serde(default, skip_serializing_if = "is_servo")]
        mode: ProgrammingMode,
        /// Raw score before the mastery remap.
        raw_score: f64,
        /// Whether it counted as correct after remapping.
        correct: bool,
        /// Ability estimate afterwards.
        theta: f64,
        /// Standard error afterwards.
        standard_error: f64,
    },
    /// A round closed and published standings.
    #[serde(rename_all = "camelCase")]
    MatchResults {
        /// Epoch milliseconds.
        ts: u64,
        /// Which round.
        match_id: String,
        /// The item everyone faced.
        challenge_id: String,
        /// At which version.
        challenge_version: u32,
        /// Which editor the round was played in. A round is single-mode, so this
        /// applies to every entry in it.
        #[serde(default, skip_serializing_if = "is_servo")]
        mode: ProgrammingMode,
        /// How many took part.
        players: usize,
        /// How many got an attempt in before the deadline.
        submitted: usize,
        /// Best completion score in the round.
        top_completion: f64,
    },
    /// A learner moved through a lesson.
    ///
    /// **Client-asserted, and the only row here that is.** The lessons run and
    /// score in the browser — Cutter Grid is outside server-side scoring by
    /// design (`docs/08-CUTTER-GRID.md` §0), the servo lessons never needed a
    /// server — so nothing about the course reached this log at all: it could
    /// only see submissions, which is the one thing a lesson never does.
    ///
    /// What it answers is purpose 2 above, whether the thing gets used and where
    /// people stop. What it must never be used for is purpose 1: an outcome the
    /// server did not produce is not an IRT datum, and pooling these with
    /// `submission` rows would fit item difficulty against a mixture of measured
    /// and claimed results. Filter on `kind` before any refit.
    #[serde(rename_all = "camelCase")]
    Lesson {
        /// Epoch milliseconds.
        ts: u64,
        /// Client-asserted player identifier, when one was supplied.
        #[serde(skip_serializing_if = "Option::is_none")]
        player_id: Option<String>,
        /// Which editor the lesson teaches. Absent means `servo`, as everywhere.
        #[serde(default, skip_serializing_if = "is_servo")]
        mode: ProgrammingMode,
        /// Which lesson, as the frontend catalogue names it.
        lesson_id: String,
        /// Zero-based section index within that lesson.
        section: u32,
        /// What that section asks for. Absent on whole-lesson outcomes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        activity: Option<LessonActivity>,
        /// What happened.
        outcome: LessonOutcome,
        /// Successful Test runs in the lesson so far, as the client counted them.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tests: Option<u32>,
    },
}

/// Serde needs a path, not a closure, to decide whether to skip a field.
fn is_servo(mode: &ProgrammingMode) -> bool {
    mode.is_default()
}

/// A stable short name for the scoring block a submission was graded against.
///
/// # Why a row needs this
///
/// `challenge_version` was meant to answer "which rules applied to this
/// response", and for the target it does: a new hairstyle is a new version.
/// It does not cover the *scoring block* underneath, because nothing forces the
/// version up when only the weighting changes. That has already happened once
/// on the deployed catalog: `neat-short-cap` moved from a reference cost of
/// 6.25 and a reference time of 5645 ms to 1.25 and 1000 ms, and stayed
/// `challengeVersion: 1`. The log therefore contains rows that claim one
/// version for two different scoring rules, and no field distinguishes them.
///
/// What that does and does not spoil is worth being precise about:
///
/// * `completion_score` is **unaffected**. It compares voxels against the
///   target and never consults the scoring block, which is exactly why
///   `07-CALIBRATION.md` fits difficulty on it. The calibration input survived.
/// * `final_score` is **not comparable** across such a change — it is a
///   weighted blend of completion, efficiency and time — and neither is the
///   adaptive `raw_score`, which is `final_score / 100`. Ability estimates
///   either side of a reweighting are in different units.
///
/// Recording what actually applied puts that boundary in the data instead of in
/// somebody's memory of a deploy.
///
/// # What it is
///
/// The first eight bytes of SHA-256 over the six numbers of the block, each as
/// its IEEE-754 big-endian bytes, in a fixed order. Deliberately not a hash of
/// the serialized JSON: that would also change if a field were renamed or
/// reordered, reporting a scoring change where there was none. Sixteen hex
/// characters distinguish the handful of weightings a catalog ever has; it is
/// an identity, not a security primitive, so truncation costs nothing here.
///
/// The digest of a block is therefore stable across builds, and two rows share
/// one exactly when they were scored by the same numbers.
pub fn scoring_digest(scoring: &ScoringConfig) -> String {
    let mut hasher = Sha256::new();
    for value in [
        scoring.weights.completion,
        scoring.weights.efficiency,
        scoring.weights.time,
        scoring.reference_program_cost,
        scoring.reference_time_ms,
        scoring.command_weight,
    ] {
        hasher.update(value.to_be_bytes());
    }

    hasher
        .finalize()
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl UsageEvent {
    /// Build the lesson event for one reported interaction.
    ///
    /// The request is taken whole because every field of it is recorded: there
    /// is no server-side derivation to do, which is exactly what makes this row
    /// client-asserted.
    pub fn from_lesson_event(
        ts: u64,
        player_id: Option<String>,
        request: LessonEventCreate,
    ) -> Self {
        let LessonEventCreate {
            lesson_id,
            section,
            activity,
            outcome,
            tests,
            mode,
        } = request;

        UsageEvent::Lesson {
            ts,
            player_id,
            mode: mode.unwrap_or_default(),
            lesson_id,
            section,
            activity,
            outcome,
            tests,
        }
    }

    /// Build the submission event for a scored result.
    ///
    /// Takes the scoring block as well as the result because the result does
    /// not carry it: the scores are in there, the rules that produced them are
    /// not. See [`scoring_digest`].
    pub fn from_submission(
        ts: u64,
        player_id: Option<String>,
        result: &SubmissionResult,
        match_id: Option<String>,
        session_id: Option<String>,
        mode: ProgrammingMode,
        scoring: &ScoringConfig,
    ) -> Self {
        let ScoreResult {
            completion_score,
            final_score,
            ..
        } = result.score;
        let ProgramMetrics {
            source_block_count,
            executed_command_count,
            estimated_duration_ms,
        } = result.metrics;

        UsageEvent::Submission {
            ts,
            player_id,
            challenge_id: result.challenge_id.clone(),
            challenge_version: result.challenge_version,
            match_id,
            session_id,
            mode,
            terminal: result.terminal.reason,
            completion_score,
            final_score,
            blocks: source_block_count,
            commands: executed_command_count,
            duration_ms: estimated_duration_ms,
            scoring_digest: Some(scoring_digest(scoring)),
        }
    }
}

/// Identity of the file actually open, so rotation can be noticed.
///
/// A path is not an identity. `rename` moves the name and leaves the inode
/// where it was, which is why a held descriptor keeps writing into the archive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileId {
    device: u64,
    inode: u64,
}

#[cfg(unix)]
fn identify(metadata: &std::fs::Metadata) -> Option<FileId> {
    use std::os::unix::fs::MetadataExt;
    Some(FileId {
        device: metadata.dev(),
        inode: metadata.ino(),
    })
}

/// Rotation detection is a Unix concept here; elsewhere the descriptor is
/// simply kept, which is the previous behaviour.
#[cfg(not(unix))]
fn identify(_metadata: &std::fs::Metadata) -> Option<FileId> {
    None
}

#[derive(Debug)]
struct Sink {
    file: File,
    id: Option<FileId>,
}

impl Sink {
    fn open(path: &Path) -> std::io::Result<Self> {
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let id = file.metadata().ok().and_then(|meta| identify(&meta));
        Ok(Self { file, id })
    }

    /// Reopen if the descriptor no longer refers to what `path` names.
    ///
    /// Costs one `stat` per event. Events are one per scored submission — a
    /// replay that already cost milliseconds of CPU — so the syscall does not
    /// register, and it removes an entire class of silent data loss.
    fn ensure_current(&mut self, path: &Path) -> std::io::Result<bool> {
        // No identity to compare (non-Unix): keep what we have.
        if self.id.is_none() {
            return Ok(false);
        }
        let current = std::fs::metadata(path)
            .ok()
            .and_then(|meta| identify(&meta));
        // `None` means the path is gone — rotated away without a replacement.
        // Reopening recreates it, which is what the operator expects to find.
        if current.is_some() && current == self.id {
            return Ok(false);
        }
        *self = Sink::open(path)?;
        Ok(true)
    }
}

/// An append-only JSONL sink.
///
/// One object per line. No rotation of its own — point `logrotate` at the file,
/// which is what a VPS already has and does better. Every line carries `ts`, so
/// bucketing by day is a property of the data rather than of the filename.
///
/// # Surviving rotation
///
/// The descriptor is checked against the path before every write and reopened
/// if they have parted company.
///
/// Without that, a rotation that *renames* the live file — which is what
/// logrotate does unless told `copytruncate` — leaves this process appending to
/// the renamed inode forever. Nothing fails: the writes succeed, the archive
/// grows, and the file at the configured path stays zero bytes. The only
/// symptom is an empty log, discovered whenever somebody next goes looking, and
/// the fix is a restart because a descriptor cannot be moved.
///
/// Depending on `copytruncate` to prevent that means depending on a config file
/// this program does not own and cannot check. Noticing instead costs one
/// `stat` per submission and works under every rotation scheme, including none.
#[derive(Debug)]
pub struct UsageLog {
    sink: Mutex<Sink>,
    path: PathBuf,
    /// Set after the first write failure, so a broken disk logs once, not once
    /// per request.
    reported: AtomicBool,
}

impl UsageLog {
    /// Open (or create) a log for appending.
    pub fn open(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Ok(Self {
            sink: Mutex::new(Sink::open(&path)?),
            path,
            reported: AtomicBool::new(false),
        })
    }

    /// Where this log is being written.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Append one event.
    ///
    /// Serializes and writes in a single `write_all` so a line is never
    /// interleaved with another writer's; the file is opened `O_APPEND`, which
    /// is what makes that atomic for writes of this size.
    pub fn record(&self, event: &UsageEvent) {
        let mut line = match serde_json::to_vec(event) {
            Ok(line) => line,
            Err(error) => return self.report(&format!("could not encode event: {error}")),
        };
        line.push(b'\n');

        let result = self
            .sink
            .lock()
            .map_err(|_| std::io::Error::other("usage log poisoned"))
            .and_then(|mut sink| {
                let reopened = sink.ensure_current(&self.path)?;
                sink.file.write_all(&line)?;
                Ok(reopened)
            });

        match result {
            // Worth one line each time: it is the only evidence that rotation
            // happened, and its absence over a long run is how you learn the
            // detection is not firing.
            Ok(true) => eprintln!(
                "usage log ({}): file was rotated, reopened it.",
                self.path.display()
            ),
            Ok(false) => {}
            Err(error) => self.report(&format!("could not write: {error}")),
        }
    }

    fn report(&self, message: &str) {
        if !self.reported.swap(true, Ordering::Relaxed) {
            eprintln!(
                "usage log ({}): {message}. Further errors will be silent; \
                 the service continues without collecting usage.",
                self.path.display()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{ProgrammingMode, ScoringConfig, UsageEvent, UsageLog, scoring_digest};
    use std::path::PathBuf;

    /// Rows captured from the format written before Cutter Grid existed.
    const LEGACY_ROWS: &str = include_str!("../tests/fixtures/usage-legacy.jsonl");

    /// Every legacy row still parses, and means what it meant.
    ///
    /// The calibration refit reads years of accumulated rows, so a schema change
    /// that quietly stops parsing the old ones does not fail loudly — it fits
    /// item difficulty against a truncated history and produces plausible,
    /// wrong parameters.
    #[test]
    fn rows_written_before_this_build_still_parse() {
        let rows: Vec<UsageEvent> = LEGACY_ROWS
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| {
                serde_json::from_str(line).unwrap_or_else(|error| {
                    panic!("legacy row no longer parses: {error}\n  {line}")
                })
            })
            .collect();

        assert_eq!(rows.len(), 5, "fixture should cover all three event kinds");

        let submissions = rows
            .iter()
            .filter(|row| matches!(row, UsageEvent::Submission { .. }))
            .count();
        assert_eq!(submissions, 3);

        // A row with no `mode` is a servo row. Defaulting it to anything else
        // would silently relabel the entire history.
        for row in &rows {
            if let UsageEvent::Submission {
                mode,
                scoring_digest,
                ..
            } = row
            {
                assert_eq!(*mode, ProgrammingMode::Servo);
                // An absent digest must stay absent. Defaulting it to the
                // scoring block this build happens to ship would assert that
                // every historical row was graded by today's weighting, which
                // for `neat-short-cap` is false for everything written before
                // 2026-08-27 — and it would be an assertion nothing could
                // later detect as wrong.
                assert_eq!(*scoring_digest, None, "{row:?}");
            }
        }
    }

    /// A servo row is byte-identical to what the previous build wrote.
    ///
    /// `mode` is skipped when servo precisely so this holds: an operator
    /// diffing two log files across a deploy should see new rows, not a
    /// reformatting of every old one.
    #[test]
    fn a_servo_row_gains_no_new_fields() {
        let legacy = LEGACY_ROWS.lines().next().expect("a first row");
        let parsed: UsageEvent = serde_json::from_str(legacy).expect("parses");
        let reserialized = serde_json::to_string(&parsed).expect("serializes");

        let before: serde_json::Value = serde_json::from_str(legacy).unwrap();
        let after: serde_json::Value = serde_json::from_str(&reserialized).unwrap();
        assert_eq!(before, after, "round-tripping a legacy row changed it");
        assert!(
            !reserialized.contains("\"mode\""),
            "servo rows must not carry a mode: {reserialized}"
        );
    }

    /// The digest separates exactly the change `challengeVersion` missed.
    ///
    /// These two blocks are the real ones: `neat-short-cap` shipped the first
    /// until 2026-08-27 and the second after, both under `challengeVersion: 1`.
    /// A reader holding rows from either side has nothing else to tell them
    /// apart.
    #[test]
    fn a_reweighting_under_one_version_gets_two_digests() {
        let before = scoring_config(6.25, 5_645.0);
        let after = scoring_config(1.25, 1_000.0);

        assert_ne!(
            scoring_digest(&before),
            scoring_digest(&after),
            "the 2026-08-27 reweighting must not be invisible"
        );

        // Stable, or two rows scored by identical rules would not group.
        assert_eq!(scoring_digest(&before), scoring_digest(&before));
        assert_eq!(scoring_digest(&after), scoring_digest(&scoring_config(1.25, 1_000.0)));

        // A name, not a number: 16 hex characters, so it survives being pasted
        // into a query or a note.
        let digest = scoring_digest(&after);
        assert_eq!(digest.len(), 16, "{digest}");
        assert!(digest.chars().all(|c| c.is_ascii_hexdigit()), "{digest}");
    }

    /// A new row is an old row plus one trailing key.
    ///
    /// The promise made to anyone diffing two log files across a deploy, and to
    /// anyone reading a row by eye: fields that existed before keep the
    /// position they had. Appending is what makes that true, so the order is
    /// worth pinning rather than leaving to the order of a struct nobody
    /// remembers is load-bearing.
    #[test]
    fn a_new_field_lands_at_the_end_and_moves_nothing() {
        let legacy = LEGACY_ROWS.lines().next().expect("a first row");
        let mut parsed: UsageEvent = serde_json::from_str(legacy).expect("parses");
        if let UsageEvent::Submission { scoring_digest, .. } = &mut parsed {
            *scoring_digest = Some(scoring_digest_of_the_current_block());
        }

        let line = serde_json::to_string(&parsed).expect("serializes");
        let (head, tail) = line
            .split_once(r#","scoringDigest""#)
            .unwrap_or_else(|| panic!("digest should be the last key: {line}"));

        assert_eq!(
            format!("{head}}}"),
            legacy,
            "everything before the new key must be the legacy row, byte for byte"
        );
        assert_eq!(tail, format!(r#":"{}"}}"#, scoring_digest_of_the_current_block()));
    }

    fn scoring_digest_of_the_current_block() -> String {
        scoring_digest(&scoring_config(1.25, 1_000.0))
    }

    /// The two digests the deployed log actually needs, pinned.
    ///
    /// A digest is only useful if it is the *same* digest next year: change how
    /// it is computed and one era of rows splits into two that no longer group,
    /// with nothing to say which is which. Pinning the literals makes that a
    /// test failure rather than a discovery.
    ///
    /// Both values are documented in `docs/DEPLOY.md` §3, which is where an
    /// analysis looks them up.
    #[test]
    fn the_shipped_digests_are_the_ones_the_docs_publish() {
        assert_eq!(
            scoring_digest(&scoring_config(6.25, 5_645.0)),
            "2081283affc7cc7d",
            "neat-short-cap as scored before 2026-08-27"
        );
        assert_eq!(
            scoring_digest(&scoring_config(1.25, 1_000.0)),
            "448b870304930bb7",
            "neat-short-cap as scored now"
        );
    }

    /// Every component is part of the identity.
    ///
    /// A digest that ignored one of them would report "same rules" across a
    /// change that moves every score, which is worse than no digest: it would
    /// be believed.
    #[test]
    fn every_number_in_the_block_moves_the_digest() {
        let base = scoring_config(1.25, 1_000.0);
        let baseline = scoring_digest(&base);

        let mut heavier_completion = base.clone();
        heavier_completion.weights.completion = 0.7;
        heavier_completion.weights.efficiency = 0.15;
        assert_ne!(scoring_digest(&heavier_completion), baseline);

        let mut dearer_commands = base.clone();
        dearer_commands.command_weight = 0.5;
        assert_ne!(scoring_digest(&dearer_commands), baseline);

        assert_ne!(scoring_digest(&scoring_config(1.25, 999.0)), baseline);
        assert_ne!(scoring_digest(&scoring_config(1.3, 1_000.0)), baseline);
    }

    fn scoring_config(reference_program_cost: f64, reference_time_ms: f64) -> ScoringConfig {
        ScoringConfig {
            weights: hcr_contract::ScoreWeights {
                completion: 0.6,
                efficiency: 0.25,
                time: 0.15,
            },
            reference_program_cost,
            reference_time_ms,
            command_weight: 0.25,
        }
    }

    /// A Cutter Grid row does carry the discriminator.
    #[test]
    fn a_cutter_grid_row_records_its_mode() {
        let event = UsageEvent::Submission {
            ts: 1,
            player_id: None,
            challenge_id: "neat-short-cap".to_string(),
            challenge_version: 1,
            match_id: None,
            session_id: None,
            mode: ProgrammingMode::CutterGrid,
            terminal: hcr_contract::TerminalReason::Completed,
            completion_score: 100.0,
            final_score: 100.0,
            blocks: 5,
            commands: 22,
            duration_ms: 12_348.0,
            scoring_digest: Some("0123456789abcdef".to_string()),
        };
        let line = serde_json::to_string(&event).expect("serializes");
        assert!(line.contains(r#""mode":"cutter-grid""#), "{line}");
    }

    /// A reader built against the old schema still reads new rows.
    ///
    /// This is the other half of compatibility and the half that is easy to
    /// forget: the log outlives whatever is reading it, and an analysis script
    /// written last year must not choke on a field added this year. Serde
    /// ignores unknown fields by default, which is exactly what the contract
    /// requires of every receiver (`docs/01-CONTRACT.md` §1).
    #[test]
    fn a_reader_that_predates_mode_still_reads_new_rows() {
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct LegacySubmissionRow {
            challenge_id: String,
            completion_score: f64,
            commands: u32,
        }

        let event = UsageEvent::Submission {
            ts: 1,
            player_id: None,
            challenge_id: "neat-short-cap".to_string(),
            challenge_version: 1,
            match_id: None,
            session_id: None,
            mode: ProgrammingMode::CutterGrid,
            terminal: hcr_contract::TerminalReason::Completed,
            completion_score: 100.0,
            final_score: 100.0,
            blocks: 5,
            commands: 22,
            duration_ms: 12_348.0,
            scoring_digest: Some("0123456789abcdef".to_string()),
        };
        let line = serde_json::to_string(&event).expect("serializes");

        let row: LegacySubmissionRow = serde_json::from_str(&line).expect("old reader copes");
        assert_eq!(row.challenge_id, "neat-short-cap");
        assert_eq!(row.completion_score, 100.0);
        assert_eq!(row.commands, 22);
    }

    fn event(ts: u64) -> UsageEvent {
        UsageEvent::MatchResults {
            ts,
            match_id: format!("m{ts}"),
            challenge_id: "neat-short-cap".to_string(),
            challenge_version: 1,
            mode: ProgrammingMode::Servo,
            players: 1,
            submitted: 1,
            top_completion: 100.0,
        }
    }

    /// A directory of our own, so a failing test cannot take another's data
    /// with it. No `tempfile` dependency for three tests.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hcr-usage-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("scratch dir");
        dir
    }

    fn lines(path: &std::path::Path) -> usize {
        std::fs::read_to_string(path)
            .map(|text| text.lines().count())
            .unwrap_or(0)
    }

    #[test]
    fn appends_one_line_per_event() {
        let dir = scratch("append");
        let path = dir.join("usage.jsonl");
        let log = UsageLog::open(&path).expect("open");
        log.record(&event(1));
        log.record(&event(2));
        assert_eq!(lines(&path), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A row is readable by anything else the moment `record` returns.
    ///
    /// Worth pinning down, because "the log looked empty" is the symptom of a
    /// held descriptor pointing at a rotated file, and it would be easy to
    /// misdiagnose that as buffering and reach for a flush that changes
    /// nothing. `File` is unbuffered — there is no userspace buffer to flush —
    /// so a `tail -f` on the server sees each row as it is written, without the
    /// process closing the file or exiting.
    #[test]
    fn each_row_is_visible_before_the_next_is_written() {
        let dir = scratch("immediate");
        let path = dir.join("usage.jsonl");
        let log = UsageLog::open(&path).expect("open");

        log.record(&event(1));
        assert_eq!(lines(&path), 1, "first row not visible while still open");

        log.record(&event(2));
        assert_eq!(lines(&path), 2, "second row not visible while still open");

        // Still open, never flushed, never dropped.
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn follows_the_path_when_rotation_renames_the_file() {
        let dir = scratch("rename");
        let path = dir.join("usage.jsonl");
        let rotated = dir.join("usage.jsonl.1");

        let log = UsageLog::open(&path).expect("open");
        log.record(&event(1));

        // Exactly what logrotate does without `copytruncate`.
        std::fs::rename(&path, &rotated).expect("rotate");
        log.record(&event(2));

        // The archive keeps only what preceded rotation, and the live path has
        // the rest. Before the descriptor check both lines landed in `.1` and
        // `usage.jsonl` did not exist at all.
        assert_eq!(lines(&rotated), 1, "archive should not have grown");
        assert_eq!(lines(&path), 1, "live path should have the later event");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn keeps_writing_through_copytruncate() {
        let dir = scratch("truncate");
        let path = dir.join("usage.jsonl");

        let log = UsageLog::open(&path).expect("open");
        log.record(&event(1));

        // `copytruncate` preserves the inode, so nothing should be reopened —
        // and `O_APPEND` resumes at the new end, which is zero.
        std::fs::copy(&path, dir.join("usage.jsonl.1")).expect("copy");
        std::fs::OpenOptions::new()
            .write(true)
            .open(&path)
            .expect("truncate")
            .set_len(0)
            .expect("truncate");

        log.record(&event(2));
        assert_eq!(lines(&path), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn recreates_a_log_that_was_deleted_outright() {
        let dir = scratch("unlink");
        let path = dir.join("usage.jsonl");

        let log = UsageLog::open(&path).expect("open");
        log.record(&event(1));
        std::fs::remove_file(&path).expect("unlink");

        log.record(&event(2));
        assert_eq!(lines(&path), 1, "should have recreated and written");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
