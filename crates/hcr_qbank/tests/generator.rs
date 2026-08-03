//! Item generation and the difficulty model.

use std::collections::BTreeSet;
use std::sync::Arc;

use arona::qbank::{QBankError, QuestionBank};
use arona::selection::SelectionHints;
use arona::selection::selectors::MaxInfoParams;
use arona::Ability;
use hcr_contract::*;
use hcr_qbank::*;

/// Prototype supplying geometry, lattice and scoring — the values shipped in
/// `src/data/challenges/defaultChallenge.ts`.
fn prototype() -> ChallengeDefinition {
    let joint = |id: &str, axis: Axis, min: f64, max: f64, initial: f64, speed: f64| JointConfig {
        id: id.into(),
        name: id.into(),
        axis,
        min_angle_deg: min,
        max_angle_deg: max,
        initial_angle_deg: initial,
        speed_deg_per_sec: speed,
    };

    ChallengeDefinition {
        id: "prototype".into(),
        name: "Prototype".into(),
        description: String::new(),
        robot_config: RobotConfig {
            joints: vec![
                joint("baseYaw", Axis::Y, -60.0, 60.0, -45.0, 60.0),
                joint("shoulderRoll", Axis::X, -45.0, 45.0, 0.0, 45.0),
                joint("shoulder", Axis::Z, -20.0, 100.0, 45.0, 45.0),
                joint("elbow", Axis::Z, -135.0, 10.0, -80.0, 60.0),
                joint("wrist", Axis::Z, -100.0, 100.0, 35.0, 75.0),
            ],
            geometry: RobotGeometryConfig {
                base_position: [0.0, 0.0, 0.0],
                shoulder_height: 0.4,
                upper_arm_length: 1.05,
                forearm_length: 0.9,
                tool_length: 0.35,
                tool_radius: 0.12,
                collision: RobotCollisionConfig {
                    link_radius: 0.075,
                    joint_radius: 0.18,
                    tool_shaft_radius: 0.075,
                    head_clearance: 0.02,
                },
            },
        },
        voxel_config: VoxelConfig {
            origin: [1.35, 1.5, 0.0],
            size: 0.16,
            head_center: [1.35, 1.42, 0.0],
            head_scale: [0.68, 0.86, 0.68],
        },
        initial_hair: HairstyleDefinition {
            id: "i".into(),
            name: "i".into(),
            voxels: vec![],
        },
        target_hair: HairstyleDefinition {
            id: "t".into(),
            name: "t".into(),
            voxels: vec![],
        },
        allowed_blocks: vec![
            AllowedBlockType::SetJointAngle,
            AllowedBlockType::Wait,
            AllowedBlockType::Repeat,
        ],
        starter_workspace: None,
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
    }
}

fn params(thickness: f64, depth: f64, span: f64, turn: f64) -> ParamVector {
    ParamVector::from([
        (CapTrimGenerator::CAP_THICKNESS.to_string(), thickness),
        (CapTrimGenerator::TRIM_DEPTH.to_string(), depth),
        (CapTrimGenerator::REGION_SPAN.to_string(), span),
        (CapTrimGenerator::REGION_TURN.to_string(), turn),
    ])
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------

#[test]
fn generation_produces_a_usable_challenge() {
    let generator = CapTrimGenerator::new(prototype());
    let challenge = generator.generate(1, &params(2.0, 1.0, 0.6, 0.0)).unwrap();

    assert!(!challenge.initial_hair.voxels.is_empty());
    assert!(
        challenge.target_hair.voxels.len() < challenge.initial_hair.voxels.len(),
        "there must be something to cut"
    );

    // The target must be a subset of the initial hair: cutting never adds hair.
    let initial: BTreeSet<_> = challenge.initial_hair.voxels.iter().copied().collect();
    let target: BTreeSet<_> = challenge.target_hair.voxels.iter().copied().collect();
    assert!(target.is_subset(&initial));
}

#[test]
fn generation_is_deterministic() {
    // The whole audit story depends on this: `(family, version, seed, params)`
    // must reproduce the same challenge forever.
    let generator = CapTrimGenerator::new(prototype());
    // `baseYaw` travels ±60°, so the turn has to name a sector the arm can
    // actually sweep — an unreachable one is now refused rather than generated.
    let vector = params(2.0, 1.0, 0.55, 0.1);

    let first = generator.generate(42, &vector).unwrap();
    let second = generator.generate(42, &vector).unwrap();

    assert_eq!(first.id, second.id);
    assert_eq!(first.initial_hair.voxels, second.initial_hair.voxels);
    assert_eq!(first.target_hair.voxels, second.target_hair.voxels);
}

#[test]
fn different_parameters_produce_different_challenges() {
    let generator = CapTrimGenerator::new(prototype());
    let narrow = generator.generate(1, &params(2.0, 1.0, 0.3, 0.0)).unwrap();
    let wide = generator.generate(1, &params(2.0, 1.0, 1.0, 0.0)).unwrap();

    assert!(
        wide.target_hair.voxels.len() < narrow.target_hair.voxels.len(),
        "a wider sector should remove more hair"
    );
}

#[test]
fn degenerate_parameters_are_rejected_rather_than_served() {
    let generator = CapTrimGenerator::new(prototype());
    // Zero trim depth leaves the cap untouched, so there is no task at all.
    let result = generator.generate(1, &params(2.0, 0.0, 0.6, 0.0));
    assert_eq!(result.unwrap_err(), GenError::NothingToRemove);

    // A zero-width sector gives the reference nothing to sweep across, so no
    // program defines a target. The family samples span in [0.25, 1.0]; this is
    // reachable only by calling `generate` directly.
    let sliver = generator.generate(1, &params(2.0, 1.0, 0.0, 0.0));
    assert_eq!(sliver.unwrap_err(), GenError::NothingToRemove);

    // A sector centred behind the head is outside `baseYaw`'s ±60° travel. It
    // used to be generated anyway, producing an item asking for hair no program
    // could touch.
    let behind = generator.generate(1, &params(2.0, 1.0, 0.6, 0.5));
    assert_eq!(behind.unwrap_err(), GenError::UnreachableSector);
}

#[test]
fn a_missing_parameter_is_reported_by_name() {
    let generator = CapTrimGenerator::new(prototype());
    let mut incomplete = params(2.0, 1.0, 0.5, 0.0);
    incomplete.remove(CapTrimGenerator::REGION_SPAN);

    assert_eq!(
        generator.generate(1, &incomplete).unwrap_err(),
        GenError::MissingParam(CapTrimGenerator::REGION_SPAN.to_string())
    );
}

#[test]
fn generated_items_are_provisional_and_carry_provenance() {
    let generator = CapTrimGenerator::new(prototype());
    // Turn inside `baseYaw`'s ±60°: a sector the arm cannot sweep no longer
    // generates at all.
    let vector = params(2.0, 1.0, 0.6, 0.1);
    let item = generator
        .generate_item(7, &vector, &DifficultyModel::expert_prior())
        .unwrap();

    // Nobody has attempted it, so it cannot claim to be calibrated.
    assert_eq!(item.dto.meta.calibration, CalibrationState::Provisional);
    assert_eq!(item.dto.meta.response_count, 0);

    let provenance = item.dto.meta.generator.as_ref().unwrap();
    assert_eq!(provenance.family_id, "cap-trim");
    assert_eq!(provenance.seed, 7);
    assert_eq!(provenance.params, vector);

    // Reproducing from the recorded provenance yields the same challenge.
    let replayed = generator.generate(provenance.seed, &provenance.params).unwrap();
    assert_eq!(replayed.target_hair.voxels, item.dto.challenge.target_hair.voxels);
}

#[test]
fn difficulty_is_always_inside_aronas_valid_range() {
    let generator = CapTrimGenerator::new(prototype());
    let model = DifficultyModel::expert_prior();

    for seed in 0..40u64 {
        let vector = generator.family().sample(seed);
        if let Ok(item) = generator.generate_item(seed, &vector, &model) {
            let b = item.dto.meta.irt.difficulty;
            assert!(
                (DIFFICULTY_MIN..=DIFFICULTY_MAX).contains(&b),
                "seed {seed} produced difficulty {b}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Difficulty targeting
// ---------------------------------------------------------------------------

#[test]
fn solving_for_a_target_difficulty_lands_near_it() {
    let generator = CapTrimGenerator::new(prototype());
    let model = DifficultyModel::expert_prior();

    for target in [-1.0, 0.0, 1.0] {
        let item = generator
            .solve_for_difficulty(target, &model, 99, 48)
            .unwrap_or_else(|| panic!("no item found near {target}"));

        let error = (item.predicted_difficulty - target).abs();
        assert!(
            error < 1.5,
            "target {target} produced {} (error {error})",
            item.predicted_difficulty
        );
    }
}

#[test]
fn solving_is_reproducible_for_a_given_seed() {
    let generator = CapTrimGenerator::new(prototype());
    let model = DifficultyModel::expert_prior();

    let first = generator.solve_for_difficulty(0.5, &model, 1234, 16).unwrap();
    let second = generator.solve_for_difficulty(0.5, &model, 1234, 16).unwrap();

    assert_eq!(first.dto.challenge.id, second.dto.challenge.id);
    assert_eq!(first.predicted_difficulty, second.predicted_difficulty);
}

#[test]
fn asking_for_a_harder_item_yields_a_harder_one() {
    let generator = CapTrimGenerator::new(prototype());
    let model = DifficultyModel::expert_prior();

    let easy = generator.solve_for_difficulty(-2.0, &model, 5, 48).unwrap();
    let hard = generator.solve_for_difficulty(2.0, &model, 5, 48).unwrap();

    assert!(
        easy.predicted_difficulty < hard.predicted_difficulty,
        "easy={} hard={}",
        easy.predicted_difficulty,
        hard.predicted_difficulty
    );
}

// ---------------------------------------------------------------------------
// Features
// ---------------------------------------------------------------------------

#[test]
fn a_symmetric_target_scores_lower_asymmetry_than_a_one_sided_one() {
    let generator = CapTrimGenerator::new(prototype());

    // A full-turn sector trims all round: symmetric about the sagittal plane.
    let symmetric = generator.generate(1, &params(2.0, 1.0, 1.0, 0.0)).unwrap();
    // A sector off to one side is not. The turn stays inside the arm's reach,
    // and the span stays wide enough to clear the minimum removal margin —
    // narrower than this and the item is refused rather than generated.
    let lopsided = generator.generate(1, &params(2.0, 1.0, 0.55, 0.15)).unwrap();

    let symmetric_features = ChallengeFeatures::extract(&symmetric);
    let lopsided_features = ChallengeFeatures::extract(&lopsided);

    assert!(
        symmetric_features.asymmetry < lopsided_features.asymmetry,
        "symmetric={} lopsided={}",
        symmetric_features.asymmetry,
        lopsided_features.asymmetry
    );
}

#[test]
fn removing_more_hair_raises_the_volume_feature() {
    let generator = CapTrimGenerator::new(prototype());
    let small = ChallengeFeatures::extract(&generator.generate(1, &params(2.0, 1.0, 0.3, 0.0)).unwrap());
    let large = ChallengeFeatures::extract(&generator.generate(1, &params(3.0, 3.0, 1.0, 0.0)).unwrap());

    assert!(large.removal_volume > small.removal_volume);
}

#[test]
fn an_empty_challenge_produces_zeroed_features() {
    // Guards against NaN leaking out of the ratio features on empty sets.
    let features = ChallengeFeatures::extract(&prototype());
    assert_eq!(features.boundary_ratio, 0.0);
    assert_eq!(features.asymmetry, 0.0);
    assert_eq!(features.reach_strain, 0.0);
    assert_eq!(features.head_proximity, 0.0);
    assert!(features.removal_volume.is_finite());
}

// ---------------------------------------------------------------------------
// Bank integration — the point of the exercise
// ---------------------------------------------------------------------------

fn hints_at(theta: f64) -> SelectionHints {
    let mut hints = SelectionHints::new(Ability(theta));
    hints.params.set(MaxInfoParams {
        tolerance: 1.0,
        min_discrimination: 0.0,
    });
    hints
}

#[test]
fn an_empty_bank_generates_rather_than_failing() {
    let generator = Arc::new(CapTrimGenerator::new(prototype()));
    let mut bank = HcrDynamicBank::new(CatalogSnapshot::new(vec![]), OutcomeStore::new(), 3)
        .allow_uncalibrated(true)
        .with_generator(generator)
        .with_generation_attempts(24);

    // A fixed pool would have nothing to offer here.
    let question = bank.select_question(&hints_at(0.0)).unwrap();

    assert!(question.content.render(arona::RenderFormat::PlainText).starts_with("cap-trim-"));
    assert_eq!(bank.generated_count(), 1);
    assert_eq!(bank.served().len(), 1);
}

#[test]
fn generation_keeps_producing_distinct_items() {
    let generator = Arc::new(CapTrimGenerator::new(prototype()));
    let mut bank = HcrDynamicBank::new(CatalogSnapshot::new(vec![]), OutcomeStore::new(), 3)
        .allow_uncalibrated(true)
        .with_generator(generator);

    for _ in 0..4 {
        bank.select_question(&hints_at(0.0)).unwrap();
    }

    let ids: BTreeSet<_> = bank.served().iter().map(|s| s.id.clone()).collect();
    assert_eq!(ids.len(), 4, "every generated item must be distinct");
    assert_eq!(bank.generated_count(), 4);
}

#[test]
fn measurement_sessions_never_serve_a_generated_item() {
    // Generated items are Provisional, so an ability estimate must not rest on
    // one — the bank must fail rather than quietly generate.
    let generator = Arc::new(CapTrimGenerator::new(prototype()));
    let mut bank = HcrDynamicBank::new(CatalogSnapshot::new(vec![]), OutcomeStore::new(), 3)
        .with_generator(generator);

    assert!(matches!(
        bank.select_question(&hints_at(0.0)),
        Err(QBankError::NoQuestionAvailable)
    ));
    assert_eq!(bank.generated_count(), 0);
}

#[test]
fn generated_items_can_be_taken_for_persistence() {
    let generator = Arc::new(CapTrimGenerator::new(prototype()));
    let mut bank = HcrDynamicBank::new(CatalogSnapshot::new(vec![]), OutcomeStore::new(), 3)
        .allow_uncalibrated(true)
        .with_generator(generator);

    bank.select_question(&hints_at(0.0)).unwrap();
    let taken = bank.take_generated();

    assert_eq!(taken.len(), 1);
    assert!(taken[0].dto.meta.generator.is_some());
    assert_eq!(bank.generated_count(), 0, "taking should drain the buffer");
}

#[test]
fn generation_targets_the_requested_difficulty() {
    let generator = Arc::new(CapTrimGenerator::new(prototype()));
    let mut bank = HcrDynamicBank::new(CatalogSnapshot::new(vec![]), OutcomeStore::new(), 3)
        .allow_uncalibrated(true)
        .with_generator(generator)
        .with_generation_attempts(48);

    // The selector maps theta to a target difficulty; the generated item should
    // follow it rather than being drawn at random.
    let mut low_hints = hints_at(-2.0);
    low_hints.target_difficulty = Some(arona::Difficulty(-2.0));
    bank.select_question(&low_hints).unwrap();

    let mut high_hints = hints_at(2.0);
    high_hints.target_difficulty = Some(arona::Difficulty(2.0));
    bank.select_question(&high_hints).unwrap();

    let generated = bank.take_generated();
    assert_eq!(generated.len(), 2);
    assert!(
        generated[0].predicted_difficulty < generated[1].predicted_difficulty,
        "first={} second={}",
        generated[0].predicted_difficulty,
        generated[1].predicted_difficulty
    );
}

// ---------------------------------------------------------------------------
// Derived starter workspaces
// ---------------------------------------------------------------------------

/// Flatten the emitted Blockly workspace back into `(jointId, angle)` pairs.
fn starter_blocks(workspace: &serde_json::Value) -> Vec<(String, f64)> {
    let mut chain = Vec::new();
    let mut node = workspace["blocks"]["blocks"][0].clone();
    loop {
        assert_eq!(node["type"], "hcr_set_joint_angle");
        chain.push((
            node["fields"]["JOINT_ID"].as_str().expect("joint id").to_string(),
            node["fields"]["ANGLE"].as_f64().expect("angle"),
        ));
        let next = node["next"]["block"].clone();
        if next.is_null() {
            return chain;
        }
        node = next;
    }
}

fn generated(turn: f64) -> ChallengeDefinition {
    CapTrimGenerator::new(prototype())
        .generate(7, &params(2.0, 1.0, 0.5, turn))
        .expect("generates")
}

/// Every item this family emits can be finished perfectly.
///
/// The invariant the family exists to hold, and the one it used to violate. The
/// shipped bank asked for hair the arm cannot reach: `Cap Trim 30%` wanted 91
/// voxels with 20 reachable, `Cap Trim 72%` wanted 284 with 145, `Cap Trim 94%`
/// wanted 345 with 232 — best achievable completion 77.95, 79.53 and 15.67. No
/// learner could score 100 on any of them.
///
/// Deriving the target from a replayed reference makes that structurally
/// impossible: the target *is* what the reference leaves, so the reference
/// scores 100 by definition. This checks the definition holds end to end.
#[test]
fn every_generated_item_can_be_finished_perfectly() {
    let generator = CapTrimGenerator::new(prototype());
    let mut checked = 0;

    for seed in 0..60_u64 {
        let vector = generator.family().sample(seed);
        let Ok(challenge) = generator.generate(seed, &vector) else {
            continue;
        };

        let reference = derive_reference(
            &challenge,
            vector[CapTrimGenerator::REGION_TURN],
            vector[CapTrimGenerator::REGION_SPAN],
            CapTrimGenerator::passes(&vector).expect("complete vector"),
        )
        .unwrap_or_else(|| panic!("seed {seed}: item shipped without a reference"));

        let outcome = hcr_sim::replay(
            &challenge,
            &reference.program(),
            hcr_sim::ReplayOptions::default(),
        )
        .unwrap_or_else(|error| panic!("seed {seed}: reference invalid: {error:?}"));

        assert_eq!(
            outcome.terminal.reason,
            TerminalReason::Completed,
            "seed {seed}: reference halted"
        );
        assert!(
            (outcome.score.completion_score - 100.0).abs() < 1e-6,
            "seed {seed}: reference scored {}, not 100",
            outcome.score.completion_score
        );
        checked += 1;
    }

    assert!(checked >= 40, "only {checked}/60 seeds generated");
}

/// An item has to be able to tell two learners apart.
///
/// Completion is a ratio over the asked set, so the size of that set *is* the
/// item's resolution. Both failure modes have shipped: drawn targets asked for
/// hair the arm could not reach, capping everyone at 77.95; and the first
/// derived bank served an item asking 2 voxels of 342, which can only score 0,
/// 50 or 100 however well anyone plays.
#[test]
fn every_item_has_enough_resolution_to_measure_with() {
    let generator = CapTrimGenerator::new(prototype());
    let mut checked = 0;

    for seed in 0..60_u64 {
        let vector = generator.family().sample(seed);
        let Ok(challenge) = generator.generate(seed, &vector) else {
            continue;
        };

        let initial = challenge.initial_hair.voxels.len();
        let asked = initial - challenge.target_hair.voxels.len();

        // Completion is a ratio over the asked set, so `asked` is the item's
        // resolution: an item asking for 2 voxels can only score 0, 50 or 100,
        // and one stray voxel moves it half the scale.
        assert!(
            asked >= 5,
            "seed {seed}: asks for only {asked} voxel(s) of {initial} \
             — too coarse to measure anyone with"
        );
        checked += 1;
    }

    assert!(checked >= 40, "only {checked}/60 seeds generated");
}

#[test]
fn the_starter_positions_the_tool_and_withholds_the_cut() {
    // The starter is the reference with its carving removed: it lifts the tool
    // clear of the head and stops. Shipping the sweep too would hand over the
    // solution, since the target is defined as that sweep's result.
    let challenge = generated(0.1);
    let blocks = starter_blocks(
        challenge
            .starter_workspace
            .as_ref()
            .expect("ships a starter"),
    );

    assert!(!blocks.is_empty(), "a starter has to be worth loading");
    assert!(
        blocks.iter().all(|(joint, _)| joint != "baseYaw"),
        "the sweep is the learner's to supply: {blocks:?}"
    );

    // And what it does ship must leave hair standing, or it would be the answer.
    let program = Program {
        nodes: blocks
            .iter()
            .enumerate()
            .map(|(index, (joint_id, angle_deg))| ProgramNode::SetJointAngle {
                joint_id: joint_id.clone(),
                angle_deg: *angle_deg,
                source_block_id: format!("b{index}"),
            })
            .collect(),
        source_block_count: blocks.len() as u32,
    };
    let outcome =
        hcr_sim::replay(&challenge, &program, hcr_sim::ReplayOptions::default()).expect("valid");
    assert!(outcome.score.completion_score < 100.0);
}

#[test]
fn a_derived_starter_never_halts_on_the_head() {
    // The property that makes deriving safe at all: whatever is emitted has been
    // replayed through the executor that will score it. A starter that stopped
    // on the head the first time a learner pressed Run would be a worse
    // introduction than the blank canvas it replaced.
    let generator = CapTrimGenerator::new(prototype());
    let mut with_starter = 0;

    for seed in 0..60_u64 {
        let challenge = match generator.generate(seed, &generator.family().sample(seed)) {
            Ok(challenge) => challenge,
            Err(_) => continue,
        };
        let Some(workspace) = challenge.starter_workspace.clone() else {
            continue;
        };
        with_starter += 1;

        let program = Program {
            nodes: starter_blocks(&workspace)
                .into_iter()
                .enumerate()
                .map(|(index, (joint_id, angle_deg))| ProgramNode::SetJointAngle {
                    joint_id,
                    angle_deg,
                    source_block_id: format!("b{index}"),
                })
                .collect(),
            source_block_count: 5,
        };

        let outcome = hcr_sim::replay(&challenge, &program, hcr_sim::ReplayOptions::default())
            .unwrap_or_else(|error| panic!("seed {seed}: starter is invalid: {error:?}"));
        assert_eq!(
            outcome.terminal.reason,
            TerminalReason::Completed,
            "seed {seed}: starter halted"
        );
    }

    assert!(with_starter >= 55, "only {with_starter}/60 seeds got a starter");
}
