//! Adaptive question bank for HCR challenges, built on the arona CAT engine.
//!
//! # What arona gives us, and what it doesn't
//!
//! arona supplies the measurement core: IRT models, ability estimation, item
//! selection strategies, termination rules and session orchestration. It does
//! **not** supply anything that makes a bank dynamic. `StaticQBank` is fixed at
//! construction with no add or remove, performs no exposure control, ignores
//! `SelectionHints::used_types` entirely, and reaches for `thread_rng()` so
//! sessions cannot be reproduced.
//!
//! This crate fills those gaps by implementing arona's own `QuestionBank` trait,
//! which is object-safe with only three required methods — a replacement is the
//! intended extension point, not a workaround.
//!
//! | Need | Where it lives |
//! | --- | --- |
//! | θ estimation, Fisher information, termination | arona |
//! | Mutable item pool, exposure control, content blueprint | [`bank`] |
//! | Continuous score → arona's dichotomised `Score` | [`mastery`] |
//! | Async replay → arona's synchronous `score()` | [`content`] |
//! | Item identity across a changing pool | [`bank::ServedItem`] |
//!
//! # Two constraints that shape the design
//!
//! **`Score::new` panics outside `[0,1]`** (`arona/src/core/score.rs:102`), so
//! every value handed to it is clamped rather than trusted.
//!
//! **`QuestionContent::score` is synchronous and infallible**
//! (`arona/src/content/traits.rs:134`), but scoring an HCR response means
//! replaying a program server-side. The flow therefore inverts: replay first,
//! record the outcome in an [`content::OutcomeStore`], *then* call
//! `submit_response`. See [`content::OutcomeStore::misses`] for how a violation
//! of that ordering is made visible instead of silently scoring zero.
//!
//! # Example
//!
//! ```
//! use hcr_qbank::{
//!     BankItem, CatalogSnapshot, HcrDynamicBank, OutcomeStore, SessionConfig, build_session,
//! };
//! use hcr_contract::{CalibrationState, ChallengeMeta, ItemParameters, SkillDimension};
//!
//! let meta = |b: f64| ChallengeMeta {
//!     version: 1,
//!     irt: ItemParameters { discrimination: 1.2, difficulty: b, guessing: 0.0 },
//!     calibration: CalibrationState::Calibrated,
//!     response_count: 250,
//!     dimensions: vec![SkillDimension::Kinematics],
//!     mastery_threshold: 0.5,
//!     generator: None,
//!     hardware_compatible: true,
//! };
//!
//! let catalog = CatalogSnapshot::new(vec![
//!     BankItem::new("easy", meta(-1.0)),
//!     BankItem::new("medium", meta(0.0)),
//!     BankItem::new("hard", meta(1.0)),
//! ]);
//!
//! let outcomes = OutcomeStore::new();
//! let bank = HcrDynamicBank::new(catalog, outcomes.clone(), 42);
//! let mut session = build_session(bank, SessionConfig::default(), 0.0);
//!
//! // Serve an item, replay the learner's program, then submit.
//! let _question = session.next_question().unwrap();
//! outcomes.record("submission-1", 0.86); // normalized final score
//! let result = session.submit_response("submission-1").unwrap();
//! assert!(result.correct);
//! ```

#![forbid(unsafe_code)]

pub mod bank;
pub mod blueprint;
pub mod calibration;
pub mod content;
pub mod difficulty;
pub mod exposure;
pub mod features;
pub mod generator;
pub mod item;
pub mod mastery;
pub mod session;
pub mod starter;

pub use bank::{FIELD_BOOST, HcrDynamicBank, ServedItem, SharedServedLog};
pub use blueprint::Blueprint;
pub use calibration::{
    CalibrationPipeline, CalibrationSettings, DriftReport, FitResult, ItemFit, ItemObservations,
    LinkingItem, Mode, ModeOffsetFit, Observation, PipelineReport, PromotionPolicy,
    detect_drift, estimate_mode_offset, outfit, refit_difficulty, refit_item,
};
pub use content::{ChallengeContent, OutcomeStore};
pub use difficulty::{DIFFICULTY_MAX, DIFFICULTY_MIN, DifficultyModel};
pub use exposure::ExposureController;
pub use features::ChallengeFeatures;
pub use generator::{
    CapTrimGenerator, ChallengeGenerator, GenError, GeneratedItem, ItemFamily, ParamSpec,
    ParamVector,
};
pub use item::{BankItem, CatalogSnapshot};
pub use mastery::{is_mastered, raw_from_remapped, remap_for_arona};
pub use session::{SessionConfig, build_session};
pub use starter::{ReferenceRun, derive_reference};
