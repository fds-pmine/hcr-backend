//! Automatic item generation.
//!
//! An HCR challenge is parametric — geometry, lattice, hairstyles, budget — so
//! items can be synthesised rather than authored. That matters for two reasons:
//! adaptive testing runs out of unseen items at the tails of the ability scale,
//! and a competitive round needs an item **no participant has seen**, which a
//! fixed pool cannot guarantee once a player base is active.
//!
//! Generation is deterministic. An item is identified by
//! `(family_id, version, seed, params)`, and the same tuple must reproduce the
//! same challenge byte-for-byte years later, or an audit is impossible.

use std::collections::BTreeMap;
use std::f64::consts::{PI, TAU};

use hcr_contract::{
    CalibrationState, ChallengeDefinition, ChallengeDefinitionDto, ChallengeMeta,
    GeneratorProvenance, HairstyleDefinition, ItemParameters, SkillDimension, VoxelCoord,
};
use rand::prelude::*;
use rand::rngs::StdRng;

use crate::difficulty::DifficultyModel;
use crate::features::ChallengeFeatures;

/// A tunable dimension of an item family.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamSpec {
    /// Parameter name, used as the key in a [`ParamVector`].
    pub name: String,
    /// Inclusive lower bound.
    pub min: f64,
    /// Inclusive upper bound.
    pub max: f64,
}

impl ParamSpec {
    /// Define a parameter.
    pub fn new(name: impl Into<String>, min: f64, max: f64) -> Self {
        Self {
            name: name.into(),
            min,
            max,
        }
    }
}

/// A concrete assignment of family parameters.
pub type ParamVector = BTreeMap<String, f64>;

/// A template from which items are generated.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemFamily {
    /// Stable family identifier.
    pub id: String,
    /// Generator version. A bump invalidates calibration for every instance,
    /// because the same params no longer mean the same challenge.
    pub version: String,
    /// Skill facets instances of this family exercise.
    pub dimensions: Vec<SkillDimension>,
    /// Tunable parameters.
    pub params: Vec<ParamSpec>,
    /// Whether instances can run on the physical arm.
    pub hardware_compatible: bool,
}

impl ItemFamily {
    /// Deterministically sample a parameter vector.
    pub fn sample(&self, seed: u64) -> ParamVector {
        let mut rng = StdRng::seed_from_u64(seed);
        self.params
            .iter()
            .map(|spec| {
                let value = if spec.max > spec.min {
                    rng.gen_range(spec.min..=spec.max)
                } else {
                    spec.min
                };
                (spec.name.clone(), value)
            })
            .collect()
    }
}

/// Why generation failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GenError {
    /// A required parameter was absent from the vector.
    MissingParam(String),
    /// The parameters produced a challenge with no hair at all.
    EmptyHairstyle,
    /// The parameters produced a challenge where nothing needs removing, so
    /// there is no task.
    NothingToRemove,
    /// The prototype challenge is unusable as a template.
    BadPrototype(&'static str),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::MissingParam(name) => write!(f, "missing parameter `{name}`"),
            GenError::EmptyHairstyle => write!(f, "generated hairstyle is empty"),
            GenError::NothingToRemove => write!(f, "generated target equals the initial hairstyle"),
            GenError::BadPrototype(reason) => write!(f, "unusable prototype: {reason}"),
        }
    }
}

impl std::error::Error for GenError {}

/// A generated item with its predicted placement on the difficulty scale.
#[derive(Debug, Clone)]
pub struct GeneratedItem {
    /// The challenge plus metadata, ready to serve and to persist.
    pub dto: ChallengeDefinitionDto,
    /// Difficulty the model predicted.
    pub predicted_difficulty: f64,
    /// Features that produced it, retained so the model can be refit later.
    pub features: ChallengeFeatures,
}

/// Synthesises challenges from a family.
pub trait ChallengeGenerator: Send + Sync + std::fmt::Debug {
    /// The family this generator implements.
    fn family(&self) -> &ItemFamily;

    /// Build a challenge. Must be a pure function of `(seed, params)`.
    fn generate(&self, seed: u64, params: &ParamVector) -> Result<ChallengeDefinition, GenError>;

    /// Generate an item and place it on the difficulty scale.
    fn generate_item(
        &self,
        seed: u64,
        params: &ParamVector,
        model: &DifficultyModel,
    ) -> Result<GeneratedItem, GenError> {
        let challenge = self.generate(seed, params)?;
        let features = ChallengeFeatures::extract(&challenge);
        let predicted = model.predict_features(&features);
        let family = self.family();

        Ok(GeneratedItem {
            dto: ChallengeDefinitionDto {
                challenge,
                meta: ChallengeMeta {
                    version: 1,
                    irt: ItemParameters {
                        discrimination: 1.0,
                        difficulty: predicted,
                        guessing: 0.0,
                    },
                    // Never `Calibrated`: no one has attempted this yet.
                    calibration: CalibrationState::Provisional,
                    response_count: 0,
                    dimensions: family.dimensions.clone(),
                    mastery_threshold: 0.5,
                    generator: Some(GeneratorProvenance {
                        family_id: family.id.clone(),
                        version: family.version.clone(),
                        seed,
                        params: params.clone(),
                    }),
                    hardware_compatible: family.hardware_compatible,
                },
            },
            predicted_difficulty: predicted,
            features,
        })
    }

    /// Search the family for an item near `target_difficulty`.
    ///
    /// Samples `attempts` parameter vectors and keeps the closest. Deterministic
    /// in `seed`, so the same request yields the same item.
    fn solve_for_difficulty(
        &self,
        target_difficulty: f64,
        model: &DifficultyModel,
        seed: u64,
        attempts: u32,
    ) -> Option<GeneratedItem> {
        let family = self.family();
        let mut best: Option<GeneratedItem> = None;

        for attempt in 0..attempts.max(1) {
            // Derive a distinct but reproducible seed per attempt.
            let candidate_seed = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15).wrapping_add(attempt.into());
            let params = family.sample(candidate_seed);

            let Ok(item) = self.generate_item(candidate_seed, &params, model) else {
                // Degenerate parameter draws are expected; skip and keep looking.
                continue;
            };

            let closer = best.as_ref().is_none_or(|current| {
                (item.predicted_difficulty - target_difficulty).abs()
                    < (current.predicted_difficulty - target_difficulty).abs()
            });
            if closer {
                best = Some(item);
            }
        }

        best
    }
}

/// Generates "trim a cap of hair" challenges.
///
/// The initial hairstyle is an ellipsoidal shell over the head; the target is
/// that shell with an angular sector thinned out. Varying sector width, depth and
/// orientation moves the item across the difficulty scale in a way the feature
/// model can see: a narrow off-centre trim is asymmetric and fiddly, a broad
/// shallow one is neither.
#[derive(Debug, Clone)]
pub struct CapTrimGenerator {
    family: ItemFamily,
    prototype: ChallengeDefinition,
}

impl CapTrimGenerator {
    /// Parameter names this family uses.
    pub const CAP_THICKNESS: &'static str = "cap_thickness";
    /// Depth of the trim, in voxel layers.
    pub const TRIM_DEPTH: &'static str = "trim_depth";
    /// Angular width of the trimmed sector, as a fraction of a half turn.
    pub const REGION_SPAN: &'static str = "region_span";
    /// Orientation of the trimmed sector, as a fraction of a full turn.
    pub const REGION_TURN: &'static str = "region_turn";

    /// Build a generator around a prototype challenge, which supplies the robot
    /// geometry, voxel lattice and scoring config. Only the hairstyles vary.
    pub fn new(prototype: ChallengeDefinition) -> Self {
        Self {
            family: ItemFamily {
                id: "cap-trim".to_string(),
                version: "1".to_string(),
                dimensions: vec![
                    SkillDimension::Kinematics,
                    SkillDimension::Precision,
                    SkillDimension::Safety,
                ],
                params: vec![
                    ParamSpec::new(Self::CAP_THICKNESS, 1.0, 3.0),
                    ParamSpec::new(Self::TRIM_DEPTH, 1.0, 3.0),
                    ParamSpec::new(Self::REGION_SPAN, 0.25, 1.0),
                    ParamSpec::new(Self::REGION_TURN, 0.0, 1.0),
                ],
                hardware_compatible: true,
            },
            prototype,
        }
    }

    fn param(params: &ParamVector, name: &str) -> Result<f64, GenError> {
        params
            .get(name)
            .copied()
            .ok_or_else(|| GenError::MissingParam(name.to_string()))
    }
}

impl ChallengeGenerator for CapTrimGenerator {
    fn family(&self) -> &ItemFamily {
        &self.family
    }

    fn generate(&self, seed: u64, params: &ParamVector) -> Result<ChallengeDefinition, GenError> {
        let cap_thickness = Self::param(params, Self::CAP_THICKNESS)?;
        let trim_depth = Self::param(params, Self::TRIM_DEPTH)?.min(cap_thickness);
        let region_span = Self::param(params, Self::REGION_SPAN)?;
        let region_turn = Self::param(params, Self::REGION_TURN)?;

        let voxel = self.prototype.voxel_config;
        if voxel.size <= 0.0 || voxel.head_scale.iter().any(|axis| *axis <= 0.0) {
            return Err(GenError::BadPrototype("voxel size and head scale must be positive"));
        }

        let scale_mean = voxel.head_scale.iter().sum::<f64>() / 3.0;
        // Shell thickness expressed in normalised-radius units, so it means the
        // same thing on every axis of the ellipsoid.
        let shell = cap_thickness * voxel.size / scale_mean;
        let cut_from = (1.0 + shell) - trim_depth * voxel.size / scale_mean;

        // Bound the lattice search to the region the shell can occupy.
        let extent = |axis: usize| {
            (((voxel.head_scale[axis] * (1.0 + shell)) / voxel.size).ceil() as i32) + 2
        };
        let centre = |axis: usize| {
            ((voxel.head_center[axis] - voxel.origin[axis]) / voxel.size).round() as i32
        };

        let mut initial = Vec::new();
        let mut target = Vec::new();

        // Hair grows on the crown and sides, not under the jaw.
        let chin = voxel.head_center[1] - 0.35 * voxel.head_scale[1];
        let sector_centre = region_turn * TAU;
        let sector_half_width = region_span * PI;

        for x in -extent(0)..=extent(0) {
            for y in -extent(1)..=extent(1) {
                for z in -extent(2)..=extent(2) {
                    let coord = VoxelCoord {
                        x: centre(0) + x,
                        y: centre(1) + y,
                        z: centre(2) + z,
                    };
                    let world = [
                        voxel.origin[0] + f64::from(coord.x) * voxel.size,
                        voxel.origin[1] + f64::from(coord.y) * voxel.size,
                        voxel.origin[2] + f64::from(coord.z) * voxel.size,
                    ];

                    if world[1] < chin {
                        continue;
                    }

                    let radius = (0..3)
                        .map(|axis| {
                            let d = (world[axis] - voxel.head_center[axis]) / voxel.head_scale[axis];
                            d * d
                        })
                        .sum::<f64>()
                        .sqrt();

                    // Inside the skull, or beyond the hair shell.
                    if radius < 1.0 || radius > 1.0 + shell {
                        continue;
                    }

                    initial.push(coord);

                    // Trim only the outer layers, and only inside the sector.
                    let azimuth =
                        (world[2] - voxel.head_center[2]).atan2(world[0] - voxel.head_center[0]);
                    let mut delta = (azimuth - sector_centre).abs() % TAU;
                    if delta > PI {
                        delta = TAU - delta;
                    }

                    let trimmed = radius >= cut_from && delta <= sector_half_width;
                    if !trimmed {
                        target.push(coord);
                    }
                }
            }
        }

        if initial.is_empty() {
            return Err(GenError::EmptyHairstyle);
        }
        if target.len() == initial.len() {
            return Err(GenError::NothingToRemove);
        }

        let id = format!("{}-{seed:016x}", self.family.id);
        Ok(ChallengeDefinition {
            id: id.clone(),
            name: format!("Cap Trim {:.0}%", region_span * 100.0),
            description: format!(
                "Trim {trim_depth:.0} layer(s) from a {cap_thickness:.0}-layer cap across \
                 {:.0}% of the head.",
                region_span * 100.0
            ),
            robot_config: self.prototype.robot_config.clone(),
            voxel_config: voxel,
            initial_hair: HairstyleDefinition {
                id: format!("{id}-initial"),
                name: "Generated Cap".to_string(),
                voxels: initial,
            },
            target_hair: HairstyleDefinition {
                id: format!("{id}-target"),
                name: "Trimmed Cap".to_string(),
                voxels: target,
            },
            allowed_blocks: self.prototype.allowed_blocks.clone(),
            starter_workspace: None,
            scoring: self.prototype.scoring,
        })
    }
}
