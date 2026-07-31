//! A runnable HCR server, seeded with the real shipped challenge.
//!
//! ```sh
//! cargo run -p hcr_service --features hotaru --example serve
//! curl -s localhost:8080/api/v1/challenges | jq
//! ```
//!
//! The whole binding is the loop below: every route funnels into the same
//! [`Router`], which is tested without a socket in `tests/binding.rs`. hotaru
//! carries bytes; it makes no routing decisions of its own.

use std::sync::Arc;

use hcr_contract::{
    CalibrationState, ChallengeDefinition, ChallengeDefinitionDto, ChallengeMeta, ItemParameters,
    SkillDimension,
};
use hcr_qbank::{CapTrimGenerator, ChallengeGenerator, DifficultyModel};
use hcr_service::hotaru_binding::{ROUTES, make_handler};
use hcr_service::{
    CatalogStore, HcrService, ItemRefSigner, ReplayPool, Router, ServiceConfig,
};
use hotaru::http::*;
use hotaru::prelude::*;

const BINDING: &str = "127.0.0.1:8080";

/// Origin of the Vite dev server, so a browser on a different port can talk to
/// this one. Production uses MQTT-over-WebSocket and needs no CORS at all.
const DEV_ORIGIN: &str = "http://localhost:5173";

/// The shipped challenge, lifted from the conformance fixture so the demo runs
/// against the *real* hairstyle — 241 initial voxels, 215 target — rather than a
/// toy. The fixture is generated from the TypeScript engine, so this is byte for
/// byte the challenge the frontend ships.
const VECTORS: &str = include_str!("../../hcr_sim/tests/fixtures/vectors.json");

fn seed_catalog() -> Arc<CatalogStore> {
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
    // (`docs/07-CALIBRATION.md` §8). Without this the demo session would serve
    // one item and stop with `BANK_EXHAUSTED`, which is correct but dull.
    let generator = CapTrimGenerator::new(shipped);
    let model = DifficultyModel::expert_prior();
    for (index, target) in [-1.0_f64, 0.0, 1.0].into_iter().enumerate() {
        if let Some(mut item) =
            generator.solve_for_difficulty(target, &model, 40 + index as u64, 48)
        {
            item.dto.meta.calibration = CalibrationState::Online;
            item.dto.meta.response_count = 50;
            catalog.insert(item.dto).expect("seed generated challenge");
        }
    }

    catalog
}

fn main() {
    let catalog = seed_catalog();
    println!("catalog seeded with {} challenge versions", catalog.len());

    let service = Arc::new(HcrService::new(
        catalog,
        Arc::new(ReplayPool::with_default_concurrency()),
        // A real deployment loads this from configuration. With a known key,
        // every item reference in the system is forgeable.
        ItemRefSigner::new(*b"replace-me-with-a-real-secret"),
        ServiceConfig::default(),
    ));
    let router = Arc::new(Router::new(service));

    let app = <Server>::new()
        .binding(BINDING)
        .single_protocol(ProtocolBuilder::new(HTTP::server(HttpSafety::default())))
        .build();

    for (pattern, name) in ROUTES {
        app.insert(Endpoint::<HTTP>::endpoint(
            *pattern,
            *name,
            make_handler(router.clone(), Some(DEV_ORIGIN.to_string())),
        ))
        .expect("insert endpoint");
    }

    println!("HCR service listening on http://{BINDING}");
    println!("CORS allowed for {DEV_ORIGIN}");

    // What `run_server!(APP)` expands to; called directly so the example needs
    // no proc-macro ceremony.
    hotaru::hotaru_core::app::server::run_server(app.clone());
}
