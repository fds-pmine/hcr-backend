//! Item metadata: IRT parameters, calibration state and skill dimensions.
//!
//! Mirrors the `ChallengeMeta` block of `docs/backend/schema/hcr-v1.d.ts`. These
//! ride along with a challenge on the wire and are additive — a client that
//! ignores `meta` still receives a valid v1 challenge.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use serde::{Deserialize, Serialize};

use crate::domain::ChallengeDefinition;

/// Stable identifier of a bank item.
pub type ItemId = String;

/// IRT item parameters.
///
/// Separate from arona's own `IRTParameters`, which has no serde derives
/// (`arona/src/core/irt.rs:82-87`). The backend owns this DTO and converts at the
/// boundary.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemParameters {
    /// Discrimination `a` — how sharply the item separates ability levels.
    pub discrimination: f64,
    /// Difficulty `b`, on the logit scale.
    ///
    /// Always the **solo-mode** value. Feeding a match-contaminated difficulty
    /// into the adaptive bank would corrupt every ability estimate downstream
    /// (`docs/backend/07-CALIBRATION.md` §10).
    pub difficulty: f64,
    /// Guessing `c`. Effectively 0 for HCR tasks — you cannot guess your way to
    /// a correct haircut program — which makes the model 2PL in practice.
    pub guessing: f64,
}

impl ItemParameters {
    /// A freshly generated item: predicted difficulty, neutral discrimination.
    pub fn provisional(difficulty: f64) -> Self {
        Self {
            discrimination: 1.0,
            difficulty,
            guessing: 0.0,
        }
    }
}

/// How much response data stands behind an item's parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CalibrationState {
    /// Difficulty predicted by the feature model; no responses yet.
    ///
    /// Usable in competitive rounds — ranking is valid whatever `b` turns out to
    /// be, since every player faces the identical item — but not for measurement.
    Provisional,
    /// Enough responses to refit `b` incrementally.
    Online,
    /// Batch-calibrated and stable.
    Calibrated,
    /// Withdrawn: drifted, superseded, or pathological.
    Retired,
}

impl CalibrationState {
    /// Whether this item may be served by the adaptive (measurement) bank.
    pub fn usable_for_measurement(self) -> bool {
        matches!(self, CalibrationState::Online | CalibrationState::Calibrated)
    }

    /// Whether this item may be served at all.
    pub fn servable(self) -> bool {
        !matches!(self, CalibrationState::Retired)
    }
}

/// Facets of robot-programming proficiency an item exercises.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SkillDimension {
    /// Reasoning about joint angles and reach.
    Kinematics,
    /// Ordering commands correctly.
    Sequencing,
    /// Using `repeat` to stay within budget.
    Iteration,
    /// Hitting a tight target shape.
    Precision,
    /// Keeping clear of the head.
    Safety,
}

impl SkillDimension {
    /// Every dimension, in a stable order.
    pub const ALL: [SkillDimension; 5] = [
        SkillDimension::Kinematics,
        SkillDimension::Sequencing,
        SkillDimension::Iteration,
        SkillDimension::Precision,
        SkillDimension::Safety,
    ];

    /// Stable key, used for `SelectionHints::used_types` and for tags.
    pub fn as_str(self) -> &'static str {
        match self {
            SkillDimension::Kinematics => "kinematics",
            SkillDimension::Sequencing => "sequencing",
            SkillDimension::Iteration => "iteration",
            SkillDimension::Precision => "precision",
            SkillDimension::Safety => "safety",
        }
    }
}

/// Where a generated item came from.
///
/// An item is identified by `(family_id, version, seed, params)`, and generation
/// is deterministic, so the challenge served today can be reproduced exactly
/// during an audit years later.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GeneratorProvenance {
    /// Item family template.
    pub family_id: String,
    /// Generator version; a bump re-enters calibration.
    pub version: String,
    /// Seed handed to the generator.
    pub seed: u64,
    /// Parameter vector.
    pub params: BTreeMap<String, f64>,
}

/// Psychometric metadata attached to a challenge.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeMeta {
    /// Immutable once served. Recalibration mints a new version so historical
    /// scores never move.
    pub version: u32,
    /// Solo-scale item parameters.
    pub irt: ItemParameters,
    /// Calibration lifecycle stage.
    pub calibration: CalibrationState,
    /// Responses observed for this item version.
    pub response_count: u32,
    /// Skill facets this item exercises.
    pub dimensions: Vec<SkillDimension>,
    /// Normalized final score above which the learner is judged to have mastered
    /// the item. Default 0.5.
    pub mastery_threshold: f64,
    /// Provenance when generated from an item family.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generator: Option<GeneratorProvenance>,
    /// Whether the physical arm can actually run this challenge.
    pub hardware_compatible: bool,
}

impl ChallengeMeta {
    /// Metadata for a brand-new generated item.
    pub fn provisional(version: u32, predicted_difficulty: f64) -> Self {
        Self {
            version,
            irt: ItemParameters::provisional(predicted_difficulty),
            calibration: CalibrationState::Provisional,
            response_count: 0,
            dimensions: Vec::new(),
            mastery_threshold: 0.5,
            generator: None,
            hardware_compatible: true,
        }
    }
}

/// A challenge plus its psychometric metadata, as served by the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChallengeDefinitionDto {
    /// The v1 challenge, unchanged.
    #[serde(flatten)]
    pub challenge: ChallengeDefinition,
    /// Additive metadata.
    pub meta: ChallengeMeta,
}
