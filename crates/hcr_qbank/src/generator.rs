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
    /// No safe reference solution exists, so the trim the parameters describe is
    /// one the arm cannot perform.
    ///
    /// Rejecting is the point: an item generated anyway would ask for hair
    /// nothing can reach, which is what every instance of this family used to do
    /// (see [`crate::starter`]).
    UnreachableSector,
    /// The reference cuts so little that finishing barely beats doing nothing.
    MarginTooSmall,
    /// The prototype challenge is unusable as a template.
    BadPrototype(&'static str),
}

impl std::fmt::Display for GenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GenError::MissingParam(name) => write!(f, "missing parameter `{name}`"),
            GenError::EmptyHairstyle => write!(f, "generated hairstyle is empty"),
            GenError::NothingToRemove => write!(f, "generated target equals the initial hairstyle"),
            GenError::UnreachableSector => {
                write!(f, "no safe reference solution trims the requested sector")
            }
            GenError::MarginTooSmall => {
                write!(f, "finishing scores too close to doing nothing")
            }
            GenError::BadPrototype(reason) => write!(f, "unusable prototype: {reason}"),
        }
    }
}

impl std::error::Error for GenError {}

/// Smallest share of the hair a reference solution has to remove.
///
/// Sets the gap between finishing and doing nothing: because completion is an
/// IoU over remaining hair, removing a fraction `f` of it makes that gap `100·f`
/// points. At 2% an item is worth at least two points of score, which is the
/// floor at which a learner can see they improved and the response can separate
/// one ability from another. The authored challenge sits at 5%.
const MIN_REMOVAL_FRACTION: f64 = 0.02;

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
/// The initial hairstyle is an ellipsoidal shell over the head. The target is
/// whatever a reference solution leaves standing once it has swept that shell —
/// derived rather than drawn, so the item is finishable by construction; see
/// [`crate::starter`] for what drawing it cost. Varying sector width, depth and
/// orientation changes the sweep the reference performs, and so how much of it a
/// learner has to rebuild.
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
        let (turn_min, turn_max) = Self::reachable_turns(&prototype);
        Self {
            family: ItemFamily {
                id: "cap-trim".to_string(),
                // Bumped from "2": the target is now the result of a replayed
                // reference solution rather than a sector drawn on the
                // ellipsoid, so the same params describe a different — and for
                // the first time finishable — item. Responses to the old
                // instances measured a different task and must not be pooled
                // with these during calibration.
                version: "3".to_string(),
                dimensions: vec![
                    SkillDimension::Kinematics,
                    SkillDimension::Precision,
                    SkillDimension::Safety,
                ],
                params: vec![
                    ParamSpec::new(Self::CAP_THICKNESS, 1.0, 3.0),
                    ParamSpec::new(Self::TRIM_DEPTH, 1.0, 3.0),
                    ParamSpec::new(Self::REGION_SPAN, 0.25, 1.0),
                    ParamSpec::new(Self::REGION_TURN, turn_min, turn_max),
                ],
                hardware_compatible: true,
            },
            prototype,
        }
    }

    /// The sector orientations this arm can actually reach.
    ///
    /// `region_turn` places the sector centre at azimuth `region_turn · 2π`, and
    /// the arm's azimuth is `−baseYaw` ([`crate::starter`]). So the reachable
    /// centres are exactly `−baseYaw_max ..= −baseYaw_min`, expressed as a
    /// fraction of a turn.
    ///
    /// # Why this is a restriction and not a nicety
    ///
    /// The full turn let a seed place the trim sector behind the head, where the
    /// arm cannot go. At the narrow end of `region_span` the whole sector landed
    /// there, and the result was not a hard item — it was an item **no program
    /// can affect**. Every player scores the same on it whatever they do, so its
    /// discrimination is zero: it measures nothing, ranks nobody, and pollutes
    /// calibration with responses that carry no signal.
    ///
    /// Bounding the centre is sufficient rather than merely conservative: the
    /// sector contains its own centre, so a reachable centre guarantees the arm
    /// can reach *some* of what it must trim. A wide sector may still spill past
    /// what the arm covers, which is a legitimate way for an item to be hard —
    /// it caps the achievable score equally for everyone rather than flattening
    /// it.
    ///
    /// Falls back to the full turn when the prototype has no `baseYaw`, since
    /// then there is nothing to reason from.
    fn reachable_turns(prototype: &ChallengeDefinition) -> (f64, f64) {
        prototype
            .robot_config
            .joints
            .iter()
            .find(|joint| joint.id == "baseYaw")
            .map_or((0.0, 1.0), |joint| {
                (
                    -joint.max_angle_deg / 360.0,
                    -joint.min_angle_deg / 360.0,
                )
            })
    }

    fn param(params: &ParamVector, name: &str) -> Result<f64, GenError> {
        params
            .get(name)
            .copied()
            .ok_or_else(|| GenError::MissingParam(name.to_string()))
    }

    /// How many sweeps the reference solution makes for these parameters.
    ///
    /// Public because the reference *defines* the target, so anything checking
    /// an item is finishable has to rebuild the same program — and the trim
    /// cannot go deeper than the cap is thick, which is not something a caller
    /// should have to know to rederive.
    ///
    /// # Errors
    /// [`GenError::MissingParam`] when the vector is incomplete.
    pub fn passes(params: &ParamVector) -> Result<u32, GenError> {
        let cap_thickness = Self::param(params, Self::CAP_THICKNESS)?;
        let trim_depth = Self::param(params, Self::TRIM_DEPTH)?.min(cap_thickness);
        Ok(trim_depth.round() as u32)
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

        // Bound the lattice search to the region the shell can occupy.
        let extent = |axis: usize| {
            (((voxel.head_scale[axis] * (1.0 + shell)) / voxel.size).ceil() as i32) + 2
        };
        let centre = |axis: usize| {
            ((voxel.head_center[axis] - voxel.origin[axis]) / voxel.size).round() as i32
        };

        let mut initial: Vec<VoxelCoord> = Vec::new();

        // Hair grows on the crown and sides, not under the jaw.
        let chin = voxel.head_center[1] - 0.35 * voxel.head_scale[1];

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
                }
            }
        }

        if initial.is_empty() {
            return Err(GenError::EmptyHairstyle);
        }
        // A trim of zero layers, or across zero width, describes no cut at all.
        // Caught here rather than after simulation so the error names the cause.
        let passes = Self::passes(params)?;
        if passes == 0 || region_span <= 0.0 {
            return Err(GenError::NothingToRemove);
        }

        let id = format!("{}-{seed:016x}", self.family.id);
        let initial_len = initial.len();
        let mut challenge = ChallengeDefinition {
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
                // Cloned as the placeholder target: carving depends on the hair
                // and the geometry, never on the target, so the reference can be
                // replayed before the real target exists.
                voxels: initial.clone(),
            },
            target_hair: HairstyleDefinition {
                id: format!("{id}-target"),
                name: "Trimmed Cap".to_string(),
                voxels: initial,
            },
            allowed_blocks: self.prototype.allowed_blocks.clone(),
            starter_workspace: None,
            scoring: self.prototype.scoring,
        };

        // The target is what a reference solution actually leaves behind, not a
        // sector drawn on the ellipsoid. Drawing it made items that asked for
        // hair the arm cannot reach — see [`crate::starter`] for the measured
        // damage. Deriving it means the reference scores exactly 100 by
        // definition, so every item this family emits is winnable.
        let reference =
            crate::starter::derive_reference(&challenge, region_turn, region_span, passes)
                .ok_or(GenError::UnreachableSector)?;
        if !reference.removes_any(initial_len) {
            return Err(GenError::UnreachableSector);
        }
        // Reachable is not the same as worth playing.
        //
        // Completion is an IoU over hair left standing, so an item that asks for
        // 2 voxels out of 342 scores 99.42 for doing nothing and 100 for a
        // perfect run. A learner cannot see progress in that half-point, and
        // responses to it separate nobody — the item measures noise. The seeding
        // path produced exactly that before this check existed.
        let removed = initial_len - reference.remaining.len();
        if (removed as f64) < MIN_REMOVAL_FRACTION * initial_len as f64 {
            return Err(GenError::MarginTooSmall);
        }

        challenge.starter_workspace = reference.starter_workspace();
        challenge.target_hair.voxels = reference.remaining.clone();

        // Prove it, rather than trusting the construction.
        //
        // The target is *defined* as what the reference leaves, so this should
        // be 100 by algebra — but "should be" is how the last unwinnable bank
        // shipped. Scoring the reference against the finished challenge closes
        // the loop through the same code that will score real submissions, so a
        // disagreement between carving and scoring is caught here instead of by
        // a learner who cannot finish.
        if !reference.solves(&challenge) {
            return Err(GenError::UnreachableSector);
        }

        Ok(challenge)
    }
}
