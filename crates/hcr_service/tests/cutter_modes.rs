//! Rounds and sessions are single-mode.
//!
//! A round ranks players against each other and a session estimates one ability.
//! Both only mean something if everything being compared is the same task — and
//! the two programming modes are not: one Cutter Grid command crosses a lattice
//! cell, one servo command drives a joint, and the same challenge has a
//! different difficulty in each (`docs/07-CALIBRATION.md` §1, SPEC v0.3 §15.1).
//!
//! These tests pin the boundary from both sides: the right mode is accepted, the
//! wrong one is refused, and the refusal is distinguishable from the other ways
//! a submission can be rejected.

use std::sync::Arc;

use hcr_contract::*;
use hcr_qbank::{Blueprint, ExposureController, SessionConfig};
use hcr_service::*;
use hcr_sim::ReplayOptions;

const PLAN_FIXTURE: &str =
    include_str!("../../hcr_sim/tests/fixtures/cutter-grid-plan-v2.json");
const VECTORS: &str = include_str!("../../hcr_sim/tests/fixtures/vectors.json");

/// The shipped challenge, which is the one item with a certified Cutter Grid
/// profile, plus a servo-only item to prove the filtering does something.
fn catalog() -> Arc<CatalogStore> {
    let vectors: serde_json::Value = serde_json::from_str(VECTORS).expect("vectors parse");
    let challenge: ChallengeDefinition =
        serde_json::from_value(vectors["challenge"].clone()).expect("challenge parses");

    let store = Arc::new(CatalogStore::new());
    store
        .insert(ChallengeDefinitionDto {
            challenge: challenge.clone(),
            meta: ChallengeMeta {
                programming_modes: vec![ProgrammingMode::Servo, ProgrammingMode::CutterGrid],
                calibration: CalibrationState::Calibrated,
                ..ChallengeMeta::provisional(1, 0.0)
            },
        })
        .expect("insert dual-mode");

    let mut servo_only = challenge;
    servo_only.id = "servo-only".into();
    store
        .insert(ChallengeDefinitionDto {
            challenge: servo_only,
            meta: ChallengeMeta {
                calibration: CalibrationState::Calibrated,
                ..ChallengeMeta::provisional(1, 0.0)
            },
        })
        .expect("insert servo-only");

    store
}

fn service() -> HcrService {
    HcrService::new(
        catalog(),
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"cutter-modes-signing-key-012"),
        ServiceConfig {
            session: SessionConfig {
                min_items: 1,
                max_items: 4,
                ..SessionConfig::default()
            },
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::unlimited(),
            seed: 11,
            session_idle_timeout_ms: 30 * 60 * 1000,
            ..ServiceConfig::default()
        },
    )
}

fn cutter_submission(id: &str) -> SubmissionCreate {
    let carried: CutterGridSubmission =
        serde_json::from_str(PLAN_FIXTURE).expect("plan fixture parses");
    SubmissionCreate {
        submission_id: id.into(),
        challenge_id: "neat-short-cap".into(),
        challenge_version: 1,
        program: Program {
            nodes: Vec::new(),
            source_block_count: carried.program.source_block_count,
        },
        cutter_grid: Some(carried),
        session_id: None,
        match_id: None,
        client_preview: None,
    }
}

fn servo_submission(id: &str, challenge_id: &str) -> SubmissionCreate {
    SubmissionCreate {
        submission_id: id.into(),
        challenge_id: challenge_id.into(),
        challenge_version: 1,
        program: Program {
            nodes: vec![ProgramNode::Wait {
                duration_ms: 10.0,
                source_block_id: "w".into(),
            }],
            source_block_count: 1,
        },
        cutter_grid: None,
        session_id: None,
        match_id: None,
        client_preview: None,
    }
}

fn round_config(mode: ProgrammingMode) -> MatchConfig {
    MatchConfig {
        duration_ms: 60_000,
        rank_by: RankBy::Completion,
        max_players: 4,
        min_submit_interval_ms: 0,
        challenge_ref: Some(MatchChallengeRef {
            challenge_id: "neat-short-cap".into(),
            version: 1,
        }),
        programming_mode: mode,
    }
}

// ---------------------------------------------------------------------------
// The result carries its own mode
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_scored_result_reports_which_engine_produced_it() {
    let service = service();

    let cutter = service
        .create_submission(cutter_submission("m-1"))
        .await
        .expect("scores");
    assert_eq!(cutter.programming_mode, ProgrammingMode::CutterGrid);

    let servo = service
        .create_submission(servo_submission("m-2", "neat-short-cap"))
        .await
        .expect("scores");
    assert_eq!(servo.programming_mode, ProgrammingMode::Servo);
}

/// Servo results are byte-identical to what earlier builds returned.
#[tokio::test]
async fn a_servo_result_gains_no_new_field_on_the_wire() {
    let service = service();
    let result = service
        .create_submission(servo_submission("m-3", "neat-short-cap"))
        .await
        .expect("scores");

    let json = serde_json::to_string(&result).expect("serializes");
    assert!(!json.contains("programmingMode"), "{json}");
}

// ---------------------------------------------------------------------------
// Rounds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_cutter_grid_round_accepts_a_cutter_grid_submission() {
    let service = service();
    let state = service
        .create_match(round_config(ProgrammingMode::CutterGrid))
        .expect("create");
    service
        .join_match(&state.match_id, "p1", "Player One")
        .expect("join");
    service.start_match(&state.match_id).expect("start");

    let mut request = cutter_submission("r-1");
    request.match_id = Some(state.match_id.clone());
    let ack = service
        .submit_to_match(&state.match_id, "p1", request)
        .await
        .expect("submit");

    assert!(ack.accepted, "rejected: {:?}", ack.rejected_reason);
}

/// The refusal that makes a round's ranking mean something.
#[tokio::test]
async fn a_servo_round_refuses_a_cutter_grid_submission() {
    let service = service();
    let state = service
        .create_match(round_config(ProgrammingMode::Servo))
        .expect("create");
    service
        .join_match(&state.match_id, "p1", "Player One")
        .expect("join");
    service.start_match(&state.match_id).expect("start");

    let mut request = cutter_submission("r-2");
    request.match_id = Some(state.match_id.clone());
    let ack = service
        .submit_to_match(&state.match_id, "p1", request)
        .await
        .expect("submit");

    assert!(!ack.accepted);
    assert_eq!(
        ack.rejected_reason,
        Some(MatchRejection::WrongProgrammingMode),
        "the player is on the right challenge and has to switch modes, which is \
         a different fix from WrongChallenge",
    );
}

#[tokio::test]
async fn a_cutter_grid_round_refuses_a_servo_submission() {
    let service = service();
    let state = service
        .create_match(round_config(ProgrammingMode::CutterGrid))
        .expect("create");
    service
        .join_match(&state.match_id, "p1", "Player One")
        .expect("join");
    service.start_match(&state.match_id).expect("start");

    let mut request = servo_submission("r-3", "neat-short-cap");
    request.match_id = Some(state.match_id.clone());
    let ack = service
        .submit_to_match(&state.match_id, "p1", request)
        .await
        .expect("submit");

    assert!(!ack.accepted);
    assert_eq!(
        ack.rejected_reason,
        Some(MatchRejection::WrongProgrammingMode)
    );
}

/// A lobby nobody could submit into is refused at creation, not at T0.
#[tokio::test]
async fn a_round_cannot_pin_a_challenge_that_lacks_the_mode() {
    let service = service();
    let mut config = round_config(ProgrammingMode::CutterGrid);
    config.challenge_ref = Some(MatchChallengeRef {
        challenge_id: "servo-only".into(),
        version: 1,
    });

    let error = service
        .create_match(config)
        .expect_err("servo-only item has no certified planner profile");
    assert_eq!(error.code(), HcrErrorCode::ProgramInvalid);
}

/// Unpinned rounds pick only from items that support the mode.
#[tokio::test]
async fn an_unpinned_cutter_grid_round_picks_a_supported_challenge() {
    let service = service();
    let mut config = round_config(ProgrammingMode::CutterGrid);
    config.challenge_ref = None;

    let state = service.create_match(config).expect("create");
    service
        .join_match(&state.match_id, "p1", "Player One")
        .expect("join");
    let started = service.start_match(&state.match_id).expect("start");
    assert_eq!(started.phase, MatchPhase::Running);

    // Only one item declares Cutter Grid, so the picker had exactly one legal
    // choice and must have made it rather than falling back to the alphabet.
    let challenge = service
        .match_challenge(&state.match_id)
        .expect("challenge revealed");
    assert_eq!(challenge.challenge.id, "neat-short-cap");
}

/// Standings say which task they rank.
///
/// A ranking is only meaningful next to the task it ranks: two rounds on the
/// same challenge in different modes produce two tables of numbers that must not
/// be read against each other.
#[tokio::test]
async fn results_report_the_round_mode() {
    // Deadlines are judged by the server clock, so a round is closed by moving
    // the clock rather than by waiting.
    let clock = ManualClock::new(1_700_000_000_000);
    let service = HcrService::with_clock(
        catalog(),
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"cutter-modes-signing-key-012"),
        ServiceConfig {
            session: SessionConfig::default(),
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::unlimited(),
            seed: 11,
            session_idle_timeout_ms: 30 * 60 * 1000,
            ..ServiceConfig::default()
        },
        Arc::new(clock.clone()),
    );

    let state = service
        .create_match(round_config(ProgrammingMode::CutterGrid))
        .expect("create");
    service
        .join_match(&state.match_id, "p1", "Player One")
        .expect("join");
    service.start_match(&state.match_id).expect("start");

    let mut request = cutter_submission("r-4");
    request.match_id = Some(state.match_id.clone());
    let ack = service
        .submit_to_match(&state.match_id, "p1", request)
        .await
        .expect("submit");
    assert!(ack.accepted);

    clock.advance(120_000);

    let results = service.match_results(&state.match_id).expect("results");
    assert_eq!(results.programming_mode, ProgrammingMode::CutterGrid);
    assert_eq!(results.rows.len(), 1);
    assert_eq!(results.rows[0].completion_score, 100.0);
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

/// A session serves only what it can be answered in.
#[tokio::test]
async fn a_cutter_grid_session_serves_only_profiled_items() {
    let service = service();
    let snapshot = service
        .start_session(SessionStart {
            programming_mode: ProgrammingMode::CutterGrid,
            ..SessionStart::default()
        })
        .await
        .expect("start");

    let item = service
        .next_item(&snapshot.session_id)
        .await
        .expect("an item");
    assert_eq!(
        item.challenge_id, "neat-short-cap",
        "the servo-only item must not be served into a Cutter Grid session",
    );
}

/// The estimate stays inside its own mode.
#[tokio::test]
async fn a_cutter_grid_session_refuses_a_servo_response() {
    let service = service();
    let snapshot = service
        .start_session(SessionStart {
            programming_mode: ProgrammingMode::CutterGrid,
            ..SessionStart::default()
        })
        .await
        .expect("start");
    let item = service
        .next_item(&snapshot.session_id)
        .await
        .expect("an item");

    service
        .create_submission(servo_submission("s-1", &item.challenge_id))
        .await
        .expect("scores");

    let error = service
        .respond(SessionRespond {
            session_id: snapshot.session_id.clone(),
            item_ref: item.item_ref.clone(),
            submission_id: "s-1".into(),
        })
        .await
        .expect_err("a servo attempt must not move a Cutter Grid ability");

    assert_eq!(error.code(), HcrErrorCode::ItemRefInvalid);
}

#[tokio::test]
async fn a_cutter_grid_session_accepts_a_cutter_grid_response() {
    let service = service();
    let snapshot = service
        .start_session(SessionStart {
            programming_mode: ProgrammingMode::CutterGrid,
            ..SessionStart::default()
        })
        .await
        .expect("start");
    let item = service
        .next_item(&snapshot.session_id)
        .await
        .expect("an item");

    let mut request = cutter_submission("s-2");
    request.challenge_id = item.challenge_id.clone();
    request.challenge_version = item.challenge_version;
    request.session_id = Some(snapshot.session_id.clone());
    service.create_submission(request).await.expect("scores");

    let outcome = service
        .respond(SessionRespond {
            session_id: snapshot.session_id.clone(),
            item_ref: item.item_ref,
            submission_id: "s-2".into(),
        })
        .await
        .expect("the response is in the session's own mode");

    assert!(outcome.correct, "a perfect cut should count as mastery");
}

/// A servo session is unchanged, and still sees every item.
#[tokio::test]
async fn a_servo_session_still_sees_servo_only_items() {
    let service = service();
    let snapshot = service
        .start_session(SessionStart::default())
        .await
        .expect("start");

    let mut seen = Vec::new();
    for _ in 0..2 {
        let item = service
            .next_item(&snapshot.session_id)
            .await
            .expect("an item");
        let request = servo_submission(&format!("d-{}", seen.len()), &item.challenge_id);
        let id = request.submission_id.clone();
        service.create_submission(request).await.expect("scores");
        service
            .respond(SessionRespond {
                session_id: snapshot.session_id.clone(),
                item_ref: item.item_ref.clone(),
                submission_id: id,
            })
            .await
            .expect("records");
        seen.push(item.challenge_id);
    }

    assert!(
        seen.iter().any(|id| id == "servo-only"),
        "a servo session should reach the servo-only item: {seen:?}",
    );
}
