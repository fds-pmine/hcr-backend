//! Practice sessions do not run out; measurement sessions still refuse to
//! guess.
//!
//! A class worked through the whole bank in one sitting. The fix is not a
//! bigger bank — it is a session that may synthesise an item when the pool has
//! nothing at the right difficulty. That permission is exactly what a
//! *measurement* session must never have, so both halves are pinned here.

use std::collections::BTreeSet;
use std::sync::Arc;

use hcr::*;
use hcr_contract::*;
use hcr_sim::ReplayOptions;

fn service(generate: bool) -> HcrService {
    HcrService::new(
        seed::seed_catalog(),
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"practice-session-signing-key"),
        ServiceConfig {
            generator: generate.then(seed::challenge_generator),
            ..ServiceConfig::default()
        },
    )
}

/// Distinct challenges a session will serve, up to `ceiling` requests.
///
/// Counting *calls* would prove nothing: a session re-serves from its pool
/// rather than refusing, so the interesting number is how many different
/// hairstyles it can reach. Anything past the catalog is necessarily one the
/// generator made.
///
/// Only the legitimate end of a session stops the walk; any other error fails
/// the test, so an item that is served but unplayable cannot hide (Q51).
async fn distinct_challenges(
    service: &HcrService,
    session_id: &str,
    ceiling: usize,
    submit: fn(&str, &str) -> SubmissionCreate,
) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    for round in 0..ceiling {
        let item = match service.next_item(session_id).await {
            Ok(item) => item,
            Err(ServiceError::SessionTerminated | ServiceError::BankExhausted) => break,
            Err(other) => panic!("next_item failed: {other:?}"),
        };
        seen.insert(item.challenge_id.clone());

        // A session that is awaiting a response re-serves the same item, so the
        // walk only advances if each one is actually answered.
        let submission_id = format!("p-{round}");
        let mut request = submit(&submission_id, &item.challenge_id);
        request.session_id = Some(session_id.into());
        service.create_submission(request).await.unwrap_or_else(|e| {
            panic!("{} was served but cannot be submitted: {e:?}", item.challenge_id)
        });
        service
            .respond(SessionRespond {
                session_id: session_id.into(),
                item_ref: item.item_ref.clone(),
                submission_id,
            })
            .await
            .expect("respond to the served item");
    }
    seen
}

/// A minimal scoreable attempt. What it scores does not matter here; that an
/// item was answered at all is what lets the session move on.
fn attempt(id: &str, challenge_id: &str) -> SubmissionCreate {
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
        cutter_grid_v4: None,
        session_id: None,
        match_id: None,
        client_preview: None,
    }
}

/// The bundled Profile's certified perfect cut, the one V4 program known to
/// plan; mirrors `cutter_modes.rs`.
fn v4_attempt(id: &str, challenge_id: &str) -> SubmissionCreate {
    let profile: CutterGridProfileV4 =
        serde_json::from_str(include_str!("../assets/cutter-grid-profile-v4.json"))
            .expect("bundled V4 profile parses");
    let program = profile.reference_program;
    SubmissionCreate {
        program: Program {
            nodes: Vec::new(),
            source_block_count: program.source_block_count,
        },
        cutter_grid_v4: Some(program),
        ..attempt(id, challenge_id)
    }
}

// Pins Q1 and that practice reaches the generator: the 13th item is generated and unsubmittable. Drop should_panic when generated items become submittable.
#[tokio::test]
#[should_panic(expected = "was served but cannot be submitted")]
async fn a_practice_session_keeps_serving_past_the_calibrated_bank() {
    let service = service(true);
    let snapshot = service
        .start_session(SessionStart {
            practice: true,
            ..SessionStart::default()
        })
        .await
        .expect("start");

    // The catalog holds 12 servo items. A practice session that stopped there
    // would be the same dead end the class already hit.
    distinct_challenges(&service, &snapshot.session_id, 40, attempt).await;
}

#[tokio::test]
async fn practice_without_a_generator_ends_with_bank_exhausted() {
    let service = service(false);
    let id = service
        .start_session(SessionStart {
            practice: true,
            ..SessionStart::default()
        })
        .await
        .expect("start")
        .session_id;

    // 1 shipped + 8 lessons + 3 generated catalog items.
    assert_eq!(distinct_challenges(&service, &id, 40, attempt).await.len(), 12);
    assert!(matches!(
        service.next_item(&id).await,
        Err(ServiceError::BankExhausted)
    ));
}

#[tokio::test]
async fn a_measurement_session_still_refuses_to_synthesise() {
    let service = service(true);
    let snapshot = service
        .start_session(SessionStart::default())
        .await
        .expect("start");

    // Not a limitation to work around: an ability estimate resting on an
    // uncalibrated item is not a measurement.
    let seen = distinct_challenges(&service, &snapshot.session_id, 40, attempt).await;
    assert!(
        seen.len() <= 12,
        "a measurement session reached {} distinct challenges, so it synthesised one",
        seen.len(),
    );
}

#[tokio::test]
async fn a_cutter_grid_practice_session_never_synthesises() {
    let service = service(true);
    let snapshot = service
        .start_session(SessionStart {
            practice: true,
            programming_mode: ProgrammingMode::CutterGrid,
            ..SessionStart::default()
        })
        .await
        .expect("start");

    // The generator emits servo-only `Cap Trim` items. Synthesising one into a
    // Cutter Grid session would serve a challenge with no certified profile —
    // unplannable, and impossible to finish.
    let seen = distinct_challenges(&service, &snapshot.session_id, 40, v4_attempt).await;
    assert_eq!(seen, BTreeSet::from(["neat-short-cap".to_owned()]));
}
