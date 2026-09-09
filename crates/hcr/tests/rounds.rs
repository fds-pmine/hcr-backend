//! Competitive rounds and session eviction.
//!
//! Deadlines are judged by the server clock, so these run against a
//! [`ManualClock`] — a five-minute round is tested in microseconds, and the
//! boundary cases are exact rather than racy.

use std::sync::Arc;

use hcr_contract::*;
use hcr_qbank::{Blueprint, ExposureController, SessionConfig};
use hcr::*;
use hcr_sim::ReplayOptions;

mod common;
use common::{challenge, safe_program, submission};

const T0: u64 = 1_700_000_000_000;

fn service_at(clock: &ManualClock, idle_timeout_ms: u64) -> HcrService {
    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(challenge("easy", 1, -1.0)).unwrap();
    catalog.insert(challenge("hard", 1, 1.0)).unwrap();

    HcrService::with_clock(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"round-test-key"),
        ServiceConfig {
            session: SessionConfig {
                min_items: 2,
                max_items: 3,
                ..SessionConfig::default()
            },
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::unlimited(),
            seed: 7,
            session_idle_timeout_ms: idle_timeout_ms,
            ..ServiceConfig::default()
        },
        Arc::new(clock.clone()),
    )
}

/// A service whose catalog holds exactly the named challenges.
fn service_with_catalog(clock: &ManualClock, ids: &[&str]) -> HcrService {
    let catalog = Arc::new(CatalogStore::new());
    for (index, id) in ids.iter().enumerate() {
        catalog.insert(challenge(id, 1, index as f64 - 1.0)).unwrap();
    }

    HcrService::with_clock(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"round-test-key"),
        ServiceConfig {
            session: SessionConfig {
                min_items: 2,
                max_items: 3,
                ..SessionConfig::default()
            },
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::unlimited(),
            seed: 7,
            session_idle_timeout_ms: 600_000,
            ..ServiceConfig::default()
        },
        Arc::new(clock.clone()),
    )
}

fn config(duration_ms: u64) -> MatchConfig {
    MatchConfig {
        duration_ms,
        rank_by: RankBy::Completion,
        max_players: 4,
        min_submit_interval_ms: 0,
        challenge_ref: Some(MatchChallengeRef {
            challenge_id: "easy".into(),
            version: 1,
        }),
        programming_mode: ProgrammingMode::Servo,
        // The round settings this suite is not about; a struct literal keeps
        // the compiler honest when the contract grows.
        ..MatchConfig::default()
    }
}

/// A scored submission, built directly so ranking can be tested precisely.
fn scored(id: &str, completion: f64, efficiency: f64, duration_ms: f64) -> SubmissionResult {
    SubmissionResult {
        submission_id: id.into(),
        challenge_id: "easy".into(),
        challenge_version: 1,
        status: SubmissionStatus::Completed,
        programming_mode: ProgrammingMode::Servo,
        score: ScoreResult {
            completion_score: completion,
            efficiency_score: efficiency,
            time_score: 100.0,
            // Deliberately the inverse of completion, so a test can tell which
            // metric the ranking actually used.
            final_score: 100.0 - completion,
            program_cost: 1.0,
        },
        metrics: ProgramMetrics {
            source_block_count: 1,
            executed_command_count: 1,
            estimated_duration_ms: duration_ms,
        },
        result_voxels_hash: "0".repeat(64),
        terminal: Terminal::completed(),
        replay: hcr_contract::api::ReplayInfo {
            engine_version: "test".into(),
            tick_ms: 5.0,
            simulated_ms: duration_ms,
            diverged_from_client: false,
        },
        error: None,
    }
}

fn registry_with_players(clock: &ManualClock, players: &[&str]) -> (MatchRegistry, String) {
    let registry = MatchRegistry::new(Arc::new(clock.clone()));
    let state = registry
        .create(
            config(60_000),
            MatchChallengeRef {
                challenge_id: "easy".into(),
                version: 1,
            },
        )
        .unwrap();
    for player in players {
        registry.join(&state.match_id, player, player).unwrap();
    }
    registry.start(&state.match_id).unwrap();
    (registry, state.match_id)
}

// ---------------------------------------------------------------------------
// Reveal and lifecycle
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_challenge_is_not_revealed_before_the_round_starts() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);

    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();

    // Handing it out during the lobby would give an early joiner a head start.
    assert!(service.match_challenge(&state.match_id).is_err());
    assert_eq!(state.phase, MatchPhase::Lobby);
    assert!(state.closes_at.is_none());

    let started = service.start_match(&state.match_id).unwrap();
    assert_eq!(started.phase, MatchPhase::Running);
    assert_eq!(started.opens_at, Some(T0));
    assert_eq!(started.closes_at, Some(T0 + 60_000));
    assert_eq!(
        service
            .match_challenge(&state.match_id)
            .unwrap()
            .challenge
            .id,
        "easy"
    );
}

#[tokio::test]
async fn everyone_receives_the_identical_challenge() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);
    let state = service.create_match(config(60_000)).unwrap();

    for player in ["alice", "bob", "carol"] {
        service.join_match(&state.match_id, player, player).unwrap();
    }
    service.start_match(&state.match_id).unwrap();

    let first = service.match_challenge(&state.match_id).unwrap();
    let second = service.match_challenge(&state.match_id).unwrap();
    assert_eq!(first.challenge.id, second.challenge.id);
    assert_eq!(first.meta.version, second.meta.version);
}

#[tokio::test]
async fn joining_after_the_start_is_refused() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.start_match(&state.match_id).unwrap();

    assert!(service.join_match(&state.match_id, "latecomer", "Late").is_err());
}

// ---------------------------------------------------------------------------
// The deadline
// ---------------------------------------------------------------------------

#[tokio::test]
async fn submissions_are_accepted_before_the_deadline_and_refused_after() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service.start_match(&state.match_id).unwrap();

    clock.advance(59_999);
    let inside = service
        .submit_to_match(
            &state.match_id,
            "alice",
            submission("sub-1", "easy", 1, safe_program()),
        )
        .await
        .unwrap();
    assert!(inside.accepted);
    assert_eq!(inside.server_received_at, T0 + 59_999);

    // One millisecond past the close.
    clock.advance(2);
    let outside = service
        .submit_to_match(
            &state.match_id,
            "alice",
            submission("sub-2", "easy", 1, safe_program()),
        )
        .await
        .unwrap();
    assert!(!outside.accepted);
    assert_eq!(
        outside.rejected_reason,
        Some(MatchRejection::AfterDeadline)
    );
}

#[tokio::test]
async fn an_acknowledgement_never_carries_a_score() {
    // Revealing standings mid-round would let a player refine against a known bar.
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service.start_match(&state.match_id).unwrap();

    let ack = service
        .submit_to_match(
            &state.match_id,
            "alice",
            submission("sub-1", "easy", 1, safe_program()),
        )
        .await
        .unwrap();

    // The ack type has no score field at all — this is a structural guarantee,
    // and asking for results early is refused outright.
    assert!(ack.accepted);
    assert!(service.match_results(&state.match_id).is_err());
}

#[tokio::test]
async fn results_become_available_once_the_deadline_passes() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service.start_match(&state.match_id).unwrap();

    service
        .submit_to_match(
            &state.match_id,
            "alice",
            submission("sub-1", "easy", 1, safe_program()),
        )
        .await
        .unwrap();

    assert!(service.match_results(&state.match_id).is_err());
    clock.advance(60_001);

    assert_eq!(
        service.match_state(&state.match_id).unwrap().phase,
        MatchPhase::Results
    );
    let results = service.match_results(&state.match_id).unwrap();
    assert_eq!(results.rows.len(), 1);
    assert_eq!(results.rows[0].rank, 1);
    assert_eq!(results.rows[0].player_id, "alice");
    // Published so a disputed deadline decision is checkable afterwards.
    assert_eq!(results.rows[0].server_received_at, Some(T0));
}

#[tokio::test]
async fn a_rematch_keeps_the_room_and_its_roster_and_clears_the_last_round() {
    // The point of the whole feature: twenty students do not retype a new
    // six-character code between rounds.
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 600_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service.join_match(&state.match_id, "bob", "Bob").unwrap();
    service.start_match(&state.match_id).unwrap();
    let first_challenge = service.match_challenge(&state.match_id).unwrap();

    service
        .submit_to_match(
            &state.match_id,
            "alice",
            submission("sub-1", &first_challenge.challenge.id, 1, safe_program()),
        )
        .await
        .unwrap();
    clock.advance(60_001);
    assert_eq!(
        service.match_results(&state.match_id).unwrap().rows.len(),
        2
    );

    let reopened = service.rematch(&state.match_id).unwrap();

    assert_eq!(reopened.match_id, state.match_id, "the code must survive");
    assert_eq!(reopened.phase, MatchPhase::Lobby);
    assert!(reopened.opens_at.is_none() && reopened.closes_at.is_none());
    assert_eq!(reopened.players.len(), 2, "the roster must survive");
    assert!(
        reopened.players.iter().all(|player| !player.submitted),
        "a player carrying last round's submitted flag would look done before starting",
    );
    // Standings are gone: they belonged to the round that ended.
    assert!(service.match_results(&state.match_id).is_err());

    service.start_match(&state.match_id).unwrap();
    let second_challenge = service.match_challenge(&state.match_id).unwrap();
    assert_ne!(
        second_challenge.challenge.id, first_challenge.challenge.id,
        "reopening on the same challenge would make round two a memory test",
    );
}

#[tokio::test]
async fn repeated_rematches_work_through_the_bank_before_repeating() {
    // Refusing only the challenge just played would alternate between two
    // items for ever, because the picker is deterministic.
    let clock = ManualClock::new(T0);
    // The shared fixture holds two challenges; a third makes the difference
    // between flipping and rotating observable.
    let service = service_with_catalog(&clock, &["easy", "hard", "medium"]);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();

    let mut seen = Vec::new();
    for _ in 0..4 {
        service.start_match(&state.match_id).unwrap();
        seen.push(
            service
                .match_challenge(&state.match_id)
                .unwrap()
                .challenge
                .id
                .clone(),
        );
        clock.advance(60_001);
        service.match_results(&state.match_id).unwrap();
        service.rematch(&state.match_id).unwrap();
    }

    let mut distinct = seen.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(
        distinct.len(),
        3,
        "four rounds over a three-item bank served {seen:?} — flipping, not rotating",
    );
    // The fourth round is where the bank wraps; it must not repeat the third.
    assert_ne!(seen[0], seen[1]);
    assert_ne!(seen[1], seen[2]);
    assert_ne!(seen[2], seen[3]);
}

#[tokio::test]
async fn a_room_that_has_played_everything_keeps_going() {
    // A bank with one playable item cannot avoid a repeat, and a session that
    // stopped dead at that point would be worse than one that repeats.
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 600_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();

    for _ in 0..8 {
        service.start_match(&state.match_id).unwrap();
        clock.advance(60_001);
        service.match_results(&state.match_id).unwrap();
        service
            .rematch(&state.match_id)
            .expect("a reopened room must keep working once the bank wraps");
    }
}

#[tokio::test]
async fn crews_are_set_in_the_lobby_and_published_to_everybody() {
    // The whole point: a crew a student can see on their own screen, rather
    // than a letter living in one browser's memory.
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 600_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service.join_match(&state.match_id, "bob", "Bob").unwrap();
    service.join_match(&state.match_id, "cy", "Cy").unwrap();

    let crews = std::collections::HashMap::from([
        ("alice".to_string(), "A".to_string()),
        ("bob".to_string(), "A".to_string()),
    ]);
    let assigned = service.set_match_crews(&state.match_id, &crews).unwrap();

    let crew_of = |state: &MatchState, id: &str| {
        state
            .players
            .iter()
            .find(|player| player.player_id == id)
            .and_then(|player| player.crew.clone())
    };
    assert_eq!(crew_of(&assigned, "alice").as_deref(), Some("A"));
    assert_eq!(crew_of(&assigned, "bob").as_deref(), Some("A"));
    // Absent from the map means un-crewed, not unchanged.
    assert_eq!(crew_of(&assigned, "cy"), None);

    // Every reader sees it, which is the difference from the old per-browser map.
    let seen = service.match_state(&state.match_id).unwrap();
    assert_eq!(crew_of(&seen, "alice").as_deref(), Some("A"));

    // And it survives into the round.
    service.start_match(&state.match_id).unwrap();
    let running = service.match_state(&state.match_id).unwrap();
    assert_eq!(crew_of(&running, "bob").as_deref(), Some("A"));
}

#[tokio::test]
async fn crews_cannot_be_changed_once_the_round_is_under_way() {
    // Re-teaming mid-round changes what a crew total means halfway through the
    // thing it is totalling; re-teaming after the deadline would let somebody
    // pick the winning crew with the scores already in.
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 600_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    let crews = std::collections::HashMap::from([("alice".to_string(), "B".to_string())]);

    service.set_match_crews(&state.match_id, &crews).unwrap();
    service.start_match(&state.match_id).unwrap();
    assert!(service.set_match_crews(&state.match_id, &crews).is_err());

    clock.advance(60_001);
    service.match_results(&state.match_id).unwrap();
    assert!(service.set_match_crews(&state.match_id, &crews).is_err());

    // Reopening puts the room back in a lobby, so the next round can be re-teamed.
    service.rematch(&state.match_id).unwrap();
    assert!(service.set_match_crews(&state.match_id, &crews).is_ok());
}

#[tokio::test]
async fn a_rejoining_player_keeps_the_crew_they_were_put_in() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 600_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service
        .set_match_crews(
            &state.match_id,
            &std::collections::HashMap::from([("alice".to_string(), "C".to_string())]),
        )
        .unwrap();

    // A reload re-joins with the same identity; losing the crew would silently
    // undo an assignment the room can see.
    let rejoined = service.join_match(&state.match_id, "alice", "Alice").unwrap();
    assert_eq!(
        rejoined
            .players
            .iter()
            .find(|player| player.player_id == "alice")
            .and_then(|player| player.crew.clone())
            .as_deref(),
        Some("C"),
    );
}

#[tokio::test]
async fn only_a_finished_round_can_be_reopened() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 600_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();

    // A lobby is already open.
    assert!(service.rematch(&state.match_id).is_err());

    // Reopening a running round would erase submissions from under the players
    // who made them.
    service.start_match(&state.match_id).unwrap();
    assert!(service.rematch(&state.match_id).is_err());

    clock.advance(60_001);
    assert!(service.match_results(&state.match_id).is_ok());
    assert!(service.rematch(&state.match_id).is_ok());
}

#[tokio::test]
async fn a_reopened_round_accepts_a_late_joiner() {
    // The reason to reopen rather than to keep the roster frozen: somebody who
    // missed round one can be in round two without a new room.
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 600_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service.start_match(&state.match_id).unwrap();
    assert!(service.join_match(&state.match_id, "bob", "Bob").is_err());

    clock.advance(60_001);
    service.match_results(&state.match_id).unwrap();
    service.rematch(&state.match_id).unwrap();

    let joined = service.join_match(&state.match_id, "bob", "Bob").unwrap();
    assert_eq!(joined.players.len(), 2);
}

#[tokio::test]
async fn a_non_participant_cannot_submit() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service.start_match(&state.match_id).unwrap();

    let ack = service
        .submit_to_match(
            &state.match_id,
            "mallory",
            submission("sub-x", "easy", 1, safe_program()),
        )
        .await
        .unwrap();

    assert_eq!(ack.rejected_reason, Some(MatchRejection::NotParticipant));
}

#[tokio::test]
async fn submissions_for_another_challenge_are_refused() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);
    let state = service.create_match(config(60_000)).unwrap();
    service.join_match(&state.match_id, "alice", "Alice").unwrap();
    service.start_match(&state.match_id).unwrap();

    let ack = service
        .submit_to_match(
            &state.match_id,
            "alice",
            submission("sub-w", "hard", 1, safe_program()),
        )
        .await
        .unwrap();

    assert_eq!(ack.rejected_reason, Some(MatchRejection::WrongChallenge));
}

#[test]
fn a_player_submitting_too_fast_is_throttled() {
    let clock = ManualClock::new(T0);
    let registry = MatchRegistry::new(Arc::new(clock.clone()));
    let state = registry
        .create(
            MatchConfig {
                min_submit_interval_ms: 2_000,
                ..config(60_000)
            },
            MatchChallengeRef {
                challenge_id: "easy".into(),
                version: 1,
            },
        )
        .unwrap();
    registry.join(&state.match_id, "alice", "Alice").unwrap();
    registry.start(&state.match_id).unwrap();

    assert!(
        registry
            .submit(&state.match_id, "alice", &scored("s1", 50.0, 50.0, 100.0))
            .unwrap()
            .accepted
    );

    clock.advance(500);
    assert_eq!(
        registry
            .submit(&state.match_id, "alice", &scored("s2", 60.0, 50.0, 100.0))
            .unwrap()
            .rejected_reason,
        Some(MatchRejection::RateLimited)
    );

    clock.advance(2_000);
    assert!(
        registry
            .submit(&state.match_id, "alice", &scored("s3", 60.0, 50.0, 100.0))
            .unwrap()
            .accepted
    );
}

// ---------------------------------------------------------------------------
// Ranking
// ---------------------------------------------------------------------------

#[test]
fn the_best_attempt_counts_not_the_last() {
    // Unlimited resubmission with best-of is the fairest treatment of a lag
    // spike or a bad first idea.
    let clock = ManualClock::new(T0);
    let (registry, match_id) = registry_with_players(&clock, &["alice"]);

    registry
        .submit(&match_id, "alice", &scored("good", 90.0, 50.0, 100.0))
        .unwrap();
    registry
        .submit(&match_id, "alice", &scored("bad", 10.0, 50.0, 100.0))
        .unwrap();

    clock.advance(60_001);
    let results = registry.results(&match_id).unwrap();

    assert_eq!(results.rows[0].completion_score, 90.0);
    assert_eq!(results.rows[0].submission_id.as_deref(), Some("good"));
}

#[test]
fn ranking_uses_completion_not_the_weighted_score() {
    // `final_score` in these fixtures is deliberately the inverse of completion,
    // so ranking on the wrong metric would reverse the standings.
    let clock = ManualClock::new(T0);
    let (registry, match_id) = registry_with_players(&clock, &["alice", "bob"]);

    registry
        .submit(&match_id, "alice", &scored("a", 90.0, 50.0, 100.0))
        .unwrap();
    registry
        .submit(&match_id, "bob", &scored("b", 40.0, 50.0, 100.0))
        .unwrap();

    clock.advance(60_001);
    let results = registry.results(&match_id).unwrap();

    assert_eq!(results.rank_by, RankBy::Completion);
    assert_eq!(results.rows[0].player_id, "alice");
    assert_eq!(results.rows[1].player_id, "bob");
    // The weighted score is still reported, just not used for ordering.
    assert!(results.rows[0].final_score < results.rows[1].final_score);
}

#[test]
fn ties_break_on_efficiency_then_duration() {
    let clock = ManualClock::new(T0);
    let (registry, match_id) = registry_with_players(&clock, &["alice", "bob", "carol"]);

    // Identical completion; efficiency separates alice, duration separates the rest.
    registry
        .submit(&match_id, "alice", &scored("a", 80.0, 90.0, 500.0))
        .unwrap();
    registry
        .submit(&match_id, "bob", &scored("b", 80.0, 50.0, 200.0))
        .unwrap();
    registry
        .submit(&match_id, "carol", &scored("c", 80.0, 50.0, 900.0))
        .unwrap();

    clock.advance(60_001);
    let ranked: Vec<String> = registry
        .results(&match_id)
        .unwrap()
        .rows
        .into_iter()
        .map(|row| row.player_id)
        .collect();

    assert_eq!(ranked, vec!["alice", "bob", "carol"]);
}

#[test]
fn a_player_who_never_submits_is_ranked_last_not_omitted() {
    // Dropping them would hide that they took part at all.
    let clock = ManualClock::new(T0);
    let (registry, match_id) = registry_with_players(&clock, &["alice", "ghost"]);

    registry
        .submit(&match_id, "alice", &scored("a", 30.0, 50.0, 100.0))
        .unwrap();

    clock.advance(60_001);
    let results = registry.results(&match_id).unwrap();

    assert_eq!(results.rows.len(), 2);
    assert_eq!(results.rows[1].player_id, "ghost");
    assert_eq!(results.rows[1].rank, 2);
    assert_eq!(results.rows[1].completion_score, 0.0);
    assert!(results.rows[1].submission_id.is_none());
}

#[test]
fn ranking_by_final_score_is_available_as_a_setting() {
    let clock = ManualClock::new(T0);
    let registry = MatchRegistry::new(Arc::new(clock.clone()));
    let state = registry
        .create(
            MatchConfig {
                rank_by: RankBy::Final,
                ..config(60_000)
            },
            MatchChallengeRef {
                challenge_id: "easy".into(),
                version: 1,
            },
        )
        .unwrap();
    registry.join(&state.match_id, "alice", "Alice").unwrap();
    registry.join(&state.match_id, "bob", "Bob").unwrap();
    registry.start(&state.match_id).unwrap();

    registry
        .submit(&state.match_id, "alice", &scored("a", 90.0, 50.0, 100.0))
        .unwrap();
    registry
        .submit(&state.match_id, "bob", &scored("b", 40.0, 50.0, 100.0))
        .unwrap();

    clock.advance(60_001);
    let results = registry.results(&state.match_id).unwrap();

    // final_score is 100 - completion here, so the order flips.
    assert_eq!(results.rows[0].player_id, "bob");
}

// ---------------------------------------------------------------------------
// Clock sync
// ---------------------------------------------------------------------------

#[tokio::test]
async fn time_sync_echoes_the_client_stamp_with_the_server_clock() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 60_000);

    let reply = service.time_sync(123);
    assert_eq!(reply.client_sent_at, 123);
    assert_eq!(reply.server_time, T0);
}

// ---------------------------------------------------------------------------
// Session eviction
// ---------------------------------------------------------------------------

#[tokio::test]
async fn idle_sessions_are_evicted() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 10_000);

    let session_id = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;
    assert_eq!(service.live_sessions().await, 1);

    // Still fresh.
    clock.advance(5_000);
    assert!(service.evict_idle_sessions().await.is_empty());
    assert_eq!(service.live_sessions().await, 1);

    // Past the timeout.
    clock.advance(10_000);
    let evicted = service.evict_idle_sessions().await;
    assert_eq!(evicted, vec![session_id.clone()]);
    assert_eq!(service.live_sessions().await, 0);

    assert!(matches!(
        service.next_item(&session_id).await,
        Err(ServiceError::SessionNotFound(_))
    ));
}

#[tokio::test]
async fn activity_keeps_a_session_alive() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 10_000);

    let session_id = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;

    // Touch the session just before the timeout would fire, three times over.
    for _ in 0..3 {
        clock.advance(9_000);
        service.session_snapshot(&session_id).await.unwrap();
        assert!(service.evict_idle_sessions().await.is_empty());
    }

    assert_eq!(service.live_sessions().await, 1);
}

#[tokio::test]
async fn eviction_leaves_other_sessions_untouched() {
    let clock = ManualClock::new(T0);
    let service = service_at(&clock, 10_000);

    let stale = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;
    clock.advance(9_000);
    let fresh = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;

    clock.advance(5_000);
    let evicted = service.evict_idle_sessions().await;

    assert_eq!(evicted, vec![stale]);
    assert!(service.session_snapshot(&fresh).await.is_ok());
}

// ---------------------------------------------------------------------------
// Room codes
// ---------------------------------------------------------------------------

#[test]
fn room_codes_are_short_unambiguous_and_unique() {
    let clock = ManualClock::new(T0);
    let registry = MatchRegistry::new(Arc::new(clock.clone()));

    let codes: Vec<String> = (0..200)
        .map(|_| {
            registry
                .create(
                    config(60_000),
                    MatchChallengeRef {
                        challenge_id: "easy".into(),
                        version: 1,
                    },
                )
                .unwrap()
                .match_id
        })
        .collect();

    for code in &codes {
        // A code has to survive being read out loud, so no I/O/0/1 and nothing
        // case-sensitive to get wrong.
        assert_eq!(code.len(), 6, "{code}");
        assert!(
            code.chars()
                .all(|c| c.is_ascii_uppercase() && !matches!(c, 'I' | 'O')
                    || matches!(c, '2'..='9')),
            "{code}"
        );
    }

    let unique: std::collections::HashSet<&String> = codes.iter().collect();
    assert_eq!(unique.len(), codes.len(), "codes collided");
}

#[test]
fn room_codes_are_not_sequential() {
    // The id is the only thing gating entry to a lobby, so a code a stranger can
    // reach by counting is a room a stranger can walk into. This also stops the
    // codes from publishing how many rounds the server has opened.
    let clock = ManualClock::new(T0);
    let open = || {
        let registry = MatchRegistry::new(Arc::new(clock.clone()));
        registry
            .create(
                config(60_000),
                MatchChallengeRef {
                    challenge_id: "easy".into(),
                    version: 1,
                },
            )
            .unwrap()
            .match_id
    };

    // Two fresh registries each open their *first* room. A counter-derived id
    // would make these identical and predictable.
    assert_ne!(open(), open());
}

// ---------------------------------------------------------------------------
// Eviction
// ---------------------------------------------------------------------------

#[test]
fn finished_and_abandoned_rounds_are_reclaimed_but_running_ones_are_not() {
    // The registry is the storage — there is no database behind it — so a public
    // server that never evicts grows for as long as it runs, and anyone can
    // create rooms.
    let clock = ManualClock::new(T0);
    let registry = MatchRegistry::new(Arc::new(clock.clone()));
    let open = || {
        registry
            .create(
                config(60_000),
                MatchChallengeRef {
                    challenge_id: "easy".into(),
                    version: 1,
                },
            )
            .unwrap()
            .match_id
    };

    let abandoned = open();
    let running = open();
    registry.join(&running, "alice", "Alice").unwrap();
    registry.start(&running).unwrap();

    // An hour on: the lobby nobody started is gone; the round in progress is not.
    let evicted = registry.evict_idle(T0 + 3_600_000, 15 * 60_000, 30 * 60_000);
    assert_eq!(evicted, vec![abandoned.clone()]);
    assert!(registry.state(&running).is_ok());
    assert!(registry.state(&abandoned).is_err());

    // Once it closes it becomes a result, and is kept only for the retention
    // window — long enough to read the scoreboard.
    clock.set(T0 + 3_600_000);
    registry.state(&running).unwrap();
    assert!(registry.evict_idle(T0 + 3_600_000, 15 * 60_000, 30 * 60_000).is_empty());
    assert_eq!(
        registry.evict_idle(T0 + 3_600_000 + 16 * 60_000, 15 * 60_000, 30 * 60_000),
        vec![running]
    );
    assert!(registry.is_empty());
}

#[test]
fn reading_a_scoreboard_keeps_it_alive() {
    // Retention runs from the last look, not from the close, so a round does not
    // vanish out from under the people still reading it.
    let clock = ManualClock::new(T0);
    let registry = MatchRegistry::new(Arc::new(clock.clone()));
    let match_id = registry
        .create(
            config(1_000),
            MatchChallengeRef {
                challenge_id: "easy".into(),
                version: 1,
            },
        )
        .unwrap()
        .match_id;
    registry.join(&match_id, "alice", "Alice").unwrap();
    registry.start(&match_id).unwrap();

    clock.set(T0 + 2_000);
    registry.state(&match_id).unwrap(); // settles to Results

    // Somebody is still watching at +14 minutes.
    clock.set(T0 + 14 * 60_000);
    registry.state(&match_id).unwrap();

    // So at +20 minutes it has still only been idle for 6.
    assert!(
        registry
            .evict_idle(T0 + 20 * 60_000, 15 * 60_000, 30 * 60_000)
            .is_empty()
    );
    assert_eq!(
        registry
            .evict_idle(T0 + 30 * 60_000, 15 * 60_000, 30 * 60_000)
            .len(),
        1
    );
}
