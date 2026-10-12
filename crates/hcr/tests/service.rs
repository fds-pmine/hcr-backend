//! Service behaviour, exercised without any transport.

use std::sync::Arc;

use hcr_contract::*;
use hcr_qbank::{Blueprint, ExposureController, SessionConfig};
use hcr::*;
use hcr_sim::ReplayOptions;

mod common;
use common::{challenge, colliding_program, safe_program, submission};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn service_with(challenges: Vec<ChallengeDefinitionDto>) -> HcrService {
    let catalog = Arc::new(CatalogStore::new());
    for dto in challenges {
        catalog.insert(dto).expect("insert");
    }
    HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"test-signing-key-0123456789"),
        ServiceConfig {
            session: SessionConfig {
                min_items: 2,
                max_items: 3,
                ..SessionConfig::default()
            },
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::unlimited(),
            seed: 99,
            session_idle_timeout_ms: 30 * 60 * 1000,
            ..ServiceConfig::default()
        },
    )
}

fn default_service() -> HcrService {
    service_with(vec![
        challenge("easy", 1, -1.0),
        challenge("medium", 1, 0.0),
        challenge("hard", 1, 1.0),
    ])
}

// ---------------------------------------------------------------------------
// Item references
// ---------------------------------------------------------------------------

fn claims(session: &str, index: usize, item: &str, version: u32) -> ItemRefClaims {
    ItemRefClaims {
        session_id: session.into(),
        bank_index: index,
        item_id: item.into(),
        challenge_version: version,
        issued_at: 1_700_000_000_000,
    }
}

#[test]
fn item_references_round_trip() {
    let signer = ItemRefSigner::new(*b"key");
    let original = claims("s-1", 3, "medium", 2);

    let token = signer.sign(&original).unwrap();
    assert_eq!(signer.verify(&token).unwrap(), original);
}

#[test]
fn a_tampered_item_reference_is_rejected() {
    let signer = ItemRefSigner::new(*b"key");
    let token = signer.sign(&claims("s-1", 0, "easy", 1)).unwrap();
    let (payload, signature) = token.split_once('.').unwrap();

    // Re-encode different claims but keep the original signature — the exact
    // move an attacker makes to answer a harder item as an easier one.
    let forged_payload = {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&claims("s-1", 0, "hard", 1)).unwrap())
    };
    assert_ne!(forged_payload, payload);

    let forged = format!("{forged_payload}.{signature}");
    assert!(matches!(
        signer.verify(&forged),
        Err(ServiceError::ItemRefInvalid(_))
    ));
}

#[test]
fn a_reference_signed_with_another_key_is_rejected() {
    let token = ItemRefSigner::new(*b"real-key")
        .sign(&claims("s-1", 0, "easy", 1))
        .unwrap();

    assert!(matches!(
        ItemRefSigner::new(*b"other-key").verify(&token),
        Err(ServiceError::ItemRefInvalid(_))
    ));
}

#[test]
fn a_malformed_reference_is_rejected_rather_than_panicking() {
    let signer = ItemRefSigner::new(*b"key");
    for bad in ["", "no-dot", "a.b", "...", "!!!.???"] {
        assert!(signer.verify(bad).is_err(), "{bad:?} should be rejected");
    }
}

#[test]
fn the_signing_key_is_never_rendered() {
    let rendered = format!("{:?}", ItemRefSigner::new(*b"super-secret"));
    assert!(!rendered.contains("secret"), "{rendered}");
}

// ---------------------------------------------------------------------------
// Catalog
// ---------------------------------------------------------------------------

#[test]
fn the_catalog_resolves_the_latest_version_by_default() {
    let store = CatalogStore::new();
    store.insert(challenge("item", 1, 0.0)).unwrap();
    store.insert(challenge("item", 3, 0.5)).unwrap();
    store.insert(challenge("item", 2, 0.2)).unwrap();

    assert_eq!(store.get("item", None).unwrap().meta.version, 3);
    assert_eq!(store.get("item", Some(1)).unwrap().meta.version, 1);
    assert_eq!(store.get("item", Some(1)).unwrap().meta.irt.difficulty, 0.0);
}

#[test]
fn overwriting_a_served_version_is_refused() {
    // Editing a version in place would move scores already reported to learners.
    let store = CatalogStore::new();
    store.insert(challenge("item", 1, 0.0)).unwrap();
    assert!(store.insert(challenge("item", 1, 9.0)).is_err());
    assert_eq!(store.get("item", Some(1)).unwrap().meta.irt.difficulty, 0.0);
}

#[test]
fn missing_challenges_are_reported_precisely() {
    let store = CatalogStore::new();
    store.insert(challenge("item", 1, 0.0)).unwrap();

    assert!(matches!(
        store.get("nope", None),
        Err(ServiceError::ChallengeNotFound { .. })
    ));
    assert!(matches!(
        store.get("item", Some(7)),
        Err(ServiceError::ChallengeNotFound { .. })
    ));
}

#[test]
fn listings_are_ordered_and_deduplicated_to_the_latest_version() {
    let store = CatalogStore::new();
    store.insert(challenge("beta", 1, 0.0)).unwrap();
    store.insert(challenge("alpha", 1, 0.0)).unwrap();
    store.insert(challenge("alpha", 2, 0.0)).unwrap();

    let listed: Vec<String> = store.list().unwrap().into_iter().map(|c| c.id).collect();
    assert_eq!(listed, vec!["alpha".to_string(), "beta".to_string()]);
}

// ---------------------------------------------------------------------------
// Submissions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_submission_is_scored_authoritatively() {
    let service = default_service();
    let result = service
        .create_submission(submission("sub-1", "easy", 1, safe_program()))
        .await
        .unwrap();

    assert_eq!(result.status, SubmissionStatus::Completed);
    assert_eq!(result.terminal.reason, TerminalReason::Completed);
    assert_eq!(result.metrics.executed_command_count, 1);
    assert!(result.score.final_score > 0.0);
    assert_eq!(result.result_voxels_hash.len(), 64);
    assert!(result.replay.engine_version.starts_with("hcr/"));
}

#[tokio::test]
async fn resubmitting_the_same_id_returns_the_first_result() {
    // QoS 1 is at-least-once and HTTP clients retry, so duplicates are normal.
    let service = default_service();

    let first = service
        .create_submission(submission("sub-1", "easy", 1, safe_program()))
        .await
        .unwrap();
    // A different program under the same id must NOT re-score.
    let second = service
        .create_submission(submission("sub-1", "easy", 1, colliding_program()))
        .await
        .unwrap();

    assert_eq!(first.result_voxels_hash, second.result_voxels_hash);
    assert_eq!(first.terminal.reason, second.terminal.reason);
    assert_eq!(second.status, SubmissionStatus::Completed);
}

#[tokio::test]
async fn a_submission_id_cannot_be_claimed_by_another_submitter() {
    // F7: a bare id once returned anyone's stored result.
    let service = default_service();
    let session_id = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;
    let item = service.next_item(&session_id).await.unwrap();
    let request = submission(
        "sub-bob",
        &item.challenge_id,
        item.challenge_version,
        safe_program(),
    );

    service
        .create_submission_for(request.clone(), Some("bob"), true)
        .await
        .expect("bob's own submission");
    assert!(matches!(
        service.create_submission_for(request, Some("mallory"), true).await,
        Err(ServiceError::ProgramInvalid { .. })
    ));

    let error = service
        .respond(SessionRespond {
            session_id: session_id.clone(),
            item_ref: item.item_ref,
            submission_id: "sub-bob".into(),
        })
        .await
        .expect_err("scored outside this session");
    assert_eq!(
        error,
        ServiceError::ItemRefInvalid("the submission was scored for another session")
    );
}

#[tokio::test]
async fn a_head_collision_still_produces_a_score() {
    // Halting on the safety constraint is a terminal state, not a failure: the
    // learner still gets a number, and the offending block is named.
    let service = default_service();
    let result = service
        .create_submission(submission("sub-c", "easy", 1, colliding_program()))
        .await
        .unwrap();

    assert_eq!(result.status, SubmissionStatus::Error);
    assert_eq!(result.terminal.reason, TerminalReason::HeadCollision);
    assert_eq!(result.terminal.source_block_id.as_deref(), Some("a"));
    assert!(result.terminal.safe_angle_deg.is_some());
    assert!(result.error.is_none(), "not a request error");
}

#[tokio::test]
async fn an_oversized_program_is_rejected_with_the_cap() {
    let service = default_service();
    let program = Program {
        nodes: vec![ProgramNode::Repeat {
            count: 20,
            body: vec![ProgramNode::Repeat {
                count: 20,
                body: vec![
                    ProgramNode::SetJointAngle {
                        joint_id: "baseYaw".into(),
                        angle_deg: -40.0,
                        source_block_id: "a".into(),
                    },
                    ProgramNode::SetJointAngle {
                        joint_id: "baseYaw".into(),
                        angle_deg: -50.0,
                        source_block_id: "b".into(),
                    },
                ],
                source_block_id: "inner".into(),
            }],
            source_block_id: "outer".into(),
        }],
        source_block_count: 4,
    };

    assert!(matches!(
        service
            .create_submission(submission("sub-big", "easy", 1, program))
            .await,
        Err(ServiceError::ProgramTooLarge { limit: 500 })
    ));
}

#[tokio::test]
async fn an_invalid_joint_names_the_offending_block() {
    let service = default_service();
    let program = Program {
        nodes: vec![ProgramNode::SetJointAngle {
            joint_id: "nonexistent".into(),
            angle_deg: 0.0,
            source_block_id: "bad-block".into(),
        }],
        source_block_count: 1,
    };

    match service
        .create_submission(submission("sub-x", "easy", 1, program))
        .await
    {
        Err(error @ ServiceError::ProgramInvalid { .. }) => {
            // The field travels to the editor so it can highlight the block.
            assert_eq!(error.to_wire().field.as_deref(), Some("nonexistent"));
        }
        other => panic!("expected ProgramInvalid, got {other:?}"),
    }
}

#[tokio::test]
async fn submitting_against_an_unknown_version_fails() {
    let service = default_service();
    assert!(matches!(
        service
            .create_submission(submission("sub-v", "easy", 99, safe_program()))
            .await,
        Err(ServiceError::ChallengeNotFound { .. })
    ));
}

#[tokio::test]
async fn identical_replays_are_served_from_cache() {
    let service = default_service();
    assert_eq!(service.replay_pool().cached(), 0);

    service
        .create_submission(submission("sub-1", "easy", 1, safe_program()))
        .await
        .unwrap();
    assert_eq!(service.replay_pool().cached(), 1);

    // A different submission id, same challenge and program: no second replay.
    service
        .create_submission(submission("sub-2", "easy", 1, safe_program()))
        .await
        .unwrap();
    assert_eq!(service.replay_pool().cached(), 1);
}

#[tokio::test]
async fn client_preview_divergence_is_reported() {
    let service = default_service();

    let honest = service
        .create_submission(submission("sub-1", "easy", 1, safe_program()))
        .await
        .unwrap();
    assert!(!honest.replay.diverged_from_client);

    let mut lying = submission("sub-2", "easy", 1, safe_program());
    lying.client_preview = Some(ClientPreview {
        score_result: ScoreResult {
            completion_score: 100.0,
            efficiency_score: 100.0,
            time_score: 100.0,
            final_score: 100.0,
            program_cost: 1.0,
        },
        result_voxels_hash: "deadbeef".repeat(8),
        engine_version: "fake".into(),
        tick_ms: 5.0,
    });

    let checked = service.create_submission(lying).await.unwrap();
    assert!(checked.replay.diverged_from_client);
    // The authoritative score is unaffected by what the client claimed.
    assert_eq!(checked.score.final_score, honest.score.final_score);
}

#[tokio::test]
async fn a_declared_block_count_below_the_program_does_not_lower_the_cost() {
    // F8: `sourceBlockCount` is client-declared, so it may raise the cost but
    // never lower it below the nodes the program actually has.
    let service = default_service();
    let program = |declared: u32| Program {
        nodes: vec![ProgramNode::Repeat {
            count: 2,
            body: safe_program().nodes,
            source_block_id: "r".into(),
        }],
        source_block_count: declared,
    };

    let honest = service
        .create_submission(submission("sub-1", "easy", 1, program(2)))
        .await
        .unwrap();
    let lying = service
        .create_submission(submission("sub-2", "easy", 1, program(0)))
        .await
        .unwrap();

    assert_eq!(lying.metrics.source_block_count, 2);
    assert_eq!(lying.score.program_cost, honest.score.program_cost);

    let cutter = CutterGridProgram {
        kind: "cutter-grid".into(),
        version: 1,
        planner_version: "cutter-grid-compact-ptp-v4".into(),
        nodes: vec![CutterGridNode::Repeat {
            count: 2,
            body: vec![CutterGridNode::Move {
                direction: CutterGridDirection::Right,
                distance: 1,
                source_block_id: "m".into(),
            }],
            source_block_id: "r".into(),
        }],
        source_block_count: 0,
    };
    assert_eq!(cutter.scored_block_count(), 2);
}

#[tokio::test]
async fn a_submission_id_outside_the_client_id_rule_is_refused() {
    // Each id becomes a map key or a usage-log field, so it is refused before
    // anything stores it.
    let service = default_service();
    let long = "x".repeat(65);
    let with_session = |id: &str| {
        let mut request = submission("sub-ok", "easy", 1, safe_program());
        request.session_id = Some(id.into());
        request
    };
    let with_match = |id: &str| {
        let mut request = submission("sub-ok", "easy", 1, safe_program());
        request.match_id = Some(id.into());
        request
    };

    for (case, request) in [
        ("empty id", submission("", "easy", 1, safe_program())),
        ("oversized id", submission(&long, "easy", 1, safe_program())),
        ("space", submission("a b", "easy", 1, safe_program())),
        ("slash", submission("a/b", "easy", 1, safe_program())),
        ("oversized session id", with_session(&long)),
        ("newline in match id", with_match("a\nb")),
    ] {
        let error = service.create_submission(request).await.expect_err(case);
        assert!(
            matches!(error, ServiceError::ProgramInvalid { .. }),
            "{case}: {error:?}"
        );
    }
    assert_eq!(service.retained_submissions(), 0);

    let longest = "x".repeat(64);
    let scored = service
        .create_submission(submission(&longest, "easy", 1, safe_program()))
        .await
        .expect("a 64-byte id is accepted");
    assert_eq!(scored.status, SubmissionStatus::Completed);
}

// ---------------------------------------------------------------------------
// Sessions
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_session_runs_end_to_end() {
    let service = default_service();
    let opened = service.start_session(SessionStart::default()).await.unwrap();

    assert_eq!(opened.state, SessionLifecycle::Active);
    assert_eq!(opened.response_count, 0);
    // arona reports infinite standard error before any response; JSON cannot say
    // "infinity", so it is absent rather than wrong.
    assert!(opened.standard_error.is_none());

    let session_id = opened.session_id.clone();
    let mut answered = 0u32;

    while answered < 3 {
        let Ok(item) = service.next_item(&session_id).await else {
            break;
        };

        let submission_id = format!("sub-{answered}");
        service
            .create_submission(SubmissionCreate {
                session_id: Some(session_id.clone()),
                ..submission(
                    &submission_id,
                    &item.challenge_id,
                    item.challenge_version,
                    safe_program(),
                )
            })
            .await
            .unwrap();

        let outcome = service
            .respond(SessionRespond {
                session_id: session_id.clone(),
                item_ref: item.item_ref,
                submission_id,
            })
            .await
            .unwrap();

        answered += 1;
        assert!(outcome.theta.is_finite());
        assert!(outcome.standard_error.is_finite());
        if outcome.terminated {
            break;
        }
    }

    assert!(answered >= 2, "min_items should have been served");

    let result = service.finalize_session(&session_id).await.unwrap();
    assert_eq!(result.total_items, answered);
    assert_eq!(result.items.len() as u32, answered);
    assert!(result.final_theta.is_finite());
    assert!(!result.termination_reason.is_empty());

    // Finalizing removes the session.
    assert!(matches!(
        service.session_snapshot(&session_id).await,
        Err(ServiceError::SessionNotFound(_))
    ));
}

#[tokio::test]
async fn asking_for_the_next_item_twice_returns_the_same_item() {
    // A retried request must not silently burn a second item out of the bank.
    let service = default_service();
    let session_id = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;

    let first = service.next_item(&session_id).await.unwrap();
    let second = service.next_item(&session_id).await.unwrap();

    assert_eq!(first.challenge_id, second.challenge_id);
    assert_eq!(first.item_ref, second.item_ref);

    let snapshot = service.session_snapshot(&session_id).await.unwrap();
    assert_eq!(snapshot.state, SessionLifecycle::AwaitingResponse);
    assert_eq!(snapshot.response_count, 0);
}

#[tokio::test]
async fn responding_with_a_forged_reference_is_refused() {
    let service = default_service();
    let session_id = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;
    let item = service.next_item(&session_id).await.unwrap();

    service
        .create_submission(submission(
            "sub-1",
            &item.challenge_id,
            item.challenge_version,
            safe_program(),
        ))
        .await
        .unwrap();

    // Minted with the wrong key.
    let forged = ItemRefSigner::new(*b"attacker")
        .sign(&claims(&session_id, 0, &item.challenge_id, 1))
        .unwrap();

    assert!(matches!(
        service
            .respond(SessionRespond {
                session_id: session_id.clone(),
                item_ref: forged,
                submission_id: "sub-1".into(),
            })
            .await,
        Err(ServiceError::ItemRefInvalid(_))
    ));

    // The session is untouched.
    let snapshot = service.session_snapshot(&session_id).await.unwrap();
    assert_eq!(snapshot.response_count, 0);
}

#[tokio::test]
async fn a_reference_from_another_session_is_refused() {
    let service = default_service();
    let first = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;
    let second = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;

    let item = service.next_item(&first).await.unwrap();
    service.next_item(&second).await.unwrap();
    service
        .create_submission(submission(
            "sub-1",
            &item.challenge_id,
            item.challenge_version,
            safe_program(),
        ))
        .await
        .unwrap();

    // A genuinely signed token, presented against the wrong session.
    assert!(matches!(
        service
            .respond(SessionRespond {
                session_id: second.clone(),
                item_ref: item.item_ref,
                submission_id: "sub-1".into(),
            })
            .await,
        Err(ServiceError::ItemRefInvalid(_))
    ));
}

#[tokio::test]
async fn responding_before_the_submission_is_scored_is_refused() {
    let service = default_service();
    let session_id = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;
    let item = service.next_item(&session_id).await.unwrap();

    // The score must come from replay, never from the caller.
    assert!(matches!(
        service
            .respond(SessionRespond {
                session_id: session_id.clone(),
                item_ref: item.item_ref,
                submission_id: "never-scored".into(),
            })
            .await,
        Err(ServiceError::ItemRefInvalid(_))
    ));
}

#[tokio::test]
async fn a_submission_for_a_different_challenge_is_refused() {
    let service = default_service();
    let session_id = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;
    let item = service.next_item(&session_id).await.unwrap();

    // Score some *other* challenge, then try to answer with it.
    let other = ["easy", "medium", "hard"]
        .into_iter()
        .find(|id| *id != item.challenge_id)
        .unwrap();
    service
        .create_submission(SubmissionCreate {
            session_id: Some(session_id.clone()),
            ..submission("sub-other", other, 1, safe_program())
        })
        .await
        .unwrap();

    assert!(matches!(
        service
            .respond(SessionRespond {
                session_id: session_id.clone(),
                item_ref: item.item_ref,
                submission_id: "sub-other".into(),
            })
            .await,
        Err(ServiceError::ItemRefInvalid(_))
    ));
}

#[tokio::test]
async fn a_session_over_an_empty_catalog_is_refused() {
    let service = service_with(vec![]);
    assert!(matches!(
        service.start_session(SessionStart::default()).await,
        Err(ServiceError::BankExhausted)
    ));
}

#[tokio::test]
async fn operations_on_an_unknown_session_are_reported() {
    let service = default_service();
    assert!(matches!(
        service.next_item("s-nope").await,
        Err(ServiceError::SessionNotFound(_))
    ));
    assert!(matches!(
        service.finalize_session("s-nope").await,
        Err(ServiceError::SessionNotFound(_))
    ));
}

#[tokio::test]
async fn sessions_are_independent() {
    let service = default_service();
    let a = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;
    let b = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;

    assert_ne!(a, b);
    assert_eq!(service.live_sessions().await, 2);

    let item = service.next_item(&a).await.unwrap();
    service
        .create_submission(SubmissionCreate {
            session_id: Some(a.clone()),
            ..submission(
                "sub-a",
                &item.challenge_id,
                item.challenge_version,
                safe_program(),
            )
        })
        .await
        .unwrap();
    service
        .respond(SessionRespond {
            session_id: a.clone(),
            item_ref: item.item_ref,
            submission_id: "sub-a".into(),
        })
        .await
        .unwrap();

    // Answering in `a` must not advance `b`.
    assert_eq!(
        service.session_snapshot(&a).await.unwrap().response_count,
        1
    );
    assert_eq!(
        service.session_snapshot(&b).await.unwrap().response_count,
        0
    );
}

// ---------------------------------------------------------------------------
// Catalog ordering and round item selection
// ---------------------------------------------------------------------------

/// A generated sibling of `common::challenge_dto`, tagged with provenance.
fn generated_dto(id: &str, calibration: CalibrationState) -> ChallengeDefinitionDto {
    let mut dto = common::challenge(id, 1, 0.0);
    dto.meta.calibration = calibration;
    dto.meta.generator = Some(GeneratorProvenance {
        family_id: "cap-trim".into(),
        version: "1".into(),
        seed: 7,
        params: Default::default(),
    });
    dto
}

#[test]
fn authored_challenges_lead_the_listing_whatever_the_alphabet_says() {
    // Generated ids begin "cap-trim-", so a plain id sort put a machine-made
    // item ahead of the authored challenge it was generated from — and every
    // client with no other signal opens the first entry.
    let catalog = CatalogStore::new();
    catalog.insert(generated_dto("cap-trim-aaa", CalibrationState::Provisional)).unwrap();
    catalog.insert(generated_dto("cap-trim-bbb", CalibrationState::Online)).unwrap();
    catalog.insert(common::challenge("neat-short-cap", 1, 0.0)).unwrap();

    let ids: Vec<String> = catalog.list().unwrap().into_iter().map(|s| s.id).collect();
    assert_eq!(ids, ["neat-short-cap", "cap-trim-aaa", "cap-trim-bbb"]);
}

#[test]
fn an_unpinned_round_lands_on_an_authored_challenge() {
    let catalog = CatalogStore::new();
    catalog.insert(generated_dto("cap-trim-aaa", CalibrationState::Calibrated)).unwrap();
    catalog.insert(common::challenge("neat-short-cap", 1, 0.0)).unwrap();

    assert_eq!(
        catalog.pick_for_match(ProgrammingMode::Servo).unwrap(),
        ("neat-short-cap".to_string(), 1)
    );
}

#[test]
fn a_round_may_use_a_provisional_item_but_never_a_retired_one() {
    // Provisional is fine: every player faces the identical item, so the
    // ranking holds whatever the item's difficulty turns out to be. Retired is
    // not — an item withdrawn as pathological must not decide who won.
    let catalog = CatalogStore::new();
    catalog.insert(generated_dto("cap-trim-aaa", CalibrationState::Provisional)).unwrap();
    assert_eq!(
        catalog.pick_for_match(ProgrammingMode::Servo).unwrap(),
        ("cap-trim-aaa".to_string(), 1)
    );

    let retired = CatalogStore::new();
    retired.insert(generated_dto("cap-trim-bbb", CalibrationState::Retired)).unwrap();
    assert!(matches!(
        retired.pick_for_match(ProgrammingMode::Servo),
        Err(ServiceError::BankExhausted)
    ));
}

// ---------------------------------------------------------------------------
// Usage collection
// ---------------------------------------------------------------------------

#[tokio::test]
async fn usage_records_the_calibration_datum_and_no_display_name() {
    let dir = std::env::temp_dir().join(format!("hcr-usage-{}", std::process::id()));
    let path = dir.join("usage.jsonl");
    let _ = std::fs::remove_file(&path);

    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(common::challenge("easy", 1, -1.0)).unwrap();
    let service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"usage-key"),
        ServiceConfig::default(),
    )
    .with_usage_log(Arc::new(UsageLog::open(&path).expect("open log")));

    service
        .create_submission_for(
            submission("sub-1", "easy", 1, safe_program()),
            Some("u-8f21"),
            true,
        )
        .await
        .unwrap();

    let line = std::fs::read_to_string(&path).expect("log written");
    let event: serde_json::Value = serde_json::from_str(line.trim()).expect("one json line");

    // One person, one item, one outcome — an IRT datum, which is the whole
    // reason this exists rather than being bolted-on analytics.
    assert_eq!(event["kind"], "submission");
    assert_eq!(event["playerId"], "u-8f21");
    assert_eq!(event["challengeId"], "easy");
    assert_eq!(event["challengeVersion"], 1);
    assert!(event["completionScore"].is_number());

    // Which rules produced `finalScore`. `challengeVersion` does not answer
    // that: the deployed catalog has already changed a scoring block without
    // moving the version, so rows either side claim version 1 and mean
    // different things. The digest is what tells them apart afterwards.
    assert_eq!(
        event["scoringDigest"],
        serde_json::Value::from(hcr::usage::scoring_digest(
            &common::challenge("easy", 1, -1.0).challenge.scoring
        )),
        "{event}"
    );

    // Free text a player typed is where a real name or an email would end up,
    // and grouping by playerId answers the same questions.
    assert!(event.get("displayName").is_none(), "{event}");
    // A learner's program is their work; the shape metrics carry the analysis
    // value without archiving it.
    assert!(event.get("program").is_none(), "{event}");

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn usage_collection_is_off_unless_a_deployment_asks_for_it() {
    // A default that collected would make every test and every dev run write to
    // disk, and would make collection something inherited rather than decided.
    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(common::challenge("easy", 1, -1.0)).unwrap();
    let service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"usage-key"),
        ServiceConfig::default(),
    );

    // No log attached: scoring still works and nothing is written anywhere.
    let result = service
        .create_submission_for(
            submission("sub-1", "easy", 1, safe_program()),
            Some("u-1"),
            true,
        )
        .await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn usage_row_skipped_when_consent_declined() {
    let dir = std::env::temp_dir().join(format!("hcr-usage-consent-{}", std::process::id()));
    let path = dir.join("usage.jsonl");
    let _ = std::fs::remove_file(&path);
    let service =
        default_service().with_usage_log(Arc::new(UsageLog::open(&path).expect("open log")));
    let rows = || {
        std::fs::read_to_string(&path)
            .unwrap_or_default()
            .lines()
            .count()
    };
    let session_id = service
        .start_session(SessionStart::default())
        .await
        .unwrap()
        .session_id;

    // The respond request carries no consent header; its row follows the
    // consent stored with the submission it names.
    for (consent, expected_rows) in [(false, 0), (true, 2)] {
        let item = service.next_item(&session_id).await.unwrap();
        let submission_id = format!("sub-{consent}");
        service
            .create_submission_for(
                SubmissionCreate {
                    session_id: Some(session_id.clone()),
                    ..submission(
                        &submission_id,
                        &item.challenge_id,
                        item.challenge_version,
                        safe_program(),
                    )
                },
                Some("u-1"),
                consent,
            )
            .await
            .unwrap();
        service
            .respond(SessionRespond {
                session_id: session_id.clone(),
                item_ref: item.item_ref,
                submission_id,
            })
            .await
            .unwrap();
        assert_eq!(rows(), expected_rows, "consent {consent}");
    }

    let declined = service
        .record_lesson_event(
            lesson_event("cutter-grid-distance", 1, LessonOutcome::Opened),
            Some("u-1"),
            false,
        )
        .expect("a declined event is still validated and accepted");
    assert!(!declined.recorded);
    assert_eq!(rows(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_idempotency_store_is_bounded() {
    // It grows once per scored program and never shrank. On a public server
    // that is a leak with a public write path.
    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(common::challenge("easy", 1, -1.0)).unwrap();
    let service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"bound-key"),
        ServiceConfig {
            max_retained_submissions: 8,
            ..ServiceConfig::default()
        },
    );

    for index in 0..25 {
        service
            .create_submission(submission(
                &format!("sub-{index}"),
                "easy",
                1,
                safe_program(),
            ))
            .await
            .unwrap();
    }

    assert!(
        service.retained_submissions() <= 8,
        "retained {}",
        service.retained_submissions()
    );

    // Re-scoring an evicted submission is deterministic, so a client that
    // retried past the cap still gets the same answer — idempotency degrades to
    // recomputation, not to a different result.
    let again = service
        .create_submission(submission("sub-0", "easy", 1, safe_program()))
        .await
        .unwrap();
    assert_eq!(again.score.completion_score, {
        let fresh = service
            .create_submission(submission("sub-fresh", "easy", 1, safe_program()))
            .await
            .unwrap();
        fresh.score.completion_score
    });
}

#[tokio::test]
async fn session_ids_are_not_derivable_from_seed() {
    // The id is the only thing gating a session, so one a classmate can
    // compute is a session a classmate can read, answer or finalize.
    let first_id = || async {
        default_service()
            .start_session(SessionStart::default())
            .await
            .unwrap()
            .session_id
    };
    assert_ne!(first_id().await, first_id().await);

    // The old formula for seed 99, ordinal 0.
    let service = default_service();
    service.start_session(SessionStart::default()).await.unwrap();
    assert!(matches!(
        service.session_snapshot("s-0000000000000063").await,
        Err(ServiceError::SessionNotFound(_))
    ));
}

#[tokio::test]
async fn live_sessions_are_capped() {
    // Anyone can open a session, and each one holds a bank snapshot until the
    // idle sweep, so without a cap a flood ends in an OOM kill.
    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(common::challenge("easy", 1, -1.0)).unwrap();
    let service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"bound-key"),
        ServiceConfig {
            max_live_sessions: 2,
            ..ServiceConfig::default()
        },
    );

    for _ in 0..2 {
        service.start_session(SessionStart::default()).await.unwrap();
    }
    assert!(matches!(
        service.start_session(SessionStart::default()).await,
        Err(ServiceError::RateLimited)
    ));
}

// ---------------------------------------------------------------------------
// Lesson telemetry
// ---------------------------------------------------------------------------

fn lesson_event(lesson_id: &str, section: u32, outcome: LessonOutcome) -> LessonEventCreate {
    LessonEventCreate {
        lesson_id: lesson_id.to_string(),
        section,
        activity: Some(LessonActivity::Observe),
        outcome,
        tests: Some(3),
        mode: Some(ProgrammingMode::CutterGrid),
    }
}

#[tokio::test]
async fn a_lesson_event_is_recorded_as_its_own_kind() {
    let dir = std::env::temp_dir().join(format!("hcr-usage-lesson-{}", std::process::id()));
    let path = dir.join("usage.jsonl");
    let _ = std::fs::remove_file(&path);

    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(common::challenge("easy", 1, -1.0)).unwrap();
    let service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"usage-key"),
        ServiceConfig::default(),
    )
    .with_usage_log(Arc::new(UsageLog::open(&path).expect("open log")));

    let ack = service
        .record_lesson_event(
            lesson_event("cutter-grid-fixed-axes", 11, LessonOutcome::SectionPassed),
            Some("u-8f21"),
            true,
        )
        .expect("accepted");
    assert!(ack.recorded);

    let line = std::fs::read_to_string(&path).expect("log written");
    let event: serde_json::Value = serde_json::from_str(line.trim()).expect("one json line");

    // `kind` is what keeps a claimed outcome out of a calibration refit: these
    // rows say what a browser reported, and a `submission` row says what the
    // server replayed. Pooling them would fit difficulty against a mixture.
    assert_eq!(event["kind"], "lesson");
    assert_eq!(event["lessonId"], "cutter-grid-fixed-axes");
    assert_eq!(event["section"], 11);
    assert_eq!(event["activity"], "observe");
    assert_eq!(event["outcome"], "section-passed");
    assert_eq!(event["mode"], "cutter-grid");
    assert_eq!(event["playerId"], "u-8f21");
    // Nothing about a score: the lessons are not scored by this service, and a
    // row that carried one would be claiming they were.
    assert!(event.get("completionScore").is_none(), "{event}");

    let _ = std::fs::remove_file(&path);
}

#[tokio::test]
async fn a_lesson_event_that_is_not_the_shape_of_one_is_refused() {
    // The endpoint appends to a file and needs no authentication, so shape is
    // the whole defence: there is no lesson catalogue here to check membership
    // against, and an unbounded field would go straight into the log.
    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(common::challenge("easy", 1, -1.0)).unwrap();
    let service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"usage-key"),
        ServiceConfig::default(),
    );

    for (case, request) in [
        ("empty id", lesson_event("", 1, LessonOutcome::Opened)),
        (
            "oversized id",
            lesson_event(&"x".repeat(65), 1, LessonOutcome::Opened),
        ),
        (
            "id that is not a slug",
            lesson_event("../../etc/passwd", 1, LessonOutcome::Opened),
        ),
        (
            "section past any course",
            lesson_event("cutter-grid-fixed-axes", 5_000, LessonOutcome::Opened),
        ),
    ] {
        let error = service
            .record_lesson_event(request, Some("u-1"), true)
            .expect_err(case);
        assert!(
            matches!(error, ServiceError::ProgramInvalid { .. }),
            "{case}: {error:?}"
        );
    }

    let mut busy = lesson_event("cutter-grid-fixed-axes", 1, LessonOutcome::Completed);
    busy.tests = Some(1_000_000);
    assert!(service.record_lesson_event(busy, Some("u-1"), true).is_err());
}

#[tokio::test]
async fn a_lesson_event_says_so_when_the_deployment_collects_nothing() {
    // Collection is off by default, and a client that cannot tell the log is off
    // from the log being broken would have no way to report either.
    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(common::challenge("easy", 1, -1.0)).unwrap();
    let service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"usage-key"),
        ServiceConfig::default(),
    );

    let ack = service
        .record_lesson_event(
            lesson_event("cutter-grid-distance", 4, LessonOutcome::QuizPassed),
            None,
            true,
        )
        .expect("accepted");
    assert!(!ack.recorded);
}

/// A new build appends to the log the previous one left, and changes none of it.
///
/// This is the property a deploy rests on and the one worth having a test for,
/// because the reassuring version of it — "the file is opened in append mode" —
/// is a claim about code, and what an operator needs is a claim about the file.
/// So: put rows written by an older build in place, run this build against the
/// same path, and check the old bytes are still there, byte for byte, with the
/// new row after them.
#[tokio::test]
async fn an_upgrade_appends_to_the_log_the_previous_build_left() {
    let dir = std::env::temp_dir().join(format!("hcr-upgrade-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("scratch dir");
    let path = dir.join("usage.jsonl");

    // What the running server has already collected, in the format it wrote:
    // no `scoringDigest`, and `matchResults` rows duplicated the way recording
    // per read left them.
    let existing = std::fs::read_to_string("tests/fixtures/usage-legacy.jsonl").expect("fixture");
    std::fs::write(&path, &existing).expect("seed the log");

    let catalog = Arc::new(CatalogStore::new());
    catalog.insert(common::challenge("easy", 1, -1.0)).unwrap();
    let service = HcrService::new(
        catalog,
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"upgrade-key"),
        ServiceConfig::default(),
    )
    .with_usage_log(Arc::new(UsageLog::open(&path).expect("open the existing log")));

    service
        .create_submission_for(
            submission("sub-after-upgrade", "easy", 1, safe_program()),
            Some("u-after"),
            true,
        )
        .await
        .unwrap();

    let after = std::fs::read_to_string(&path).expect("log still readable");

    // 1. Nothing was rewritten, truncated or reordered: the old file is a
    //    prefix of the new one.
    assert!(
        after.starts_with(&existing),
        "the previous build's rows must survive a deploy untouched"
    );

    // 2. Every one of those rows still parses as what it was.
    for line in existing.lines().filter(|line| !line.trim().is_empty()) {
        serde_json::from_str::<UsageEvent>(line).expect("a pre-upgrade row still parses");
    }

    // 3. The new row is appended, and is the new format.
    let appended: Vec<&str> = after[existing.len()..]
        .lines()
        .filter(|line| !line.trim().is_empty())
        .collect();
    assert_eq!(appended.len(), 1, "one submission, one new row: {appended:?}");
    let new_row: serde_json::Value = serde_json::from_str(appended[0]).expect("json");
    assert_eq!(new_row["playerId"], "u-after");
    assert!(new_row["scoringDigest"].is_string(), "{new_row}");

    let _ = std::fs::remove_dir_all(&dir);
}
