//! Voxel lattice, tool sweep and similarity.
//!
//! Ports `src/features/voxel/{voxelKey,contactDetection,similarity}.ts`.

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use hcr_contract::{Vec3, VoxelConfig, VoxelCoord};
use sha2::{Digest, Sha256};

/// A set of occupied voxels.
///
/// `BTreeSet` rather than `HashSet`: iteration order is part of the engine's
/// determinism contract, and hash iteration order is deliberately unspecified.
pub type VoxelSet = BTreeSet<VoxelCoord>;

/// Canonical string form of a voxel, `"x,y,z"`.
///
/// This is the v1 `VoxelKey` and the unit the result hash is built from.
pub fn coord_to_key(coord: &VoxelCoord) -> String {
    format!("{},{},{}", coord.x, coord.y, coord.z)
}

/// Parse the canonical `"x,y,z"` form.
///
/// Mirrors `keyToCoord` in `src/features/voxel/voxelKey.ts`, including its
/// insistence on exactly three integer components — a malformed key is a bug
/// upstream, not something to coerce.
pub fn key_to_coord(key: &str) -> Option<VoxelCoord> {
    let mut parts = key.split(',');
    let x = parts.next()?.trim().parse().ok()?;
    let y = parts.next()?.trim().parse().ok()?;
    let z = parts.next()?.trim().parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some(VoxelCoord { x, y, z })
}

/// World-space centre of a lattice cell.
pub fn voxel_coord_to_world(coord: &VoxelCoord, origin: Vec3, size: f64) -> Vec3 {
    [
        origin[0] + f64::from(coord.x) * size,
        origin[1] + f64::from(coord.y) * size,
        origin[2] + f64::from(coord.z) * size,
    ]
}

/// Slab-method segment/AABB overlap test.
///
/// Ports `segmentIntersectsAabb`, including its use of machine epsilon as the
/// "direction is parallel to this axis" threshold.
pub fn segment_intersects_aabb(start: Vec3, end: Vec3, min: Vec3, max: Vec3) -> bool {
    let mut minimum_t = 0.0_f64;
    let mut maximum_t = 1.0_f64;

    for axis in 0..3 {
        let direction = end[axis] - start[axis];
        if direction.abs() < f64::EPSILON {
            if start[axis] < min[axis] || start[axis] > max[axis] {
                return false;
            }
            continue;
        }

        let inverse = 1.0 / direction;
        let mut first_t = (min[axis] - start[axis]) * inverse;
        let mut second_t = (max[axis] - start[axis]) * inverse;
        if first_t > second_t {
            core::mem::swap(&mut first_t, &mut second_t);
        }
        minimum_t = minimum_t.max(first_t);
        maximum_t = maximum_t.min(second_t);
        if minimum_t > maximum_t {
            return false;
        }
    }

    true
}

/// Voxels the tool touches moving from `start` to `end` in one tick.
///
/// Each voxel is tested as an AABB expanded by `size/2 + sphere_radius`, against
/// the straight segment between the two end-effector positions. The segment is a
/// chord across what is physically an arc; the resulting error is bounded and
/// quantified in `docs/backend/02-DETERMINISM.md` §1.
pub fn find_swept_voxel_hits(
    start: Vec3,
    end: Vec3,
    voxels: &VoxelSet,
    voxel_config: &VoxelConfig,
    sphere_radius: f64,
) -> Vec<VoxelCoord> {
    let half_voxel = voxel_config.size / 2.0;
    let expansion = half_voxel + sphere_radius;
    let mut hits = Vec::new();

    for coord in voxels {
        let center = voxel_coord_to_world(coord, voxel_config.origin, voxel_config.size);
        let min = [
            center[0] - expansion,
            center[1] - expansion,
            center[2] - expansion,
        ];
        let max = [
            center[0] + expansion,
            center[1] + expansion,
            center[2] + expansion,
        ];

        if segment_intersects_aabb(start, end, min, max) {
            hits.push(*coord);
        }
    }

    hits
}

/// How well a run performed the cut the challenge asked for, scaled to 0..=100.
///
/// Ports `calculateTrimScore` in `src/features/voxel/similarity.ts`. Jaccard
/// overlap between the hair removed and the hair the target says to remove:
///
/// ```text
/// asked   = initial \ target
/// removed = initial \ result
/// score   = |removed ∩ asked| / |removed ∪ asked|
/// ```
///
/// # Why not compare the hair left standing
///
/// That is what this used to do, and its floor was not zero. Most of a hairstyle
/// is never meant to be touched, so an empty program already matched nearly all
/// of it: on the shipped challenge the target keeps 229 of 241 voxels, and doing
/// nothing scored **95.02**. The whole distance between "did nothing" and
/// "perfect" was five points on top of a floor that measured the hairstyle
/// rather than the learner, and everything downstream — the ability seed above
/// all — inherited it.
///
/// A challenge that asks for nothing is satisfied by doing nothing, so an empty
/// union scores 100.
pub fn calculate_trim_score(initial: &VoxelSet, target: &VoxelSet, result: &VoxelSet) -> f64 {
    let mut intersection = 0usize;
    let mut union = 0usize;

    for key in initial {
        let asked = !target.contains(key);
        let removed = !result.contains(key);
        if asked || removed {
            union += 1;
            if asked && removed {
                intersection += 1;
            }
        }
    }

    // Hair conjured from nothing is not something the engine can produce, but a
    // score is the wrong place to discover that: count it against the run.
    union += result.iter().filter(|key| !initial.contains(*key)).count();

    if union == 0 {
        100.0
    } else {
        (intersection as f64 / union as f64) * 100.0
    }
}

/// SHA-256 over the lexicographically sorted voxel keys, joined by newlines.
///
/// The sort is a **byte sort of the key strings**, not a numeric sort of the
/// coordinates — `"10,0,0"` precedes `"2,0,0"`. Sorting at all is what makes the
/// hash comparable across languages, since neither JS `Set` nor Rust `HashSet`
/// has a canonical iteration order.
pub fn result_voxels_hash(voxels: &VoxelSet) -> String {
    let mut keys: Vec<String> = voxels.iter().map(coord_to_key).collect();
    keys.sort_unstable();

    let mut hasher = Sha256::new();
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            hasher.update(b"\n");
        }
        hasher.update(key.as_bytes());
    }

    let digest = hasher.finalize();
    let mut out = String::with_capacity(64);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
