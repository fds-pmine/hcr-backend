//! Competitive rounds.
//!
//! Implements `docs/backend/06-MULTIPLAYER.md`: all players start together, get
//! the identical challenge, and have a fixed wall-clock window to submit. Highest
//! similarity to the target wins.
//!
//! The fairness properties are the design, not decoration:
//!
//! * The **server clock** decides acceptance. A client's own timestamp is never
//!   consulted, so a tampered clock buys nothing.
//! * **Scores stay hidden** until the round closes, so nobody can submit, read
//!   their standing, and refine against a known bar.
//! * **Resubmission is unlimited and the best attempt counts**, which is the
//!   fairest treatment of a lag spike or a bad first idea.
//! * The **challenge is not revealed before the start**, so nobody gets a head
//!   start.

use std::collections::HashMap;
use std::sync::Mutex;

use hcr_contract::{
    MatchChallengeRef, MatchConfig, MatchPhase, MatchPlayer, MatchRejection, MatchResultRow,
    MatchResults, MatchState, MatchSubmissionAck, ProgramMetrics, RankBy, SubmissionResult,
};

use crate::clock::SharedClock;
use crate::error::{ServiceError, ServiceResult};

/// One player's best accepted attempt.
#[derive(Debug, Clone)]
struct Entry {
    submission_id: String,
    completion_score: f64,
    final_score: f64,
    efficiency_score: f64,
    metrics: ProgramMetrics,
    server_received_at: u64,
}

impl Entry {
    fn ranking_score(&self, rank_by: RankBy) -> f64 {
        match rank_by {
            RankBy::Completion => self.completion_score,
            RankBy::Final => self.final_score,
        }
    }

    /// Whether `self` outranks `other`, applying the published tie-break.
    fn beats(&self, other: &Entry, rank_by: RankBy) -> bool {
        let (mine, theirs) = (self.ranking_score(rank_by), other.ranking_score(rank_by));
        if mine != theirs {
            return mine > theirs;
        }
        if self.efficiency_score != other.efficiency_score {
            return self.efficiency_score > other.efficiency_score;
        }
        if self.metrics.estimated_duration_ms != other.metrics.estimated_duration_ms {
            return self.metrics.estimated_duration_ms < other.metrics.estimated_duration_ms;
        }
        self.server_received_at < other.server_received_at
    }
}

#[derive(Debug)]
struct Room {
    state: MatchState,
    challenge: MatchChallengeRef,
    players: HashMap<String, MatchPlayer>,
    /// Player -> best accepted attempt.
    entries: HashMap<String, Entry>,
    last_submit_at: HashMap<String, u64>,
}

impl Room {
    fn public_state(&self, now: u64) -> MatchState {
        let mut players: Vec<MatchPlayer> = self.players.values().cloned().collect();
        // HashMap order is unspecified; keep listings reproducible.
        players.sort_by(|a, b| a.player_id.cmp(&b.player_id));

        MatchState {
            players,
            server_time: now,
            ..self.state.clone()
        }
    }

    /// Advance out of `Running` once the deadline has passed.
    fn settle(&mut self, now: u64) {
        if self.state.phase == MatchPhase::Running
            && self.state.closes_at.is_some_and(|closes| now >= closes)
        {
            // Submissions were replayed as they arrived, so grading is a sort,
            // not a computation — the round moves straight to results.
            self.state.phase = MatchPhase::Results;
        }
    }
}

/// Tracks live rounds.
#[derive(Debug)]
pub struct MatchRegistry {
    rooms: Mutex<HashMap<String, Room>>,
    clock: SharedClock,
    counter: Mutex<u64>,
}

impl MatchRegistry {
    /// Build a registry over a clock.
    pub fn new(clock: SharedClock) -> Self {
        Self {
            rooms: Mutex::new(HashMap::new()),
            clock,
            counter: Mutex::new(0),
        }
    }

    fn lock(&self) -> ServiceResult<std::sync::MutexGuard<'_, HashMap<String, Room>>> {
        self.rooms
            .lock()
            .map_err(|_| ServiceError::Internal("match registry poisoned"))
    }

    fn next_id(&self) -> String {
        let mut counter = self.counter.lock().unwrap_or_else(|p| p.into_inner());
        *counter += 1;
        format!("m-{:012x}", *counter)
    }

    /// Open a lobby.
    pub fn create(
        &self,
        config: MatchConfig,
        challenge: MatchChallengeRef,
    ) -> ServiceResult<MatchState> {
        let now = self.clock.now_ms();
        let match_id = self.next_id();

        let room = Room {
            state: MatchState {
                match_id: match_id.clone(),
                phase: MatchPhase::Lobby,
                config,
                opens_at: None,
                closes_at: None,
                server_time: now,
                players: Vec::new(),
            },
            challenge,
            players: HashMap::new(),
            entries: HashMap::new(),
            last_submit_at: HashMap::new(),
        };

        let state = room.public_state(now);
        self.lock()?.insert(match_id, room);
        Ok(state)
    }

    /// Join a lobby.
    pub fn join(
        &self,
        match_id: &str,
        player_id: &str,
        display_name: &str,
    ) -> ServiceResult<MatchState> {
        let now = self.clock.now_ms();
        let mut rooms = self.lock()?;
        let room = rooms
            .get_mut(match_id)
            .ok_or_else(|| ServiceError::SessionNotFound(match_id.to_string()))?;

        if room.state.phase != MatchPhase::Lobby {
            return Err(ServiceError::SessionTerminated);
        }
        if !room.players.contains_key(player_id) && room.players.len() >= room.state.config.max_players
        {
            return Err(ServiceError::RateLimited);
        }

        room.players.insert(
            player_id.to_string(),
            MatchPlayer {
                player_id: player_id.to_string(),
                display_name: display_name.to_string(),
                connected: true,
                submitted: false,
            },
        );
        Ok(room.public_state(now))
    }

    /// Start the round: fix the roster, set the deadline, reveal the challenge.
    pub fn start(&self, match_id: &str) -> ServiceResult<MatchState> {
        let now = self.clock.now_ms();
        let mut rooms = self.lock()?;
        let room = rooms
            .get_mut(match_id)
            .ok_or_else(|| ServiceError::SessionNotFound(match_id.to_string()))?;

        if room.state.phase != MatchPhase::Lobby {
            return Err(ServiceError::SessionTerminated);
        }

        room.state.phase = MatchPhase::Running;
        room.state.opens_at = Some(now);
        room.state.closes_at = Some(now + room.state.config.duration_ms);
        Ok(room.public_state(now))
    }

    /// Current state, settling the phase if the deadline has passed.
    pub fn state(&self, match_id: &str) -> ServiceResult<MatchState> {
        let now = self.clock.now_ms();
        let mut rooms = self.lock()?;
        let room = rooms
            .get_mut(match_id)
            .ok_or_else(|| ServiceError::SessionNotFound(match_id.to_string()))?;
        room.settle(now);
        Ok(room.public_state(now))
    }

    /// Which challenge the round uses.
    ///
    /// Refused before the start: revealing it during the lobby would hand an
    /// early joiner a head start.
    pub fn challenge_ref(&self, match_id: &str) -> ServiceResult<MatchChallengeRef> {
        let now = self.clock.now_ms();
        let mut rooms = self.lock()?;
        let room = rooms
            .get_mut(match_id)
            .ok_or_else(|| ServiceError::SessionNotFound(match_id.to_string()))?;
        room.settle(now);

        if room.state.phase == MatchPhase::Lobby {
            return Err(ServiceError::ItemRefInvalid(
                "the challenge is not revealed until the round starts",
            ));
        }
        Ok(room.challenge.clone())
    }

    /// Record a scored submission against a round.
    ///
    /// The reply carries no score. Acceptance is judged purely on server receive
    /// time against the deadline.
    pub fn submit(
        &self,
        match_id: &str,
        player_id: &str,
        result: &SubmissionResult,
    ) -> ServiceResult<MatchSubmissionAck> {
        let now = self.clock.now_ms();
        let mut rooms = self.lock()?;
        let room = rooms
            .get_mut(match_id)
            .ok_or_else(|| ServiceError::SessionNotFound(match_id.to_string()))?;
        room.settle(now);

        let refuse = |reason: MatchRejection| {
            Ok(MatchSubmissionAck {
                submission_id: result.submission_id.clone(),
                accepted: false,
                server_received_at: now,
                rejected_reason: Some(reason),
            })
        };

        if !room.players.contains_key(player_id) {
            return refuse(MatchRejection::NotParticipant);
        }

        // Order matters. `settle` has already advanced a closed round out of
        // `Running`, so a plain phase check would tell a player who missed the
        // deadline by a millisecond that they were in the "wrong phase" — true,
        // but useless. Test the deadline first so they learn what actually
        // happened.
        if matches!(
            room.state.phase,
            MatchPhase::Lobby | MatchPhase::Countdown | MatchPhase::Cancelled
        ) {
            return refuse(MatchRejection::WrongPhase);
        }
        if room.state.closes_at.is_some_and(|closes| now >= closes) {
            return refuse(MatchRejection::AfterDeadline);
        }
        if room.state.phase != MatchPhase::Running {
            return refuse(MatchRejection::WrongPhase);
        }
        if result.challenge_id != room.challenge.challenge_id
            || result.challenge_version != room.challenge.version
        {
            return refuse(MatchRejection::WrongChallenge);
        }
        if let Some(previous) = room.last_submit_at.get(player_id) {
            if now.saturating_sub(*previous) < room.state.config.min_submit_interval_ms {
                return refuse(MatchRejection::RateLimited);
            }
        }

        room.last_submit_at.insert(player_id.to_string(), now);

        let entry = Entry {
            submission_id: result.submission_id.clone(),
            completion_score: result.score.completion_score,
            final_score: result.score.final_score,
            efficiency_score: result.score.efficiency_score,
            metrics: result.metrics,
            server_received_at: now,
        };

        let rank_by = room.state.config.rank_by;
        let keep = match room.entries.get(player_id) {
            None => true,
            Some(existing) => entry.beats(existing, rank_by),
        };
        if keep {
            room.entries.insert(player_id.to_string(), entry);
        }
        if let Some(player) = room.players.get_mut(player_id) {
            player.submitted = true;
        }

        Ok(MatchSubmissionAck {
            submission_id: result.submission_id.clone(),
            accepted: true,
            server_received_at: now,
            rejected_reason: None,
        })
    }

    /// Final standings.
    ///
    /// Refused while the round is running: publishing early is the whole thing
    /// the hidden-score rule exists to prevent.
    pub fn results(&self, match_id: &str) -> ServiceResult<MatchResults> {
        let now = self.clock.now_ms();
        let mut rooms = self.lock()?;
        let room = rooms
            .get_mut(match_id)
            .ok_or_else(|| ServiceError::SessionNotFound(match_id.to_string()))?;
        room.settle(now);

        if room.state.phase != MatchPhase::Results {
            return Err(ServiceError::SessionNotAwaitingResponse);
        }

        let rank_by = room.state.config.rank_by;
        let mut rows: Vec<MatchResultRow> = room
            .players
            .values()
            .map(|player| match room.entries.get(&player.player_id) {
                Some(entry) => MatchResultRow {
                    rank: 0,
                    player_id: player.player_id.clone(),
                    display_name: player.display_name.clone(),
                    completion_score: entry.completion_score,
                    final_score: entry.final_score,
                    metrics: entry.metrics,
                    submission_id: Some(entry.submission_id.clone()),
                    server_received_at: Some(entry.server_received_at),
                },
                // Ranked last rather than omitted: dropping them would hide that
                // they took part at all.
                None => MatchResultRow {
                    rank: 0,
                    player_id: player.player_id.clone(),
                    display_name: player.display_name.clone(),
                    completion_score: 0.0,
                    final_score: 0.0,
                    metrics: ProgramMetrics::default(),
                    submission_id: None,
                    server_received_at: None,
                },
            })
            .collect();

        rows.sort_by(|a, b| {
            let entry_of = |row: &MatchResultRow| room.entries.get(&row.player_id).cloned();
            match (entry_of(a), entry_of(b)) {
                (Some(left), Some(right)) => {
                    if left.beats(&right, rank_by) {
                        std::cmp::Ordering::Less
                    } else if right.beats(&left, rank_by) {
                        std::cmp::Ordering::Greater
                    } else {
                        a.player_id.cmp(&b.player_id)
                    }
                }
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.player_id.cmp(&b.player_id),
            }
        });

        for (index, row) in rows.iter_mut().enumerate() {
            row.rank = index as u32 + 1;
        }

        Ok(MatchResults {
            match_id: match_id.to_string(),
            challenge_id: room.challenge.challenge_id.clone(),
            challenge_version: room.challenge.version,
            rank_by,
            rows,
        })
    }

    /// Abandon a round that never ran.
    pub fn cancel(&self, match_id: &str) -> ServiceResult<MatchState> {
        let now = self.clock.now_ms();
        let mut rooms = self.lock()?;
        let room = rooms
            .get_mut(match_id)
            .ok_or_else(|| ServiceError::SessionNotFound(match_id.to_string()))?;
        room.state.phase = MatchPhase::Cancelled;
        Ok(room.public_state(now))
    }

    /// Live round count.
    pub fn len(&self) -> usize {
        self.rooms.lock().map(|r| r.len()).unwrap_or(0)
    }

    /// Whether no rounds exist.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
