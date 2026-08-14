//! Cutter Grid submissions end to end, through the service and the HTTP binding.
//!
//! `hcr_sim`'s own suite proves the verifier is right about a trajectory. This
//! one proves the service reaches it: that a Cutter Grid submission is routed to
//! verification rather than to servo replay, that a rejection becomes the right
//! wire error, and that the usage log records which mode produced the row.

use std::sync::Arc;

use hcr_contract::*;
use hcr_qbank::{Blueprint, ExposureController, SessionConfig};
use hcr_service::*;
use hcr_sim::ReplayOptions;

/// The real planner's certified trajectory, shared with `hcr_sim`'s tests rather
/// than duplicated — two copies would drift and the drift would look like a bug
/// in whichever crate noticed second.
const PLAN_FIXTURE: &str =
    include_str!("../../hcr_sim/tests/fixtures/cutter-grid-plan-v2.json");

/// The shipped challenge, from the fixture the deployed catalog seeds from.
const VECTORS: &str = include_str!("../../hcr_sim/tests/fixtures/vectors.json");

/// A catalog holding only the shipped challenge.
///
/// `seed_catalog` would also do, and it is what the server runs — but it
/// generates the whole item bank, which costs seconds per call and none of it is
/// used here. The plan was planned against *this* challenge, and a synthetic one
/// would fail the signature check before reaching anything under test.
fn catalog() -> Arc<CatalogStore> {
    let vectors: serde_json::Value = serde_json::from_str(VECTORS).expect("vectors parse");
    let challenge: ChallengeDefinition =
        serde_json::from_value(vectors["challenge"].clone()).expect("challenge parses");

    let catalog = Arc::new(CatalogStore::new());
    catalog
        .insert(ChallengeDefinitionDto {
            challenge,
            meta: ChallengeMeta::provisional(1, 0.0),
        })
        .expect("insert");
    catalog
}

fn service() -> HcrService {
    HcrService::new(
        catalog(),
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"cutter-test-signing-key-0123"),
        ServiceConfig {
            session: SessionConfig::default(),
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::unlimited(),
            seed: 7,
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
        // A Cutter Grid submission carries no joint commands. The field stays
        // required so every existing client and reader is unaffected.
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

#[tokio::test]
async fn a_cutter_grid_submission_is_verified_and_scored() {
    let service = service();
    let result = service
        .create_submission(cutter_submission("sub-cutter-1"))
        .await
        .expect("the certified reference trajectory should score");

    assert_eq!(result.status, SubmissionStatus::Completed);
    assert_eq!(result.score.completion_score, 100.0);
    // Per cell, not per block: the reference program is five blocks, 22 cells.
    assert_eq!(result.metrics.executed_command_count, 22);
    assert!(!result.replay.diverged_from_client);
}

/// The empty `program` is never replayed.
///
/// Worth pinning: an empty servo program is a validation error, so if dispatch
/// ever fell through to the servo engine this would fail with `PROGRAM_INVALID`
/// rather than scoring — a regression that would otherwise only show up as
/// Cutter Grid mysteriously breaking.
#[tokio::test]
async fn the_servo_program_is_not_replayed_when_a_trajectory_is_present() {
    let service = service();
    let mut request = cutter_submission("sub-cutter-2");
    request.program.nodes.clear();

    let result = service
        .create_submission(request)
        .await
        .expect("dispatch should reach the trajectory verifier");
    assert_eq!(result.score.completion_score, 100.0);
}

/// Idempotency is indifferent to mode.
#[tokio::test]
async fn resubmitting_returns_the_first_result() {
    let service = service();
    let first = service
        .create_submission(cutter_submission("sub-cutter-3"))
        .await
        .expect("first");
    let second = service
        .create_submission(cutter_submission("sub-cutter-3"))
        .await
        .expect("second");

    assert_eq!(first.result_voxels_hash, second.result_voxels_hash);
    assert_eq!(first.score.final_score, second.score.final_score);
}

/// A tampered trajectory is refused with a code a client can act on.
#[tokio::test]
async fn a_rejected_trajectory_becomes_a_trajectory_rejected_error() {
    let service = service();
    let mut request = cutter_submission("sub-cutter-4");
    if let Some(cutter) = request.cutter_grid.as_mut() {
        cutter.plan.challenge_signature = "0000000000000000".into();
    }

    let error = service
        .create_submission(request)
        .await
        .expect_err("a plan for another challenge must not score");

    let wire = error.to_wire();
    assert_eq!(wire.code, HcrErrorCode::TrajectoryRejected);
    assert!(!wire.retryable, "replanning is required, not retrying");
    assert_eq!(
        wire.details
            .as_ref()
            .and_then(|details| details.get("rejection"))
            .map(String::as_str),
        Some("SIGNATURE_MISMATCH"),
    );
}

/// The offending block reaches the editor.
///
/// The frontend highlights `field` to show a learner where a program went wrong;
/// dropping it on this path would leave them with a message and no location.
#[tokio::test]
async fn a_rejection_carries_the_block_to_highlight() {
    let service = service();
    let mut request = cutter_submission("sub-cutter-5");
    if let Some(cutter) = request.cutter_grid.as_mut() {
        // Break the first step's timeline: attributable to its source block.
        cutter.plan.steps[0].duration_ms = 1.0;
    }

    let error = service
        .create_submission(request)
        .await
        .expect_err("an inconsistent timeline must be refused");

    let wire = error.to_wire();
    assert_eq!(wire.code, HcrErrorCode::TrajectoryRejected);
    assert_eq!(wire.field.as_deref(), Some("reference-1"));
}

/// A trajectory failure is a 422, not a 500.
#[test]
fn a_rejection_maps_to_unprocessable_content() {
    assert_eq!(status_for(HcrErrorCode::TrajectoryRejected), 422);
}

/// The usage row says which mode wrote the program.
///
/// Without it the calibration refit pools two incomparable tasks — a Cutter Grid
/// command is one cell of travel, a servo command is one joint move — and fits a
/// single difficulty against the mixture.
#[tokio::test]
async fn the_usage_row_records_the_cutter_grid_mode() {
    let dir = std::env::temp_dir().join(format!("hcr-cutter-usage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("usage.jsonl");

    let service = service().with_usage_log(Arc::new(UsageLog::open(&path).expect("open log")));
    service
        .create_submission(cutter_submission("sub-cutter-6"))
        .await
        .expect("scores");

    let written = std::fs::read_to_string(&path).expect("log written");
    assert!(
        written.contains(r#""mode":"cutter-grid""#),
        "usage row should record the mode: {written}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// A servo row still carries no mode, so old and new files stay comparable.
#[tokio::test]
async fn a_servo_row_is_unchanged_by_the_new_field() {
    let dir = std::env::temp_dir().join(format!("hcr-servo-usage-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let path = dir.join("usage.jsonl");

    let service = service().with_usage_log(Arc::new(UsageLog::open(&path).expect("open log")));
    let request = SubmissionCreate {
        submission_id: "sub-servo-1".into(),
        challenge_id: "neat-short-cap".into(),
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
    };
    service.create_submission(request).await.expect("scores");

    let written = std::fs::read_to_string(&path).expect("log written");
    assert!(
        !written.contains(r#""mode""#),
        "a servo row must stay byte-identical to what earlier builds wrote: {written}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
