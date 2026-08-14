//! Behaviour of the dynamic bank and its arona integration.

use std::collections::HashMap;

use arona::qbank::{QBankError, QuestionBank};
use arona::selection::SelectionHints;
use arona::selection::selectors::MaxInfoParams;
use arona::{Ability, RenderFormat};
use hcr_contract::{
    CalibrationState, ChallengeMeta, ItemParameters, ProgrammingMode, SkillDimension,
};
use hcr_qbank::*;

fn meta(
    b: f64,
    calibration: CalibrationState,
    dimensions: Vec<SkillDimension>,
) -> ChallengeMeta {
    ChallengeMeta {
        version: 1,
        irt: ItemParameters {
            discrimination: 1.2,
            difficulty: b,
            guessing: 0.0,
        },
        calibration,
        response_count: 250,
        dimensions,
        mastery_threshold: 0.5,
        generator: None,
        hardware_compatible: true,
        programming_modes: vec![ProgrammingMode::Servo],
    }
}

fn calibrated(b: f64) -> ChallengeMeta {
    meta(b, CalibrationState::Calibrated, vec![SkillDimension::Kinematics])
}

/// Items spread across the difficulty range.
fn spread_catalog() -> std::sync::Arc<CatalogSnapshot> {
    CatalogSnapshot::new(vec![
        BankItem::new("very-easy", calibrated(-2.0)),
        BankItem::new("easy", calibrated(-1.0)),
        BankItem::new("medium", calibrated(0.0)),
        BankItem::new("hard", calibrated(1.0)),
        BankItem::new("very-hard", calibrated(2.0)),
    ])
}

/// Hints as a max-information selector would produce them.
fn hints_at(theta: f64) -> SelectionHints {
    let mut hints = SelectionHints::new(Ability(theta));
    hints.params.set(MaxInfoParams {
        tolerance: 1.0,
        min_discrimination: 0.0,
    });
    hints
}

fn bank_over(catalog: std::sync::Arc<CatalogSnapshot>) -> HcrDynamicBank {
    HcrDynamicBank::new(catalog, OutcomeStore::new(), 7)
        // k=1 makes selection deterministic so tests assert on the ranking
        // itself rather than on a sample from it.
        .with_randomesque_k(1)
        .with_exposure(ExposureController::unlimited())
}

fn item_of(question: &arona::Question) -> String {
    question
        .content
        .render(RenderFormat::PlainText)
        .split('@')
        .next()
        .unwrap()
        .to_string()
}

// ---------------------------------------------------------------------------
// Selection
// ---------------------------------------------------------------------------

#[test]
fn selects_the_item_most_informative_at_the_current_ability() {
    for (theta, expected) in [(-2.0, "very-easy"), (0.0, "medium"), (2.0, "very-hard")] {
        let mut bank = bank_over(spread_catalog());
        let question = bank.select_question(&hints_at(theta)).unwrap();
        assert_eq!(item_of(&question), expected, "at theta={theta}");
    }
}

#[test]
fn never_serves_the_same_item_twice_in_a_session() {
    let mut bank = bank_over(spread_catalog());
    let mut seen = Vec::new();

    for _ in 0..5 {
        let question = bank.select_question(&hints_at(0.0)).unwrap();
        seen.push(item_of(&question));
    }

    let unique: std::collections::HashSet<_> = seen.iter().collect();
    assert_eq!(unique.len(), 5, "expected 5 distinct items, got {seen:?}");
}

#[test]
fn an_exhausted_bank_reports_no_question_available() {
    let mut bank = bank_over(spread_catalog());
    for _ in 0..5 {
        bank.select_question(&hints_at(0.0)).unwrap();
    }
    assert!(matches!(
        bank.select_question(&hints_at(0.0)),
        Err(QBankError::NoQuestionAvailable)
    ));
}

#[test]
fn reset_makes_every_item_available_again() {
    let mut bank = bank_over(spread_catalog());
    for _ in 0..5 {
        bank.select_question(&hints_at(0.0)).unwrap();
    }
    bank.reset();

    assert!(bank.select_question(&hints_at(0.0)).is_ok());
    assert!(bank.served().len() == 1, "reset should clear serve history");
}

#[test]
fn retired_items_are_never_served() {
    let catalog = CatalogSnapshot::new(vec![
        BankItem::new("live", calibrated(0.0)),
        BankItem::new(
            "dead",
            meta(0.0, CalibrationState::Retired, vec![SkillDimension::Safety]),
        ),
    ]);
    let mut bank = bank_over(catalog);

    let question = bank.select_question(&hints_at(0.0)).unwrap();
    assert_eq!(item_of(&question), "live");
    assert!(matches!(
        bank.select_question(&hints_at(0.0)),
        Err(QBankError::NoQuestionAvailable)
    ));
}

#[test]
fn uncalibrated_items_are_measurement_only_by_default() {
    let catalog = CatalogSnapshot::new(vec![BankItem::new(
        "fresh",
        meta(
            0.0,
            CalibrationState::Provisional,
            vec![SkillDimension::Precision],
        ),
    )]);

    // Adaptive measurement must not rest on an uncalibrated item.
    let mut strict = bank_over(catalog.clone());
    assert!(matches!(
        strict.select_question(&hints_at(0.0)),
        Err(QBankError::NoQuestionAvailable)
    ));

    // A competitive round may use it: ranking is valid whatever `b` is, because
    // every player faces the identical item.
    let mut relaxed = bank_over(catalog).allow_uncalibrated(true);
    assert!(relaxed.select_question(&hints_at(0.0)).is_ok());
}

#[test]
fn min_discrimination_from_the_hints_filters_weak_items() {
    let mut weak = calibrated(0.0);
    weak.irt.discrimination = 0.2;
    let catalog = CatalogSnapshot::new(vec![BankItem::new("weak", weak)]);

    let mut bank = bank_over(catalog);
    let mut hints = SelectionHints::new(Ability(0.0));
    hints.params.set(MaxInfoParams {
        tolerance: 1.0,
        min_discrimination: 0.5,
    });

    assert!(matches!(
        bank.select_question(&hints),
        Err(QBankError::NoQuestionAvailable)
    ));
}

#[test]
fn matching_the_learners_field_boosts_an_item() {
    // Two items equally informative at theta=0; only the tag differs.
    let catalog = CatalogSnapshot::new(vec![
        BankItem::new(
            "kinematics",
            meta(0.0, CalibrationState::Calibrated, vec![SkillDimension::Kinematics]),
        ),
        BankItem::new(
            "safety",
            meta(0.0, CalibrationState::Calibrated, vec![SkillDimension::Safety]),
        ),
    ]);

    let mut bank = bank_over(catalog);
    let hints = hints_at(0.0).with_user_field("safety".to_string());
    let question = bank.select_question(&hints).unwrap();

    assert_eq!(
        item_of(&question),
        "safety",
        "the field-matching item should win via the {FIELD_BOOST}x boost"
    );
}

#[test]
fn selection_is_reproducible_for_a_given_seed() {
    let sequence = |seed: u64| {
        let mut bank = HcrDynamicBank::new(spread_catalog(), OutcomeStore::new(), seed)
            .with_exposure(ExposureController::unlimited());
        (0..5)
            .map(|_| item_of(&bank.select_question(&hints_at(0.0)).unwrap()))
            .collect::<Vec<_>>()
    };

    // Same seed, same order — arona's own thread_rng()-based selection cannot
    // offer this, and it is what makes a session auditable.
    assert_eq!(sequence(11), sequence(11));
}

// ---------------------------------------------------------------------------
// Blueprint and exposure
// ---------------------------------------------------------------------------

#[test]
fn the_blueprint_excludes_a_saturated_dimension() {
    let catalog = CatalogSnapshot::new(vec![
        BankItem::new(
            "precision",
            meta(0.0, CalibrationState::Calibrated, vec![SkillDimension::Precision]),
        ),
        BankItem::new(
            "safety",
            meta(0.0, CalibrationState::Calibrated, vec![SkillDimension::Safety]),
        ),
    ]);

    let mut bank = bank_over(catalog).with_blueprint(Blueprint::uniform().with_tolerance(0.0));

    // Report precision as heavily over-served through `used_types` — the field
    // arona defines and StaticQBank never reads.
    let mut hints = hints_at(0.0);
    hints.used_types = HashMap::from([("precision".to_string(), 9u32)]);

    let question = bank.select_question(&hints).unwrap();
    assert_eq!(item_of(&question), "safety");
}

#[test]
fn exposure_control_withholds_an_overexposed_item() {
    let catalog = CatalogSnapshot::new(vec![
        BankItem::new("hot", calibrated(0.0)),
        BankItem::new("cold", calibrated(0.0)),
    ]);

    let mut exposure = ExposureController::new(0.2).with_warmup(0);
    for _ in 0..10 {
        exposure.record("hot");
    }

    let mut bank = HcrDynamicBank::new(catalog, OutcomeStore::new(), 3)
        .with_randomesque_k(1)
        .with_exposure(exposure);

    let question = bank.select_question(&hints_at(0.0)).unwrap();
    assert_eq!(item_of(&question), "cold");
}

// ---------------------------------------------------------------------------
// Item identity
// ---------------------------------------------------------------------------

#[test]
fn served_items_map_bank_indices_back_to_identity() {
    let mut bank = bank_over(spread_catalog());

    bank.select_question(&hints_at(-2.0)).unwrap();
    bank.select_question(&hints_at(2.0)).unwrap();

    // arona addresses items by Vec index and Question carries no id, so this map
    // is what turns a selection back into something nameable.
    assert_eq!(bank.last_selected_index(), Some(1));
    assert_eq!(bank.item_at(0).unwrap().id, "very-easy");
    assert_eq!(bank.item_at(1).unwrap().id, "very-hard");
    assert_eq!(bank.item_at(1).unwrap().version, 1);
    assert!(bank.item_at(2).is_none());
}

#[test]
fn stats_describe_the_servable_pool() {
    let catalog = CatalogSnapshot::new(vec![
        BankItem::new("easy", calibrated(-1.0)),
        BankItem::new("hard", calibrated(1.0)),
        BankItem::new(
            "dead",
            meta(9.0, CalibrationState::Retired, vec![SkillDimension::Safety]),
        ),
    ]);
    let mut bank = bank_over(catalog);

    let before = bank.stats();
    assert_eq!(before.total_questions, 3);
    assert_eq!(before.available_questions, 2, "retired items are not available");
    assert_eq!(before.used_questions, 0);
    // The retired item's absurd difficulty must not widen the reported range.
    assert_eq!(before.difficulty_range.0.0, -1.0);
    assert_eq!(before.difficulty_range.1.0, 1.0);
    assert!((before.avg_difficulty - 0.0).abs() < 1e-12);

    bank.select_question(&hints_at(0.0)).unwrap();
    let after = bank.stats();
    assert_eq!(after.used_questions, 1);
    assert_eq!(after.available_questions, 1);
}

#[test]
fn an_empty_catalog_produces_finite_stats() {
    // Infinities would leak into anything that reads QBankStats.
    let bank = bank_over(CatalogSnapshot::new(vec![]));
    let stats = bank.stats();

    assert_eq!(stats.total_questions, 0);
    assert!(stats.difficulty_range.0.0.is_finite());
    assert!(stats.discrimination_range.1.0.is_finite());
    assert!(stats.avg_difficulty.is_finite());
    assert!(stats.is_exhausted());
}

// ---------------------------------------------------------------------------
// Scoring path
// ---------------------------------------------------------------------------

#[test]
fn a_recorded_outcome_drives_the_verdict_through_the_mastery_threshold() {
    let mut demanding = calibrated(0.0);
    demanding.mastery_threshold = 0.8;

    let catalog = CatalogSnapshot::new(vec![BankItem::new("strict", demanding)]);
    let outcomes = OutcomeStore::new();
    let bank = HcrDynamicBank::new(catalog, outcomes.clone(), 5)
        .with_exposure(ExposureController::unlimited());
    let mut session = build_session(bank, SessionConfig::default(), 0.0);

    session.next_question().unwrap();

    // 0.79 is a good score but below this item's bar — and crucially it is above
    // 0.5, so a raw score fed straight to arona would have called it correct.
    outcomes.record("sub-1", 0.79);
    let result = session.submit_response("sub-1").unwrap();
    assert!(!result.correct, "0.79 must fail a 0.80 mastery threshold");
}

#[test]
fn a_score_above_the_threshold_counts_as_mastery_and_raises_theta() {
    let catalog = spread_catalog();
    let outcomes = OutcomeStore::new();
    let bank = HcrDynamicBank::new(catalog, outcomes.clone(), 5)
        .with_exposure(ExposureController::unlimited());
    let mut session = build_session(bank, SessionConfig::default(), 0.0);

    let mut theta = session.state().ability.0;
    for index in 0..3 {
        session.next_question().unwrap();
        let submission = format!("sub-{index}");
        outcomes.record(&submission, 0.95);
        let result = session.submit_response(&submission).unwrap();
        assert!(result.correct);
        assert!(
            result.new_ability.0 >= theta,
            "ability should not fall after a correct response"
        );
        theta = result.new_ability.0;
    }

    assert!(theta > 0.0, "three masteries should push theta above the prior mean");
    assert_eq!(outcomes.misses(), 0, "every submission had a recorded outcome");
}

#[test]
fn a_missing_outcome_scores_zero_and_is_counted() {
    // `score()` returns a Score, not a Result, so a missing outcome cannot raise
    // an error. It must not pass silently either.
    let catalog = CatalogSnapshot::new(vec![BankItem::new("item", calibrated(0.0))]);
    let outcomes = OutcomeStore::new();
    let bank = HcrDynamicBank::new(catalog, outcomes.clone(), 5)
        .with_exposure(ExposureController::unlimited());
    let mut session = build_session(bank, SessionConfig::default(), 0.0);

    session.next_question().unwrap();
    let result = session.submit_response("never-recorded").unwrap();

    assert!(!result.correct);
    assert_eq!(
        outcomes.misses(),
        1,
        "a missing replay outcome must be observable, not silent"
    );
}

#[test]
fn a_full_adaptive_session_terminates_and_finalizes() {
    let catalog = spread_catalog();
    let outcomes = OutcomeStore::new();
    let bank = HcrDynamicBank::new(catalog, outcomes.clone(), 5)
        .with_exposure(ExposureController::unlimited());
    let config = SessionConfig {
        min_items: 2,
        max_items: 4,
        ..SessionConfig::default()
    };
    let mut session = build_session(bank, config, 0.0);

    let mut served = 0;
    while !session.should_terminate() {
        if session.next_question().is_err() {
            break;
        }
        let submission = format!("sub-{served}");
        // Alternate outcomes so the estimate has both directions of evidence.
        outcomes.record(&submission, if served % 2 == 0 { 0.9 } else { 0.2 });
        session.submit_response(&submission).unwrap();
        served += 1;
    }

    assert!(served >= 2, "should serve at least min_items");
    assert!(served <= 4, "should never exceed max_items, served {served}");

    let outcome = session.finalize();
    assert_eq!(outcome.total_items, served);
    assert!(outcome.final_ability.0.is_finite());
    assert!(!outcome.termination_reason.is_empty());
}
