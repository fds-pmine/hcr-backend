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
//!    [`ProgrammingMode::default`] returns.
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

use hcr_contract::{ProgramMetrics, ScoreResult, SubmissionResult, TerminalReason};
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
}

/// Serde needs a path, not a closure, to decide whether to skip a field.
fn is_servo(mode: &ProgrammingMode) -> bool {
    mode.is_default()
}

impl UsageEvent {
    /// Build the submission event for a scored result.
    pub fn from_submission(
        ts: u64,
        player_id: Option<String>,
        result: &SubmissionResult,
        match_id: Option<String>,
        session_id: Option<String>,
        mode: ProgrammingMode,
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
    use super::{ProgrammingMode, UsageEvent, UsageLog};
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
            if let UsageEvent::Submission { mode, .. } = row {
                assert_eq!(*mode, ProgrammingMode::Servo);
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
