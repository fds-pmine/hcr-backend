//! Competitive rounds.
//!
//! Mirrors `docs/backend/schema/hcr-v1.d.ts` §6b and the rules in
//! `docs/backend/06-MULTIPLAYER.md`.

use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::domain::ProgramMetrics;

/// Lifecycle of a round.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MatchPhase {
    /// Open for joining; the challenge is not yet known to anyone.
    Lobby,
    /// Start has been called; participants are fixed.
    Countdown,
    /// The challenge is revealed and submissions are accepted.
    Running,
    /// The deadline has passed and results are being assembled.
    Grading,
    /// Final standings published.
    Results,
    /// Abandoned before it ran.
    Cancelled,
}

/// What decides the winner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RankBy {
    /// Voxel IoU against the target — "most similar to the target wins".
    ///
    /// The default, and the stated game rule. Ranking on `finalScore` instead
    /// would fold in efficiency and time at 0.4 weight, quietly rewarding short
    /// programs over accurate haircuts.
    #[default]
    Completion,
    /// The full weighted score.
    Final,
}

/// Round settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchConfig {
    /// Wall-clock length of the round.
    pub duration_ms: u64,
    /// Ranking metric.
    #[serde(default)]
    pub rank_by: RankBy,
    /// Cap on participants.
    pub max_players: usize,
    /// Minimum gap between one player's submissions.
    ///
    /// Scores are hidden during the round, so a player cannot binary-search the
    /// target — but without a cap they could still burn replay capacity for
    /// everyone else.
    pub min_submit_interval_ms: u64,
    /// Pin a specific challenge. When absent the server picks one, and every
    /// participant gets the identical item either way.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge_ref: Option<MatchChallengeRef>,
}

impl Default for MatchConfig {
    fn default() -> Self {
        Self {
            duration_ms: 5 * 60 * 1000,
            rank_by: RankBy::Completion,
            max_players: 16,
            min_submit_interval_ms: 2_000,
            challenge_ref: None,
        }
    }
}

/// A pinned challenge for a round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchChallengeRef {
    /// Which challenge.
    pub challenge_id: String,
    /// Which version.
    pub version: u32,
}

/// A participant.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchPlayer {
    /// Player identity.
    pub player_id: String,
    /// Display name.
    pub display_name: String,
    /// Whether they are currently connected.
    pub connected: bool,
    /// Whether they have submitted at least once.
    pub submitted: bool,
}

/// Public state of a round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchState {
    /// Round identity.
    pub match_id: String,
    /// Lifecycle position.
    pub phase: MatchPhase,
    /// Settings.
    pub config: MatchConfig,
    /// When the round opened, server epoch-ms.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opens_at: Option<u64>,
    /// When it closes, server epoch-ms.
    ///
    /// Authoritative. Clients render a countdown from it, but acceptance is
    /// decided by server receive time — a mis-synced client can be surprised,
    /// never advantaged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closes_at: Option<u64>,
    /// Server clock at the moment this state was produced, for offset estimation.
    pub server_time: u64,
    /// Participants.
    pub players: Vec<MatchPlayer>,
}

/// Why a submission was turned away.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MatchRejection {
    /// Arrived after the deadline, by the server's clock.
    AfterDeadline,
    /// The player is submitting faster than the configured interval.
    RateLimited,
    /// The submitter is not in this round.
    NotParticipant,
    /// The round is not accepting submissions.
    WrongPhase,
    /// The submission scored a different challenge from the round's.
    WrongChallenge,
}

/// Response to a submission during a round.
///
/// Deliberately carries **no score**: revealing standings mid-round would let a
/// player refine against a known bar.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchSubmissionAck {
    /// Which submission.
    pub submission_id: String,
    /// Whether it counts.
    pub accepted: bool,
    /// Server receive time — the value the deadline was judged against.
    pub server_received_at: u64,
    /// Why not, when refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<MatchRejection>,
}

/// One player's standing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchResultRow {
    /// Final placing, 1-based.
    pub rank: u32,
    /// Player identity.
    pub player_id: String,
    /// Display name.
    pub display_name: String,
    /// Similarity to the target.
    pub completion_score: f64,
    /// Weighted score, reported even when ranking on completion.
    pub final_score: f64,
    /// Program size and timing.
    pub metrics: ProgramMetrics,
    /// The submission that counted; absent when the player never submitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub submission_id: Option<String>,
    /// When it arrived, so a disputed deadline is checkable after the fact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_received_at: Option<u64>,
}

/// Final standings.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatchResults {
    /// Round identity.
    pub match_id: String,
    /// The challenge everyone faced.
    pub challenge_id: String,
    /// At which version.
    pub challenge_version: u32,
    /// Metric used.
    pub rank_by: RankBy,
    /// Standings, best first.
    pub rows: Vec<MatchResultRow>,
}

/// Clock-synchronisation exchange.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimeSync {
    /// Echoed back so the client can compute round-trip time.
    pub client_sent_at: u64,
    /// Server clock when the reply was produced.
    pub server_time: u64,
}
