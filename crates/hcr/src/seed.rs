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

/// Build a catalog: one authored challenge plus three generated items.
pub fn seed_catalog() -> Arc<CatalogStore> {
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

    // Three generated items spanning the difficulty scale, so the catalog shows
    // the dynamic bank rather than a single hand-authored challenge.
    //
    // They are promoted to `Online` here to stand in for items that have already
    // accumulated responses. Straight out of the generator they are
    // `Provisional`, and an adaptive session refuses those on purpose — an
    // ability estimate must not rest on an uncalibrated item
    // (`docs/07-CALIBRATION.md` §8). Without this a demo session would serve one
    // item and stop with `BANK_EXHAUSTED`, which is correct but dull.
    let generator = CapTrimGenerator::new(shipped);
    let model = DifficultyModel::expert_prior();
    for (index, target) in [-1.0_f64, 0.0, 1.0].into_iter().enumerate() {
        if let Some(mut item) = generator.solve_for_difficulty(target, &model, 40 + index as u64, 48)
        {
            item.dto.meta.calibration = CalibrationState::Online;
            item.dto.meta.response_count = 50;
            catalog.insert(item.dto).expect("seed generated challenge");
        }
    }

    catalog
}
