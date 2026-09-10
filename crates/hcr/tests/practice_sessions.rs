//! Practice sessions do not run out; measurement sessions still refuse to
//! guess.
//!
//! A class worked through the whole bank in one sitting. The fix is not a
//! bigger bank — it is a session that may synthesise an item when the pool has
//! nothing at the right difficulty. That permission is exactly what a
//! *measurement* session must never have, so both halves are pinned here.

use std::sync::Arc;

use hcr::*;
use hcr_contract::*;
use hcr_sim::ReplayOptions;

fn service_with_generator() -> HcrService {
    HcrService::new(
        seed::seed_catalog(),
        Arc::new(ReplayPool::new(2, ReplayOptions::default())),
        ItemRefSigner::new(*b"practice-session-signing-key"),
        ServiceConfig {
            generator: Some(seed::challenge_generator()),
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
async fn distinct_challenges(
    service: &HcrService,
    session_id: &str,
    ceiling: usize,
) -> std::collections::BTreeSet<String> {
    let mut seen = std::collections::BTreeSet::new();
    for round in 0..ceiling {
        let Ok(item) = service.next_item(session_id).await else {
            break;
        };
        seen.insert(item.challenge_id.clone());

        // A session that is awaiting a response re-serves the same item, so the
        // walk only advances if each one is actually answered.
        let submission_id = format!("p-{round}");
        if service
            .create_submission(attempt(&submission_id, &item.challenge_id))
            .await
            .is_err()
        {
            break;
        }
        if service
            .respond(SessionRespond {
                session_id: session_id.into(),
                item_ref: item.item_ref.clone(),
                submission_id,
            })
            .await
            .is_err()
        {
            break;
        }
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
        session_id: None,
        match_id: None,
        client_preview: None,
    }
}

#[tokio::test]
async fn a_practice_session_keeps_serving_past_the_calibrated_bank() {
    let service = service_with_generator();
    let snapshot = service
        .start_session(SessionStart {
            practice: true,
            ..SessionStart::default()
        })
        .await
        .expect("start");

    // The catalog holds 12 servo items. A practice session that stopped there
    // would be the same dead end the class already hit.
    let seen = distinct_challenges(&service, &snapshot.session_id, 40).await;
    println!("practice reached {} distinct challenges", seen.len());
    assert!(
        seen.len() > 12,
        "practice reached {} distinct challenges; the catalog alone holds 12, so \
         nothing was generated",
        seen.len(),
    );
}

#[tokio::test]
async fn a_measurement_session_still_refuses_to_synthesise() {
    let service = service_with_generator();
    let snapshot = service
        .start_session(SessionStart::default())
        .await
        .expect("start");

    // Not a limitation to work around: an ability estimate resting on an
    // uncalibrated item is not a measurement.
    let seen = distinct_challenges(&service, &snapshot.session_id, 40).await;
    assert!(
        seen.len() <= 12,
        "a measurement session reached {} distinct challenges, so it synthesised one",
        seen.len(),
    );
}

#[tokio::test]
async fn a_cutter_grid_practice_session_never_synthesises() {
    let service = service_with_generator();
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
    let seen = distinct_challenges(&service, &snapshot.session_id, 40).await;
    assert!(
        seen.len() <= 9,
        "a Cutter Grid practice session reached {} distinct challenges; only 9 carry \
         a certified profile",
        seen.len(),
    );
    // And every one of them must actually be playable in the mode.
    for id in &seen {
        let dto = service
            .get_challenge(id, None)
            .unwrap_or_else(|_| panic!("{id} is served but not fetchable"));
        assert!(
            dto.meta.supports(ProgrammingMode::CutterGrid),
            "{id} was served into a Cutter Grid session without supporting the mode",
        );
    }
}
