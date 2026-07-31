//! Calibration pipeline.
//!
//! The load-bearing tests here generate responses from **known** `(a, b, δ)` and
//! check the pipeline recovers those values. Anything weaker only proves the
//! fitter is self-consistent, which is exactly what a broken fitter also is.

use hcr_contract::{CalibrationState, ItemParameters};
use hcr_qbank::calibration::*;
use rand::prelude::*;
use rand::rngs::StdRng;

/// Abilities spread across the usable scale, as a real anchor sample would be.
fn thetas(count: usize) -> Vec<f64> {
    (0..count)
        .map(|index| -2.5 + 5.0 * (index as f64) / ((count - 1) as f64))
        .collect()
}

/// Sample responses from the true model.
fn responses(
    a: f64,
    b: f64,
    delta: f64,
    mode: Mode,
    abilities: &[f64],
    seed: u64,
) -> Vec<Observation> {
    let mut rng = StdRng::seed_from_u64(seed);
    abilities
        .iter()
        .map(|theta| {
            let shift = if mode == Mode::Match { delta } else { 0.0 };
            let p = 1.0 / (1.0 + (-(a * (theta - b - shift))).exp());
            Observation::new(*theta, rng.gen_bool(p), mode)
        })
        .collect()
}

fn params(a: f64, b: f64) -> ItemParameters {
    ItemParameters {
        discrimination: a,
        difficulty: b,
        guessing: 0.0,
    }
}

fn settings() -> CalibrationSettings {
    CalibrationSettings::default()
}

// ---------------------------------------------------------------------------
// Difficulty recovery
// ---------------------------------------------------------------------------

#[test]
fn refit_recovers_a_known_difficulty() {
    let (true_a, true_b) = (1.2, 0.8);
    let observations = responses(true_a, true_b, 0.0, Mode::Solo, &thetas(3000), 42);

    // Start deliberately wrong so convergence is doing real work.
    let fit = refit_difficulty(&observations, true_a, 0.0, 0.0, &settings());

    assert!(fit.converged && !fit.separated);
    assert!(
        (fit.difficulty - true_b).abs() < 3.0 * fit.se_difficulty,
        "recovered {} vs true {true_b} (se {})",
        fit.difficulty,
        fit.se_difficulty
    );
    assert!(
        (fit.difficulty - true_b).abs() < 0.15,
        "recovered {}",
        fit.difficulty
    );
}

#[test]
fn refit_converges_from_either_side() {
    let (true_a, true_b) = (1.0, -0.6);
    let observations = responses(true_a, true_b, 0.0, Mode::Solo, &thetas(3000), 7);

    let from_below = refit_difficulty(&observations, true_a, -2.5, 0.0, &settings());
    let from_above = refit_difficulty(&observations, true_a, 2.5, 0.0, &settings());

    assert!(from_below.converged && from_above.converged);
    assert!(
        (from_below.difficulty - from_above.difficulty).abs() < 1e-4,
        "the likelihood is concave, so both starts must reach the same maximum: \
         {} vs {}",
        from_below.difficulty,
        from_above.difficulty
    );
}

#[test]
fn joint_refit_recovers_discrimination_and_difficulty() {
    let (true_a, true_b) = (1.5, 0.4);
    let observations = responses(true_a, true_b, 0.0, Mode::Solo, &thetas(6000), 11);

    let fit = refit_item(&observations, 1.0, 0.0, 0.0, &settings());

    assert!(fit.converged);
    assert!(
        (fit.difficulty - true_b).abs() < 0.15,
        "difficulty {} vs {true_b}",
        fit.difficulty
    );
    assert!(
        (fit.discrimination - true_a).abs() < 0.25,
        "discrimination {} vs {true_a}",
        fit.discrimination
    );
}

#[test]
fn joint_refit_reports_failure_when_ability_has_no_spread() {
    // Discrimination is the slope of the response curve in θ; with every
    // respondent at the same θ there is no slope to estimate.
    let flat = vec![0.0; 500];
    let observations = responses(1.0, 0.0, 0.0, Mode::Solo, &flat, 3);

    let fit = refit_item(&observations, 1.0, 0.0, 0.0, &settings());
    assert!(!fit.converged || !fit.se_difficulty.is_finite());
}

// ---------------------------------------------------------------------------
// Degenerate input
// ---------------------------------------------------------------------------

#[test]
fn all_correct_or_all_wrong_is_reported_as_separation() {
    let abilities = thetas(200);
    let all_correct: Vec<_> = abilities
        .iter()
        .map(|t| Observation::new(*t, true, Mode::Solo))
        .collect();

    let fit = refit_difficulty(&all_correct, 1.0, 0.0, 0.0, &settings());

    assert!(fit.separated, "uniform responses must be flagged");
    assert!(!fit.is_usable(), "a separated fit must not be acted on");
}

#[test]
fn an_empty_sample_leaves_parameters_untouched() {
    let fit = refit_difficulty(&[], 1.0, 0.42, 0.0, &settings());

    assert_eq!(fit.difficulty, 0.42);
    assert_eq!(fit.observations, 0);
    assert!(!fit.is_usable());
}

// ---------------------------------------------------------------------------
// The identification argument, as a test
// ---------------------------------------------------------------------------

#[test]
fn difficulty_and_mode_offset_are_confounded_without_linking_items() {
    // An item observed ONLY under match conditions cannot separate its own
    // difficulty from the mode effect: the data constrain `b + δ`, nothing more.
    let (true_a, true_b, true_delta) = (1.2, 0.5, 0.7);
    let observations = responses(true_a, true_b, true_delta, Mode::Match, &thetas(3000), 5);

    // Assume no mode effect: the fit absorbs it into difficulty.
    let assuming_none = refit_difficulty(&observations, true_a, 0.0, 0.0, &settings());
    // Supply the true offset: the fit recovers the true difficulty.
    let with_offset = refit_difficulty(&observations, true_a, 0.0, true_delta, &settings());

    assert!(
        (assuming_none.difficulty - (true_b + true_delta)).abs() < 0.15,
        "with δ=0 the estimate should absorb the offset: got {}, expected ≈ {}",
        assuming_none.difficulty,
        true_b + true_delta
    );
    assert!(
        (with_offset.difficulty - true_b).abs() < 0.15,
        "with the true δ the estimate should recover b: got {}, expected ≈ {true_b}",
        with_offset.difficulty
    );

    // The two estimates differ by exactly the offset — which is precisely why
    // linking items are structurally required, not merely nice to have.
    assert!(
        ((assuming_none.difficulty - with_offset.difficulty) - true_delta).abs() < 0.1,
        "the shift should equal δ"
    );
}

#[test]
fn mode_offset_is_recovered_from_linking_items() {
    let true_delta = 0.6;
    let abilities = thetas(2500);

    let solo_a = responses(1.2, 0.0, 0.0, Mode::Solo, &abilities, 1);
    let match_a = responses(1.2, 0.0, true_delta, Mode::Match, &abilities, 2);
    let solo_b = responses(1.0, 0.9, 0.0, Mode::Solo, &abilities, 3);
    let match_b = responses(1.0, 0.9, true_delta, Mode::Match, &abilities, 4);

    let mut all_a = solo_a.clone();
    all_a.extend(match_a);
    let mut all_b = solo_b.clone();
    all_b.extend(match_b);

    // Difficulty pinned by solo data, exactly as stage A does it.
    let fit_a = refit_difficulty(&solo_a, 1.2, 0.0, 0.0, &settings());
    let fit_b = refit_difficulty(&solo_b, 1.0, 0.0, 0.0, &settings());

    let linking = [
        LinkingItem {
            discrimination: 1.2,
            difficulty: fit_a.difficulty,
            observations: &all_a,
        },
        LinkingItem {
            discrimination: 1.0,
            difficulty: fit_b.difficulty,
            observations: &all_b,
        },
    ];

    let offset = estimate_mode_offset(&linking, 0.0, &settings());

    assert!(offset.converged);
    assert_eq!(offset.linking_items, 2);
    assert!(
        (offset.offset - true_delta).abs() < 0.12,
        "recovered δ = {} vs true {true_delta} (se {})",
        offset.offset,
        offset.se
    );
}

#[test]
fn mode_offset_reports_nothing_when_there_are_no_match_responses() {
    let solo = responses(1.0, 0.0, 0.0, Mode::Solo, &thetas(100), 1);
    let linking = [LinkingItem {
        discrimination: 1.0,
        difficulty: 0.0,
        observations: &solo,
    }];

    let offset = estimate_mode_offset(&linking, 0.0, &settings());
    assert_eq!(offset.observations, 0);
    assert!(!offset.converged);
}

// ---------------------------------------------------------------------------
// End to end
// ---------------------------------------------------------------------------

#[test]
fn the_pipeline_recovers_both_the_offset_and_item_difficulties() {
    let true_delta = 0.55;
    let abilities = thetas(2500);

    // Two linking items, each served in both conditions.
    let linking: Vec<ItemObservations> = [("link-easy", 1.2, -0.4), ("link-hard", 1.1, 0.7)]
        .iter()
        .enumerate()
        .map(|(index, (id, a, b))| {
            let mut observations = responses(*a, *b, 0.0, Mode::Solo, &abilities, 100 + index as u64);
            observations.extend(responses(
                *a,
                *b,
                true_delta,
                Mode::Match,
                &abilities,
                200 + index as u64,
            ));
            ItemObservations {
                item_id: (*id).to_string(),
                params: params(*a, 0.0),
                state: CalibrationState::Online,
                observations,
            }
        })
        .collect();

    // Match-only items, whose difficulty is only recoverable once δ is known.
    let truth = [("match-a", 1.0, 0.2), ("match-b", 1.3, -0.8)];
    let others: Vec<ItemObservations> = truth
        .iter()
        .enumerate()
        .map(|(index, (id, a, b))| ItemObservations {
            item_id: (*id).to_string(),
            params: params(*a, 0.0),
            state: CalibrationState::Provisional,
            observations: responses(
                *a,
                *b,
                true_delta,
                Mode::Match,
                &abilities,
                300 + index as u64,
            ),
        })
        .collect();

    let pipeline = CalibrationPipeline::default();
    let report = pipeline.run(&linking, &others);

    assert!(report.mode_offset.converged);
    assert!(
        (report.mode_offset.offset - true_delta).abs() < 0.12,
        "δ = {}",
        report.mode_offset.offset
    );

    for (id, _, true_b) in truth {
        let fit = report
            .items
            .iter()
            .find(|item| item.item_id == id)
            .unwrap_or_else(|| panic!("{id} missing from the report"));
        assert!(
            (fit.after.difficulty - true_b).abs() < 0.2,
            "{id}: recovered {} vs true {true_b}",
            fit.after.difficulty
        );
    }
}

#[test]
fn an_unconverged_offset_is_not_applied() {
    // With no linking items there is nothing to identify δ from. Applying a
    // fabricated offset would shift every match-observed item arbitrarily.
    let abilities = thetas(600);
    let others = vec![ItemObservations {
        item_id: "solo-only".to_string(),
        params: params(1.0, 0.0),
        state: CalibrationState::Online,
        observations: responses(1.0, 0.5, 0.0, Mode::Solo, &abilities, 9),
    }];

    let report = CalibrationPipeline::default().run(&[], &others);

    assert!(!report.mode_offset.converged);
    assert_eq!(report.mode_offset.offset, 0.0);
    // The item is still fitted, just without a mode correction.
    assert!((report.items[0].after.difficulty - 0.5).abs() < 0.2);
}

// ---------------------------------------------------------------------------
// Fit quality, drift and lifecycle
// ---------------------------------------------------------------------------

#[test]
fn outfit_is_near_one_for_model_conforming_data() {
    let observations = responses(1.2, 0.3, 0.0, Mode::Solo, &thetas(3000), 21);
    let value = outfit(&observations, 1.2, 0.3, 0.0);
    assert!(
        (0.8..1.25).contains(&value),
        "conforming data should sit near 1, got {value}"
    );
}

#[test]
fn outfit_is_large_when_responses_contradict_the_model() {
    // Invert every response: able people fail, weak people pass.
    let conforming = responses(1.5, 0.0, 0.0, Mode::Solo, &thetas(1500), 33);
    let inverted: Vec<_> = conforming
        .iter()
        .map(|o| Observation::new(o.theta, !o.correct, o.mode))
        .collect();

    let value = outfit(&inverted, 1.5, 0.0, 0.0);
    assert!(value > 2.0, "contradictory data should misfit badly, got {value}");
}

#[test]
fn drift_is_detected_when_difficulty_shifts_midstream() {
    let abilities = thetas(1200);
    let mut stream = responses(1.2, -0.5, 0.0, Mode::Solo, &abilities, 51);
    stream.extend(responses(1.2, 1.0, 0.0, Mode::Solo, &abilities, 52));

    let report = detect_drift(&stream, 1.2, 0.0, 0.0, &settings()).expect("drift report");

    assert!(report.is_drifting(), "z = {}", report.z);
    assert!(report.late_difficulty > report.early_difficulty);
}

#[test]
fn a_stable_item_does_not_report_drift() {
    let abilities = thetas(1200);
    let mut stream = responses(1.2, 0.3, 0.0, Mode::Solo, &abilities, 61);
    stream.extend(responses(1.2, 0.3, 0.0, Mode::Solo, &abilities, 62));

    let report = detect_drift(&stream, 1.2, 0.3, 0.0, &settings()).expect("drift report");
    assert!(!report.is_drifting(), "z = {}", report.z);
}

#[test]
fn promotion_follows_sample_size_and_precision() {
    let policy = PromotionPolicy::default();
    let usable = |n: usize, se: f64| FitResult {
        difficulty: 0.0,
        discrimination: 1.0,
        se_difficulty: se,
        observations: n,
        iterations: 3,
        converged: true,
        separated: false,
    };

    // Too little data to leave Provisional.
    assert_eq!(
        policy.next_state(CalibrationState::Provisional, &usable(10, 0.3), 1.0),
        CalibrationState::Provisional
    );
    // Enough for Online.
    assert_eq!(
        policy.next_state(CalibrationState::Provisional, &usable(50, 0.4), 1.0),
        CalibrationState::Online
    );
    // Enough for Calibrated.
    assert_eq!(
        policy.next_state(CalibrationState::Online, &usable(500, 0.2), 1.0),
        CalibrationState::Calibrated
    );
    // Plenty of data but imprecise: no promotion.
    assert_eq!(
        policy.next_state(CalibrationState::Online, &usable(500, 0.9), 1.0),
        CalibrationState::Online
    );
}

#[test]
fn misfitting_or_undiscriminating_items_are_retired() {
    let policy = PromotionPolicy::default();
    let fit = |a: f64| FitResult {
        difficulty: 0.0,
        discrimination: a,
        se_difficulty: 0.1,
        observations: 500,
        iterations: 3,
        converged: true,
        separated: false,
    };

    assert_eq!(
        policy.next_state(CalibrationState::Calibrated, &fit(0.1), 1.0),
        CalibrationState::Retired,
        "an item that barely discriminates is not measuring anything"
    );
    assert_eq!(
        policy.next_state(CalibrationState::Calibrated, &fit(1.2), 5.0),
        CalibrationState::Retired,
        "severe misfit retires the item"
    );
    // Retirement is one-way.
    assert_eq!(
        policy.next_state(CalibrationState::Retired, &fit(1.2), 1.0),
        CalibrationState::Retired
    );
}

#[test]
fn an_unusable_fit_never_moves_the_lifecycle() {
    let policy = PromotionPolicy::default();
    let separated = FitResult {
        difficulty: 3.0,
        discrimination: 1.0,
        se_difficulty: f64::INFINITY,
        observations: 5000,
        iterations: 1,
        converged: true,
        separated: true,
    };

    assert_eq!(
        policy.next_state(CalibrationState::Provisional, &separated, 1.0),
        CalibrationState::Provisional
    );
}

#[test]
fn a_material_difficulty_change_requires_a_new_version() {
    let abilities = thetas(2000);
    let items = vec![ItemObservations {
        item_id: "moved".to_string(),
        params: params(1.2, 0.0),
        state: CalibrationState::Online,
        // Truth is far from the stored parameter.
        observations: responses(1.2, 1.1, 0.0, Mode::Solo, &abilities, 71),
    }];

    let report = CalibrationPipeline::default().run(&[], &items);
    let fit = &report.items[0];

    assert!(fit.needs_version_bump, "Δb = {}", fit.after.difficulty - fit.before.difficulty);
    assert_eq!(report.version_bumps().count(), 1);
}

#[test]
fn a_stable_item_does_not_churn_versions() {
    let abilities = thetas(2000);
    let truth = 0.35;
    // Store a parameter already at the truth.
    let items = vec![ItemObservations {
        item_id: "stable".to_string(),
        params: params(1.2, truth),
        state: CalibrationState::Online,
        observations: responses(1.2, truth, 0.0, Mode::Solo, &abilities, 81),
    }];

    let report = CalibrationPipeline::default().run(&[], &items);
    assert!(
        !report.items[0].needs_version_bump,
        "sampling noise alone must not mint versions: Δb = {}",
        report.items[0].after.difficulty - truth
    );
}

#[test]
fn double_fitting_a_linking_item_is_detectable() {
    // A linking item passed as an ordinary item too would be refitted in stage C
    // with a δ-contaminated estimate, destroying the identification the linking
    // set exists to provide.
    let make = |id: &str| ItemObservations {
        item_id: id.to_string(),
        params: params(1.0, 0.0),
        state: CalibrationState::Online,
        observations: vec![],
    };

    let linking = [make("link-a"), make("link-b")];
    let others = [make("link-b"), make("plain")];

    assert_eq!(overlapping_ids(&linking, &others), vec!["link-b".to_string()]);
    assert!(overlapping_ids(&linking, &[make("plain")]).is_empty());
}
