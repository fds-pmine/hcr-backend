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

/// Intersection-over-union of two voxel sets, scaled to 0..=100.
///
/// Two empty sets score 100, per SPEC v0.3 §10.3.
pub fn calculate_voxel_iou(target: &VoxelSet, result: &VoxelSet) -> f64 {
    if target.is_empty() && result.is_empty() {
        return 100.0;
    }

    let intersection_size = target.iter().filter(|key| result.contains(*key)).count();
    let union_size = target.len() + result.len() - intersection_size;

    if union_size == 0 {
        100.0
    } else {
        (intersection_size as f64 / union_size as f64) * 100.0
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
