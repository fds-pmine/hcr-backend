//! The reference catalog.
//!
//! One hand-authored challenge — the shipped `neat-short-cap`, read from the
//! conformance fixture so it is byte for byte what the frontend ships — plus
//! three generated items spanning the difficulty scale.
//!
//! Lives in the library rather than beside a binary because both the
//! development server and the deployable one seed from it, and a catalog that
//! differed between them would make a bug reproducible in only one.

use std::sync::Arc;

use hcr_contract::{
    CalibrationState, ChallengeDefinition, ChallengeDefinitionDto, ChallengeMeta, ItemParameters,
    ProgrammingMode, SkillDimension,
};
use hcr_qbank::{CapTrimGenerator, ChallengeGenerator, DifficultyModel};
use crate::CatalogStore;

/// The shipped challenge, lifted from the conformance fixture so the server
/// runs against the *real* hairstyle — 241 initial voxels, 229 target — rather
/// than a toy. The fixture is generated from the TypeScript engine, so this is
/// byte for byte the challenge the frontend ships.
// Vendored from `hcr_sim`'s conformance fixture; see the note in
// `cutter_grid_planner.rs` on why a published crate cannot reach a sibling.
const VECTORS: &str = include_str!("../assets/vectors.json");

/// How many generated items a catalog carries unless a deployment asks for more.
///
/// Three demonstrates a dynamic bank. It does not feed a room: a class playing
/// competitive rounds works through the bank and then starts repeating, and
/// repeating a hairstyle turns the next round into a memory test.
pub const DEFAULT_GENERATED_ITEMS: usize = 3;

/// The first three targets are the demonstration spread and are promoted to
/// `Online`; anything past them is generated across this range and left
/// `Provisional`, which is exactly what a competitive round wants.
const DIFFICULTY_RANGE: (f64, f64) = (-1.5, 1.5);

/// Build a catalog: one authored challenge plus three generated items.
pub fn seed_catalog() -> Arc<CatalogStore> {
    seed_catalog_with(DEFAULT_GENERATED_ITEMS)
}

/// Build a catalog with `generated` synthesised items beside the authored one.
///
/// Generating more is legitimate for rounds and refused for measurement, and
/// the difference is not a workaround — it is the calibration rule. A round
/// ranks players against each other on an identical item, so the ranking holds
/// whatever the item's difficulty parameter turns out to be
/// (`CatalogStore::pick_for_match`). An adaptive session estimates an ability,
/// which an uncalibrated item cannot support (`docs/07-CALIBRATION.md` §8), and
/// `Bank::generate_at` refuses to synthesise into a measurement session for
/// that reason. So items past the demonstration three stay `Provisional`: they
/// are served to rounds, withheld from measurement, and calibrate themselves
/// once real responses arrive.
pub fn seed_catalog_with(generated: usize) -> Arc<CatalogStore> {
    let catalog = Arc::new(CatalogStore::new());

    let vectors: serde_json::Value =
        serde_json::from_str(VECTORS).expect("conformance fixture parses");
    let shipped: ChallengeDefinition =
        serde_json::from_value(vectors["challenge"].clone()).expect("challenge parses");

    catalog
        .insert(ChallengeDefinitionDto {
            challenge: shipped.clone(),
            meta: ChallengeMeta {
                version: 1,
                irt: ItemParameters {
                    discrimination: 1.2,
                    difficulty: 0.0,
                    guessing: 0.0,
                },
                calibration: CalibrationState::Calibrated,
                response_count: 250,
                dimensions: vec![SkillDimension::Kinematics, SkillDimension::Precision],
                mastery_threshold: 0.5,
                generator: None,
                hardware_compatible: false,
                // The one challenge with a certified Cutter Grid profile: the
                // frontend bundles `cutter-grid-profile-v2.json` for exactly
                // this hairstyle and geometry, and the signature check ties a
                // trajectory to it. Every other item in this catalog is
                // generated, and the generator produces no profile.
                programming_modes: vec![ProgrammingMode::Servo, ProgrammingMode::CutterGrid],
            },
        })
        .expect("seed shipped challenge");

    // The demonstration three are promoted to `Online` to stand in for items
    // that have already accumulated responses, because an adaptive session
    // refuses `Provisional` items on purpose and a demo session that served one
    // item and stopped with `BANK_EXHAUSTED` would be correct but dull.
    let generator = CapTrimGenerator::new(shipped);
    let model = DifficultyModel::expert_prior();
    for (index, target) in difficulty_targets(generated).into_iter().enumerate() {
        let Some(mut item) = generator.solve_for_difficulty(target, &model, 40 + index as u64, 48)
        else {
            // The family cannot always hit a requested difficulty. A short bank
            // is a worse catalog, not a broken one.
            continue;
        };
        if index < DEFAULT_GENERATED_ITEMS {
            item.dto.meta.calibration = CalibrationState::Online;
            item.dto.meta.response_count = 50;
        }
        // A duplicate id means the search landed on an identical item; keeping
        // the first is right and the insert failing is not an error.
        let _ = catalog.insert(item.dto);
    }

    catalog
}

/// Difficulty targets for `count` items: the demonstration spread first, then
/// an even sweep of the range so a bigger bank is a wider one.
fn difficulty_targets(count: usize) -> Vec<f64> {
    let mut targets = vec![-1.0_f64, 0.0, 1.0];
    targets.truncate(count.min(DEFAULT_GENERATED_ITEMS));

    let extra = count.saturating_sub(DEFAULT_GENERATED_ITEMS);
    let (low, high) = DIFFICULTY_RANGE;
    for step in 0..extra {
        // Offset by half a step so the sweep does not simply land on the three
        // targets already taken.
        let position = (step as f64 + 0.5) / extra as f64;
        targets.push(low + (high - low) * position);
    }
    targets
}
