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
use serde::Serialize;

/// One recorded interaction.
///
/// `kind` is the discriminator; every variant carries `ts` (epoch ms) and, where
/// one is known, `playerId`.
#[derive(Debug, Clone, Serialize)]
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
        /// How many took part.
        players: usize,
        /// How many got an attempt in before the deadline.
        submitted: usize,
        /// Best completion score in the round.
        top_completion: f64,
    },
}

impl UsageEvent {
    /// Build the submission event for a scored result.
    pub fn from_submission(
        ts: u64,
        player_id: Option<String>,
        result: &SubmissionResult,
        match_id: Option<String>,
        session_id: Option<String>,
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
            terminal: result.terminal.reason,
            completion_score,
            final_score,
            blocks: source_block_count,
            commands: executed_command_count,
            duration_ms: estimated_duration_ms,
        }
    }
}

/// An append-only JSONL sink.
///
/// One object per line. No rotation of its own — point `logrotate` at the file,
/// which is what a VPS already has and does better. Every line carries `ts`, so
/// bucketing by day is a property of the data rather than of the filename.
#[derive(Debug)]
pub struct UsageLog {
    file: Mutex<File>,
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
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        Ok(Self {
            file: Mutex::new(file),
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
            .file
            .lock()
            .map_err(|_| std::io::Error::other("usage log poisoned"))
            .and_then(|mut file| file.write_all(&line));

        if let Err(error) = result {
            self.report(&format!("could not write: {error}"));
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
