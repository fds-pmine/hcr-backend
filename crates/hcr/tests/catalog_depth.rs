//! What a Solo Practice session can actually be served.
//!
//! A class worked through the whole adaptive bank in one sitting and hit the
//! end of it, which is the failure these tests exist to stop coming back.
//! Depth is not the same as item count: an adaptive session refuses
//! `Provisional` items on purpose, so only calibrated ones count, and a bank
//! whose difficulties all sit in one place cannot adapt however many items it
//! holds.

use hcr::seed::seed_catalog;
use hcr_contract::*;

/// Items an adaptive session may actually measure with.
fn measurable(catalog: &hcr::catalog::CatalogStore, mode: ProgrammingMode) -> Vec<(String, f64)> {
    catalog
        .snapshot_for(mode)
        .expect("snapshot")
        .items()
        .iter()
        .filter(|item| item.meta.calibration != CalibrationState::Provisional)
        .map(|item| (item.id.as_str().to_string(), item.meta.irt.difficulty))
        .collect()
}

#[test]
fn solo_practice_has_more_than_a_single_sitting_of_items() {
    let catalog = seed_catalog();
    let items = measurable(&catalog, ProgrammingMode::Servo);
    assert!(
        items.len() >= 12,
        "adaptive Servo bank is {} items; a class finishes four in well under an hour",
        items.len(),
    );
}

#[test]
fn the_eight_authored_lessons_are_all_servable() {
    let catalog = seed_catalog();
    let ids: Vec<String> = measurable(&catalog, ProgrammingMode::Servo)
        .into_iter()
        .map(|(id, _)| id)
        .collect();
    for lesson in [
        "lesson-1-first-cut",
        "lesson-2-clear-the-head",
        "lesson-3-shoulder",
        "lesson-4-elbow",
        "lesson-5-wrist",
        "lesson-6-stop-short",
        "lesson-7-narrow-band",
        "lesson-8-full-cut",
    ] {
        assert!(
            ids.iter().any(|id| id == lesson),
            "{lesson} is missing from the adaptive bank",
        );
    }
}

#[test]
fn the_bank_spans_enough_difficulty_to_adapt() {
    let catalog = seed_catalog();
    let difficulties: Vec<f64> = measurable(&catalog, ProgrammingMode::Servo)
        .into_iter()
        .map(|(_, b)| b)
        .collect();
    let low = difficulties.iter().copied().fold(f64::INFINITY, f64::min);
    let high = difficulties.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    // The generated family alone spans about [-0.92, -0.57]: an ability
    // estimate outside that has nothing to aim at, so the session stops
    // telling the learner anything.
    assert!(
        high - low >= 2.0,
        "difficulty spans only {low:+.2}..{high:+.2}; an adaptive session needs room to move",
    );
}

#[test]
fn cutter_grid_has_a_bank_rather_than_a_single_challenge() {
    // A one-item Cutter Grid bank means an adaptive session ends after one
    // challenge and a versus room repeats itself on the second round. Each item
    // here ships a certified profile; a challenge advertising the mode without
    // one is a task nobody can finish, so this count and the profile directory
    // have to change together.
    let catalog = seed_catalog();
    let grid = measurable(&catalog, ProgrammingMode::CutterGrid);
    assert!(
        grid.len() >= 9,
        "Cutter Grid bank is {} items: {:?}",
        grid.len(),
        grid,
    );
}
