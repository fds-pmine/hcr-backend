//! HTTP binding: routing, codecs and status mapping.

use std::sync::Arc;

use hcr_contract::api::ScoreInput;
use hcr_contract::*;
use hcr_qbank::{Blueprint, ExposureController, SessionConfig};
use hcr_service::*;
use hcr_sim::ReplayOptions;

mod common;
use common::{challenge, safe_program, submission};

fn router() -> Router {
    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(challenge("easy", 1, -1.0)).unwrap();
    catalog.insert(challenge("hard", 2, 1.0)).unwrap();

    Router::new(Arc::new(HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"binding-key"),
        ServiceConfig {
            session: SessionConfig {
                min_items: 1,
                max_items: 2,
                ..SessionConfig::default()
            },
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::unlimited(),
            seed: 5,
            session_idle_timeout_ms: 60_000,
            ..ServiceConfig::default()
        },
    )))
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

#[tokio::test]
async fn listing_challenges_returns_the_v1_summary_shape() {
    let reply = router().dispatch(HttpCall::get("/api/v1/challenges")).await;

    assert_eq!(reply.status, 200);
    let listed: Vec<ChallengeSummary> = reply.json().expect("summaries");
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].id, "easy");
}

#[tokio::test]
async fn a_challenge_can_be_fetched_by_id_and_by_version() {
    let router = router();

    let latest = router.dispatch(HttpCall::get("/api/v1/challenges/hard")).await;
    assert_eq!(latest.status, 200);
    assert!(latest.text().contains("\"version\":2"));

    let pinned = router
        .dispatch(HttpCall::get("/api/v1/challenges/hard/2"))
        .await;
    assert_eq!(pinned.status, 200);

    // The wire form is the *definition* — voxels as an array, not a Set, which is
    // what the frontend's normalizer expects.
    assert!(pinned.text().contains("\"initialHair\""));
    assert!(pinned.text().contains("\"voxels\""));
}

#[tokio::test]
async fn unknown_challenges_and_routes_are_404() {
    let router = router();

    assert_eq!(
        router
            .dispatch(HttpCall::get("/api/v1/challenges/nope"))
            .await
            .status,
        404
    );
    assert_eq!(
        router.dispatch(HttpCall::get("/api/v1/nonsense")).await.status,
        404
    );
    assert_eq!(router.dispatch(HttpCall::get("/")).await.status, 404);
}

// ---------------------------------------------------------------------------
// Scoring parity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn the_score_endpoint_matches_the_v1_score_provider() {
    let input = ScoreInput {
        // Asked for one voxel off, and it came off: completion 100.
        initial_voxels: vec!["0,0,0".into(), "1,0,0".into(), "2,0,0".into()],
        target_voxels: vec!["0,0,0".into(), "1,0,0".into()],
        result_voxels: vec!["0,0,0".into(), "1,0,0".into()],
        program_metrics: ProgramMetrics {
            source_block_count: 5,
            executed_command_count: 5,
            estimated_duration_ms: 5_645.0,
        },
        scoring: ScoringConfig {
            weights: ScoreWeights {
                completion: 0.6,
                efficiency: 0.25,
                time: 0.15,
            },
            reference_program_cost: 6.25,
            reference_time_ms: 5_645.0,
            command_weight: 0.25,
        },
    };

    let reply = router()
        .dispatch(HttpCall::post("/api/v1/score", &input))
        .await;

    assert_eq!(reply.status, 200);
    let score: ScoreResult = reply.json().expect("score");
    assert_eq!(score.completion_score, 100.0);
    assert_eq!(score.final_score, 100.0);
}

#[tokio::test]
async fn a_malformed_voxel_key_is_reported_as_a_validation_error() {
    let input = ScoreInput {
        initial_voxels: vec![],
        target_voxels: vec!["not-a-key".into()],
        result_voxels: vec![],
        program_metrics: ProgramMetrics::default(),
        scoring: ScoringConfig {
            weights: ScoreWeights {
                completion: 0.6,
                efficiency: 0.25,
                time: 0.15,
            },
            reference_program_cost: 6.25,
            reference_time_ms: 5_645.0,
            command_weight: 0.25,
        },
    };

    let reply = router()
        .dispatch(HttpCall::post("/api/v1/score", &input))
        .await;

    assert_eq!(reply.status, 422);
    assert!(reply.text().contains("PROGRAM_INVALID"));
}

// ---------------------------------------------------------------------------
// Submissions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_submission_round_trips_through_the_router() {
    let router = router();
    let reply = router
        .dispatch(HttpCall::post(
            "/api/v1/submissions",
            submission("sub-1", "easy", 1, safe_program()),
        ))
        .await;

    assert_eq!(reply.status, 200);
    let result: SubmissionResult = reply.json().expect("result");
    assert_eq!(result.status, SubmissionStatus::Completed);

    let fetched = router.dispatch(HttpCall::get("/api/v1/submissions/sub-1")).await;
    assert_eq!(fetched.status, 200);
}

#[tokio::test]
async fn a_malformed_body_is_422_not_500() {
    let reply = router()
        .dispatch(HttpCall {
            method: Method::Post,
            path: "/api/v1/submissions".into(),
            body: b"{ not json".to_vec(),
            player_id: None,
            display_name: None,
        })
        .await;

    assert_eq!(reply.status, 422);
    assert!(reply.text().contains("PROGRAM_INVALID"));
}

#[tokio::test]
async fn error_codes_map_to_the_documented_statuses() {
    // Spot-check the table in 01-CONTRACT.md §7.
    assert_eq!(status_for(HcrErrorCode::Unauthorized), 401);
    assert_eq!(status_for(HcrErrorCode::ChallengeNotFound), 404);
    assert_eq!(status_for(HcrErrorCode::ProgramTooLarge), 422);
    assert_eq!(status_for(HcrErrorCode::ItemRefInvalid), 409);
    assert_eq!(status_for(HcrErrorCode::RateLimited), 429);
    assert_eq!(status_for(HcrErrorCode::ReplayTimeout), 504);
    assert_eq!(status_for(HcrErrorCode::Internal), 500);
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_can_be_driven_entirely_over_http() {
    let router = router();

    let opened = router
        .dispatch(HttpCall::post("/api/v1/sessions", SessionStart::default()))
        .await;
    assert_eq!(opened.status, 200);
    let snapshot: SessionSnapshot = opened.json().expect("snapshot");
    let session_id = snapshot.session_id;

    let next = router
        .dispatch(HttpCall::post(
            format!("/api/v1/sessions/{session_id}/next"),
            (),
        ))
        .await;
    assert_eq!(next.status, 200);
    let item: NextItem = next.json().expect("item");

    router
        .dispatch(HttpCall::post(
            "/api/v1/submissions",
            submission("sub-1", &item.challenge_id, item.challenge_version, safe_program()),
        ))
        .await;

    let responded = router
        .dispatch(HttpCall::post(
            format!("/api/v1/sessions/{session_id}/responses"),
            SessionRespond {
                // Deliberately wrong: the path must win.
                session_id: "some-other-session".into(),
                item_ref: item.item_ref,
                submission_id: "sub-1".into(),
            },
        ))
        .await;

    assert_eq!(
        responded.status, 200,
        "the path is authoritative for identity: {}",
        responded.text()
    );
    let outcome: ResponseOutcome = responded.json().expect("outcome");
    assert!(outcome.theta.is_finite());

    let finalized = router
        .dispatch(HttpCall::post(
            format!("/api/v1/sessions/{session_id}/finalize"),
            (),
        ))
        .await;
    assert_eq!(finalized.status, 200);
    let result: SessionResultDto = finalized.json().expect("result");
    assert_eq!(result.total_items, 1);
}

#[tokio::test]
async fn operations_on_an_unknown_session_are_404() {
    let reply = router()
        .dispatch(HttpCall::post("/api/v1/sessions/s-nope/next", ()))
        .await;
    assert_eq!(reply.status, 404);
}

// ---------------------------------------------------------------------------
// Rounds
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_round_can_be_created_joined_and_started_over_http() {
    let router = router();

    let created = router
        .dispatch(HttpCall::post("/api/v1/matches", MatchConfig::default()))
        .await;
    assert_eq!(created.status, 200);
    let state: MatchState = created.json().expect("state");
    let match_id = state.match_id;
    assert_eq!(state.phase, MatchPhase::Lobby);

    // The challenge is withheld until the round starts.
    assert_eq!(
        router
            .dispatch(HttpCall::get(format!("/api/v1/matches/{match_id}/challenge")))
            .await
            .status,
        409
    );

    let joined = router
        .dispatch(
            HttpCall::post(format!("/api/v1/matches/{match_id}/join"), ()).as_player("alice"),
        )
        .await;
    assert_eq!(joined.status, 200);

    let started = router
        .dispatch(HttpCall::post(format!("/api/v1/matches/{match_id}/start"), ()))
        .await;
    assert_eq!(started.status, 200);

    assert_eq!(
        router
            .dispatch(HttpCall::get(format!("/api/v1/matches/{match_id}/challenge")))
            .await
            .status,
        200
    );
}

#[tokio::test]
async fn a_round_that_is_not_there_yet_says_so_in_its_own_code() {
    // Neither refusal is a fault: the caller should retry later, not
    // differently. Reporting them as a forged reference or a terminated session
    // would be misleading in a message a waiting player actually reads.
    let router = router();
    let created = router
        .dispatch(HttpCall::post("/api/v1/matches", MatchConfig::default()))
        .await;
    let match_id = created.json::<MatchState>().expect("state").match_id;

    let early = router
        .dispatch(HttpCall::get(format!("/api/v1/matches/{match_id}/challenge")))
        .await;
    assert_eq!(early.status, 409);
    assert!(early.text().contains("MATCH_NOT_READY"), "{}", early.text());
    assert!(early.text().contains("revealed when the round starts"));

    router
        .dispatch(
            HttpCall::post(format!("/api/v1/matches/{match_id}/join"), ()).as_player("alice"),
        )
        .await;
    router
        .dispatch(HttpCall::post(format!("/api/v1/matches/{match_id}/start"), ()))
        .await;

    let running = router
        .dispatch(HttpCall::get(format!("/api/v1/matches/{match_id}/results")))
        .await;
    assert_eq!(running.status, 409);
    assert!(running.text().contains("MATCH_NOT_READY"));
    assert!(running.text().contains("published when the round closes"));
}

#[tokio::test]
async fn a_display_name_labels_the_roster_without_becoming_the_identity() {
    let router = router();
    let created = router
        .dispatch(HttpCall::post("/api/v1/matches", MatchConfig::default()))
        .await;
    let match_id = created.json::<MatchState>().expect("state").match_id;

    let joined = router
        .dispatch(
            HttpCall::post(format!("/api/v1/matches/{match_id}/join"), ())
                .as_player_named("u-8f21", "Alice"),
        )
        .await;

    let state: MatchState = joined.json().expect("state");
    let player = state.players.first().expect("one participant");
    // The label is what other players read; the id is what the server acts on.
    assert_eq!(player.display_name, "Alice");
    assert_eq!(player.player_id, "u-8f21");
}

#[tokio::test]
async fn an_absent_display_name_falls_back_to_the_player_id() {
    let router = router();
    let created = router
        .dispatch(HttpCall::post("/api/v1/matches", MatchConfig::default()))
        .await;
    let match_id = created.json::<MatchState>().expect("state").match_id;

    let joined = router
        .dispatch(HttpCall::post(format!("/api/v1/matches/{match_id}/join"), ()).as_player("bob"))
        .await;

    let state: MatchState = joined.json().expect("state");
    assert_eq!(state.players[0].display_name, "bob");
}

#[tokio::test]
async fn joining_without_an_authenticated_player_is_refused() {
    // Identity comes from the auth layer, never the body — otherwise a caller
    // could act as somebody else.
    let router = router();
    let created = router
        .dispatch(HttpCall::post("/api/v1/matches", MatchConfig::default()))
        .await;
    let state: MatchState = created.json().expect("state");

    let reply = router
        .dispatch(HttpCall::post(
            format!("/api/v1/matches/{}/join", state.match_id),
            (),
        ))
        .await;

    assert_eq!(reply.status, 500);
    assert!(reply.text().contains("authenticated player"));
}

#[tokio::test]
async fn the_time_endpoint_reports_the_server_clock() {
    let reply = router().dispatch(HttpCall::get("/api/v1/time")).await;
    assert_eq!(reply.status, 200);

    let sync: TimeSync = reply.json().expect("time");
    assert!(sync.server_time > 0);
}
