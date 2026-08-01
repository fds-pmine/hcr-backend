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
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::sync::Mutex;

use hmac::{Hmac, Mac};
use sha2::Sha256;

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
    /// Server time of the last operation on this room, for eviction.
    last_seen: u64,
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
    ///
    /// Also records the touch used by eviction: every read and write path calls
    /// this, so a room stays alive exactly as long as somebody is looking at it.
    fn settle(&mut self, now: u64) {
        self.last_seen = self.last_seen.max(now);

        if self.state.phase == MatchPhase::Running
            && self.state.closes_at.is_some_and(|closes| now >= closes)
        {
            // Submissions were replayed as they arrived, so grading is a sort,
            // not a computation — the round moves straight to results.
            self.state.phase = MatchPhase::Results;
        }
    }
}

/// Characters a room code may contain.
///
/// Base32 without `I`, `O`, `0` or `1`, because a code's job is to survive being
/// read out loud and typed back in.
const CODE_ALPHABET: &[u8; 32] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";

/// Length of a room code. 32⁶ ≈ 1.07 × 10⁹.
const CODE_LEN: usize = 6;

/// Tracks live rounds.
#[derive(Debug)]
pub struct MatchRegistry {
    rooms: Mutex<HashMap<String, Room>>,
    clock: SharedClock,
    counter: Mutex<u64>,
    /// Keys the room-code derivation. See [`MatchRegistry::derive_code`].
    code_key: [u8; 32],
}

impl MatchRegistry {
    /// Build a registry over a clock.
    pub fn new(clock: SharedClock) -> Self {
        Self {
            rooms: Mutex::new(HashMap::new()),
            clock,
            counter: Mutex::new(0),
            code_key: random_key(),
        }
    }

    fn lock(&self) -> ServiceResult<std::sync::MutexGuard<'_, HashMap<String, Room>>> {
        self.rooms
            .lock()
            .map_err(|_| ServiceError::Internal("match registry poisoned"))
    }

    /// Turn a counter value into a room code.
    ///
    /// The counter alone would be a fine identifier and a poor code. A room's id
    /// **is** the capability to join it — nothing else gates entering a lobby —
    /// so a sequential id lets anyone walk into somebody else's round by
    /// counting, and tells them how many rounds the server has ever opened.
    /// HMAC under a per-process key removes both without giving up the counter's
    /// guarantee that inputs never repeat.
    fn derive_code(&self, counter: u64) -> String {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(&self.code_key)
            .expect("HMAC accepts a 32-byte key");
        mac.update(&counter.to_be_bytes());
        let digest = mac.finalize().into_bytes();

        digest
            .iter()
            .take(CODE_LEN)
            .map(|byte| CODE_ALPHABET[usize::from(*byte) % CODE_ALPHABET.len()] as char)
            .collect()
    }

    /// Open a lobby.
    pub fn create(
        &self,
        config: MatchConfig,
        challenge: MatchChallengeRef,
    ) -> ServiceResult<MatchState> {
        let now = self.clock.now_ms();
        let mut rooms = self.lock()?;

        // Distinct counter values can still truncate to the same six characters.
        // The counter rules out an infinite loop; the occupancy check rules out
        // silently evicting a live round.
        let match_id = {
            let mut counter = self.counter.lock().unwrap_or_else(|p| p.into_inner());
            loop {
                *counter += 1;
                let candidate = self.derive_code(*counter);
                if !rooms.contains_key(&candidate) {
                    break candidate;
                }
            }
        };

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
            last_seen: now,
        };

        let state = room.public_state(now);
        rooms.insert(match_id, room);
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

        room.last_seen = now;
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

        room.last_seen = now;
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
            return Err(ServiceError::MatchNotReady(
                "The challenge is revealed when the round starts.",
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
            return Err(ServiceError::MatchNotReady(
                "Results are published when the round closes.",
            ));
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
        room.last_seen = now;
        room.state.phase = MatchPhase::Cancelled;
        Ok(room.public_state(now))
    }

    /// Drop rounds nobody is using any more.
    ///
    /// Rooms are created by anyone who asks and were never removed, so on a
    /// public server the registry grew without bound — every abandoned lobby
    /// and every finished round kept its roster and entries forever. There is no
    /// database behind this; the map *is* the storage, so nothing reclaims it.
    ///
    /// A **running** round is never evicted regardless of age. It settles to
    /// `Results` on its own deadline, and dropping one mid-flight would delete a
    /// competition in progress.
    ///
    /// Returns the ids removed, for logging.
    pub fn evict_idle(
        &self,
        now: u64,
        results_retention_ms: u64,
        lobby_idle_timeout_ms: u64,
    ) -> Vec<String> {
        let mut rooms = match self.lock() {
            Ok(rooms) => rooms,
            Err(_) => return Vec::new(),
        };

        let mut removed = Vec::new();
        rooms.retain(|match_id, room| {
            let idle = now.saturating_sub(room.last_seen);
            let expired = match room.state.phase {
                // Kept a while after the close so players can read the
                // scoreboard; the retention clock restarts whenever one does.
                MatchPhase::Results | MatchPhase::Cancelled => idle > results_retention_ms,
                MatchPhase::Lobby | MatchPhase::Countdown => idle > lobby_idle_timeout_ms,
                MatchPhase::Running | MatchPhase::Grading => false,
            };
            if expired {
                removed.push(match_id.clone());
            }
            !expired
        });
        removed
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

/// A 32-byte key, from the OS entropy `RandomState` is seeded with.
///
/// `RandomState::new()` draws from the platform's random source and yields a
/// distinct key per call, which is enough for identifiers that only have to be
/// unguessable within a process lifetime. It is deliberately **not** used for
/// anything that must survive a restart or resist an offline attack —
/// `ItemRefSigner`'s key is loaded from configuration for exactly that reason.
fn random_key() -> [u8; 32] {
    let mut key = [0u8; 32];
    for chunk in key.chunks_mut(8) {
        let word = RandomState::new().build_hasher().finish().to_ne_bytes();
        chunk.copy_from_slice(&word[..chunk.len()]);
    }
    key
}
