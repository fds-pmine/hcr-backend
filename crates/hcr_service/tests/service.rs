//! Service behaviour, exercised without any transport.

use std::sync::Arc;

use hcr_contract::*;
use hcr_qbank::{Blueprint, ExposureController, SessionConfig};
use hcr_service::*;
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
    assert!(result.replay.engine_version.starts_with("hcr_sim/"));
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
            .create_submission(submission(
                &submission_id,
                &item.challenge_id,
                item.challenge_version,
                safe_program(),
            ))
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
        .create_submission(submission("sub-other", other, 1, safe_program()))
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
        .create_submission(submission(
            "sub-a",
            &item.challenge_id,
            item.challenge_version,
            safe_program(),
        ))
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
        .create_submission_for(submission("sub-1", "easy", 1, safe_program()), Some("u-1"))
        .await;
    assert!(result.is_ok());
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
