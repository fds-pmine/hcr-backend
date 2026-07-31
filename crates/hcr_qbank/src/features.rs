//! Radiographic features of a challenge, used to predict its difficulty.
//!
//! Everything here is computed from the [`ChallengeDefinition`] alone — no
//! solver, no response data — so a freshly generated item can be placed on the
//! difficulty scale before anyone has attempted it.

use std::collections::BTreeSet;

use hcr_contract::{ChallengeDefinition, VoxelCoord};
use hcr_sim::voxel_coord_to_world;

/// Feature vector for the difficulty model.
///
/// All fields except `removal_volume` are normalised to `[0, 1]`, which keeps the
/// model's coefficients interpretable as "logits of difficulty per unit of
/// feature".
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ChallengeFeatures {
    /// `ln(1 + n)` where `n` is the number of voxels that must be removed.
    ///
    /// Log-scaled because the jump from 10 to 20 voxels matters far more than
    /// the jump from 200 to 210.
    pub removal_volume: f64,
    /// Fraction of target voxels sitting on the target's surface.
    ///
    /// A wispy or filigreed target is mostly boundary, and boundary is where IoU
    /// punishes imprecision.
    pub boundary_ratio: f64,
    /// How far the target departs from left/right symmetry, `0` = mirror-perfect.
    ///
    /// Symmetric crops can be programmed once and mirrored; asymmetric ones
    /// cannot.
    pub asymmetry: f64,
    /// Mean reach demand over the removal set, `0` until 70 % of reach is needed.
    pub reach_strain: f64,
    /// Mean closeness of the removal set to the head ellipsoid.
    ///
    /// Voxels hugging the scalp force the arm through the region where the
    /// head-safety constraint bites.
    pub head_proximity: f64,
}

impl ChallengeFeatures {
    /// Extract features from a challenge.
    pub fn extract(challenge: &ChallengeDefinition) -> Self {
        let initial: BTreeSet<VoxelCoord> = challenge.initial_hair.voxels.iter().copied().collect();
        let target: BTreeSet<VoxelCoord> = challenge.target_hair.voxels.iter().copied().collect();

        let removal: Vec<VoxelCoord> = initial.difference(&target).copied().collect();

        Self {
            removal_volume: (1.0 + removal.len() as f64).ln(),
            boundary_ratio: boundary_ratio(&target),
            asymmetry: asymmetry(&target, challenge),
            reach_strain: reach_strain(&removal, challenge),
            head_proximity: head_proximity(&removal, challenge),
        }
    }

    /// Features in the model's canonical order, excluding the intercept.
    pub fn as_array(&self) -> [f64; 5] {
        [
            self.removal_volume,
            self.boundary_ratio,
            self.asymmetry,
            self.reach_strain,
            self.head_proximity,
        ]
    }
}

/// Share of target voxels with at least one exposed face.
fn boundary_ratio(target: &BTreeSet<VoxelCoord>) -> f64 {
    if target.is_empty() {
        return 0.0;
    }

    let neighbours = |v: VoxelCoord| {
        [
            VoxelCoord { x: v.x + 1, ..v },
            VoxelCoord { x: v.x - 1, ..v },
            VoxelCoord { y: v.y + 1, ..v },
            VoxelCoord { y: v.y - 1, ..v },
            VoxelCoord { z: v.z + 1, ..v },
            VoxelCoord { z: v.z - 1, ..v },
        ]
    };

    let boundary = target
        .iter()
        .filter(|voxel| {
            neighbours(**voxel)
                .iter()
                .any(|neighbour| !target.contains(neighbour))
        })
        .count();

    boundary as f64 / target.len() as f64
}

/// Jaccard distance between the target and its mirror image.
///
/// The mirror plane is the head's sagittal plane. The arm's base sits on the
/// world origin and the head is offset along +X, so left/right for the arm is the
/// **Z** axis — that is the axis a symmetric haircut is symmetric about.
fn asymmetry(target: &BTreeSet<VoxelCoord>, challenge: &ChallengeDefinition) -> f64 {
    if target.is_empty() {
        return 0.0;
    }

    let voxel = &challenge.voxel_config;
    // Lattice column of the head centre along Z, rounded to the nearest cell.
    let centre_z = ((voxel.head_center[2] - voxel.origin[2]) / voxel.size).round() as i32;

    let mirrored: BTreeSet<VoxelCoord> = target
        .iter()
        .map(|v| VoxelCoord {
            z: 2 * centre_z - v.z,
            ..*v
        })
        .collect();

    let union = target.union(&mirrored).count();
    if union == 0 {
        return 0.0;
    }
    let intersection = target.intersection(&mirrored).count();
    (union - intersection) as f64 / union as f64
}

/// Mean normalised reach demand, measured from the shoulder.
fn reach_strain(removal: &[VoxelCoord], challenge: &ChallengeDefinition) -> f64 {
    if removal.is_empty() {
        return 0.0;
    }

    let geometry = &challenge.robot_config.geometry;
    let shoulder = [
        geometry.base_position[0],
        geometry.base_position[1] + geometry.shoulder_height,
        geometry.base_position[2],
    ];
    let max_reach = geometry.upper_arm_length + geometry.forearm_length + geometry.tool_length;
    if max_reach <= 0.0 {
        return 0.0;
    }

    let total: f64 = removal
        .iter()
        .map(|voxel| {
            let world = voxel_coord_to_world(
                voxel,
                challenge.voxel_config.origin,
                challenge.voxel_config.size,
            );
            let distance = ((world[0] - shoulder[0]).powi(2)
                + (world[1] - shoulder[1]).powi(2)
                + (world[2] - shoulder[2]).powi(2))
            .sqrt();
            // Nothing counts until 70 % of reach; saturates at full extension.
            (((distance / max_reach) - 0.7) / 0.3).clamp(0.0, 1.0)
        })
        .sum();

    total / removal.len() as f64
}

/// Mean closeness of the removal set to the head surface.
///
/// Uses the same normalised ellipsoid metric as the collision constraint: `q = 1`
/// is the surface, `q > 1` is outside.
fn head_proximity(removal: &[VoxelCoord], challenge: &ChallengeDefinition) -> f64 {
    if removal.is_empty() {
        return 0.0;
    }

    let voxel = &challenge.voxel_config;
    if voxel.head_scale.iter().any(|axis| *axis <= 0.0) {
        return 0.0;
    }

    let total: f64 = removal
        .iter()
        .map(|coord| {
            let world = voxel_coord_to_world(coord, voxel.origin, voxel.size);
            let q = (0..3)
                .map(|axis| {
                    let d = (world[axis] - voxel.head_center[axis]) / voxel.head_scale[axis];
                    d * d
                })
                .sum::<f64>()
                .sqrt();
            // 1.0 (touching the scalp) → 1.0; 1.6 and beyond → 0.
            ((1.6 - q) / 0.6).clamp(0.0, 1.0)
        })
        .sum();

    total / removal.len() as f64
}
