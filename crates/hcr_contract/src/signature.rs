//! Challenge signature — `fnv1a64` over everything Cutter Grid planning depends on.
//!
//! Ports `src/features/cutter-grid/signature.ts`. A trajectory plan names the
//! signature it was planned against; the server recomputes it from its own copy
//! of the challenge and refuses a mismatch. That single comparison covers a
//! surprising amount: joint travel, arm and collision dimensions, lattice
//! placement, head ellipsoid, tool radius and both hairstyles. Change any of
//! them and every previously planned trajectory stops being about this challenge.
//!
//! # Why the JSON is written by hand
//!
//! The hash is taken over `JSON.stringify(...)` of a JavaScript object literal,
//! so the bytes depend on key order and on JavaScript's number formatting.
//! `serde_json` agrees on neither: it would sort nothing and would print `45.0`
//! where JavaScript prints `45`. Reproducing the string exactly is the only way
//! the two languages arrive at the same digest, so the field order below is
//! **normative** and must track `signature.ts` literally.
//!
//! [`tests::shipped_challenge_matches_the_bundled_profile`] pins this against the
//! signature the frontend's own certified profile carries. If that test fails,
//! this file drifted from `signature.ts` and no Cutter Grid submission will be
//! accepted until they agree again.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::domain::{ChallengeDefinition, JointConfig, RobotGeometryConfig, Vec3, VoxelConfig};

/// Profile version the V2 signature is taken at.
///
/// Mirrors `CUTTER_GRID_PROFILE_V2_VERSION`.
const PROFILE_V2_VERSION: u32 = 2;

/// Planner build the V2 signature is taken at.
const LADDER_PLANNER_VERSION: &str = "cutter-grid-ladder-v2";

/// The ladder planner's search constants, verbatim from
/// `CUTTER_GRID_LADDER_SIGNATURE_CONFIG`.
///
/// These are in the signature deliberately: two planners that search differently
/// can disagree about which programs are reachable, so a plan built under one set
/// of constants must not be replayed as though it were built under another.
const LADDER_CONFIG_JSON: &str = concat!(
    r#"{"candidateSeedBudgets":[24,96,384],"#,
    r#""candidateDeduplicationDistance":0.01,"#,
    r#""candidateLimit":128,"#,
    r#""entryOptionLimit":32,"#,
    r#""edgeMaximumJointDeltaDeg":0.5,"#,
    r#""edgeMaximumEndEffectorDistanceDivisor":16,"#,
    r#""entryPrmHaltonNodes":[2048,8192],"#,
    r#""entryPrmNeighbors":24}"#
);

/// 64-bit FNV-1a, as the frontend computes it.
///
/// The frontend folds over `charCodeAt`, which yields UTF-16 code units, so this
/// does too. Every input here is ASCII, where code units and bytes coincide —
/// but matching the source means the next non-ASCII challenge id does not
/// silently produce two different hashes.
pub fn fnv1a64(input: &str) -> String {
    const PRIME: u64 = 1_099_511_628_211;
    let mut value: u64 = 14_695_981_039_346_656_037;
    for unit in input.encode_utf16() {
        value ^= u64::from(unit);
        value = value.wrapping_mul(PRIME);
    }
    format!("{value:016x}")
}

/// Signature of `challenge` for the V2 (ladder) planner.
pub fn cutter_grid_challenge_signature_v2(challenge: &ChallengeDefinition) -> String {
    fnv1a64(&signature_document(challenge))
}

/// The exact string the frontend hands to `fnv1a64`.
///
/// Exposed for tests and for operators diffing a signature mismatch: comparing
/// two documents says *what* changed, where comparing two hashes only says that
/// something did.
pub fn signature_document(challenge: &ChallengeDefinition) -> String {
    let mut out = String::with_capacity(8192);

    out.push_str(r#"{"profileVersion":"#);
    out.push_str(&number(f64::from(PROFILE_V2_VERSION)));
    out.push_str(r#","plannerVersion":""#);
    out.push_str(LADDER_PLANNER_VERSION);
    out.push_str(r#"","ladder":"#);
    out.push_str(LADDER_CONFIG_JSON);

    out.push_str(r#","joints":["#);
    for (index, joint) in challenge.robot_config.joints.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_joint(&mut out, joint);
    }
    out.push(']');

    out.push_str(r#","geometry":"#);
    write_geometry(&mut out, &challenge.robot_config.geometry);

    out.push_str(r#","voxelConfig":"#);
    write_voxel_config(&mut out, &challenge.voxel_config);

    out.push_str(r#","initialHair":"#);
    write_sorted_voxel_keys(&mut out, &challenge.initial_hair.voxels);

    out.push_str(r#","targetHair":"#);
    write_sorted_voxel_keys(&mut out, &challenge.target_hair.voxels);

    out.push_str(r#","cutterRadius":"#);
    out.push_str(&number(challenge.robot_config.geometry.tool_radius));
    out.push('}');

    out
}

/// Field order is `signature.ts`'s own, not the challenge's: the frontend
/// rebuilds each joint with an explicit literal, dropping `name`.
fn write_joint(out: &mut String, joint: &JointConfig) {
    out.push_str(r#"{"id":""#);
    write_escaped(out, &joint.id);
    out.push_str(r#"","axis":""#);
    out.push_str(match joint.axis {
        crate::domain::Axis::X => "x",
        crate::domain::Axis::Y => "y",
        crate::domain::Axis::Z => "z",
    });
    out.push_str(r#"","minAngleDeg":"#);
    out.push_str(&number(joint.min_angle_deg));
    out.push_str(r#","maxAngleDeg":"#);
    out.push_str(&number(joint.max_angle_deg));
    out.push_str(r#","initialAngleDeg":"#);
    out.push_str(&number(joint.initial_angle_deg));
    out.push_str(r#","speedDegPerSec":"#);
    out.push_str(&number(joint.speed_deg_per_sec));

    // A joint with no servo mapping contributes no `servo` key at all.
    // `JSON.stringify` drops keys whose value is `undefined`, so emitting
    // `"servo":null` here would change the digest — this is exactly the kind of
    // difference that would be invisible until every plan started bouncing.
    if let Some(servo) = &joint.servo {
        out.push_str(r#","servo":{"axis":""#);
        out.push_str(match servo.axis {
            crate::domain::ServoAxisId::X => "X",
            crate::domain::ServoAxisId::Y => "Y",
            crate::domain::ServoAxisId::Z => "Z",
            crate::domain::ServoAxisId::B => "B",
            crate::domain::ServoAxisId::E => "E",
        });
        out.push_str(r#"","centerDeg":"#);
        out.push_str(&number(servo.center_deg));
        out.push_str(r#","direction":"#);
        out.push_str(&number(f64::from(servo.direction)));
        out.push_str(r#","offsetDeg":"#);
        out.push_str(&number(servo.offset_deg));
        out.push('}');
    }
    out.push('}');
}

fn write_geometry(out: &mut String, geometry: &RobotGeometryConfig) {
    out.push_str(r#"{"basePosition":"#);
    write_vec3(out, geometry.base_position);
    out.push_str(r#","shoulderHeight":"#);
    out.push_str(&number(geometry.shoulder_height));
    out.push_str(r#","upperArmLength":"#);
    out.push_str(&number(geometry.upper_arm_length));
    out.push_str(r#","forearmLength":"#);
    out.push_str(&number(geometry.forearm_length));
    out.push_str(r#","toolLength":"#);
    out.push_str(&number(geometry.tool_length));
    out.push_str(r#","toolRadius":"#);
    out.push_str(&number(geometry.tool_radius));
    out.push_str(r#","collision":{"linkRadius":"#);
    out.push_str(&number(geometry.collision.link_radius));
    out.push_str(r#","jointRadius":"#);
    out.push_str(&number(geometry.collision.joint_radius));
    out.push_str(r#","toolShaftRadius":"#);
    out.push_str(&number(geometry.collision.tool_shaft_radius));
    out.push_str(r#","headClearance":"#);
    out.push_str(&number(geometry.collision.head_clearance));
    out.push_str("}}");
}

fn write_voxel_config(out: &mut String, voxel_config: &VoxelConfig) {
    out.push_str(r#"{"origin":"#);
    write_vec3(out, voxel_config.origin);
    out.push_str(r#","size":"#);
    out.push_str(&number(voxel_config.size));
    out.push_str(r#","headCenter":"#);
    write_vec3(out, voxel_config.head_center);
    out.push_str(r#","headScale":"#);
    write_vec3(out, voxel_config.head_scale);
    out.push('}');
}

fn write_vec3(out: &mut String, value: Vec3) {
    out.push('[');
    for (index, component) in value.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push_str(&number(*component));
    }
    out.push(']');
}

/// Hair travels as sorted `"x,y,z"` keys.
///
/// The sort is JavaScript's default — lexicographic over the key strings, so
/// `"10,0,0"` precedes `"2,0,0"`. Sorting the coordinates numerically would
/// produce a different document and therefore a different signature.
fn write_sorted_voxel_keys(out: &mut String, voxels: &[crate::domain::VoxelCoord]) {
    let mut keys: Vec<String> = voxels
        .iter()
        .map(|coord| format!("{},{},{}", coord.x, coord.y, coord.z))
        .collect();
    keys.sort_unstable();

    out.push('[');
    for (index, key) in keys.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(key);
        out.push('"');
    }
    out.push(']');
}

/// Write a JSON string body, escaping what `JSON.stringify` escapes.
///
/// Every identifier reaching this today is a plain ASCII joint id, so this never
/// does anything. It exists because the alternative — assuming that stays true —
/// fails by producing a *plausible* wrong digest rather than an error.
fn write_escaped(out: &mut String, value: &str) {
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            control if (control as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", control as u32));
            }
            other => out.push(other),
        }
    }
}

/// Format a number the way `JSON.stringify` would.
///
/// The one divergence from Rust's own formatting that matters: JavaScript prints
/// an integral double without a fractional part (`45`, not `45.0`). Beyond that
/// both languages emit the shortest string that round-trips, so they agree.
///
/// Values outside `±1e21` would take JavaScript into exponential notation. No
/// challenge field is remotely near that — these are angles and centimetres — and
/// pretending to handle it would be untested code guarding an impossible input.
fn number(value: f64) -> String {
    // `JSON.stringify(-0)` is `"0"`.
    let value = if value == 0.0 { 0.0 } else { value };
    if value.fract() == 0.0 && value.abs() < 1e21 {
        format!("{}", value as i64)
    } else {
        format!("{value}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fnv1a64_matches_the_reference_vector() {
        // The empty string is the algorithm's published offset basis, which is
        // the one value a transcription slip in either constant cannot survive.
        assert_eq!(fnv1a64(""), "cbf29ce484222325");
        assert_eq!(fnv1a64("a"), "af63dc4c8601ec8c");
        assert_eq!(fnv1a64("foobar"), "85944171f73967e8");
    }

    /// The signature the frontend's own certified V2 profile was built under.
    ///
    /// Read from `tests/fixtures/cutter-grid-profile-v2.json` in the frontend
    /// repository. If this constant and that file ever disagree, the frontend
    /// moved and this port has to follow.
    const SHIPPED_CHALLENGE_SIGNATURE_V2: &str = "7d5a4afd61db49ea";

    /// Same fixture `hcr_service::seed` reads, so the challenge under test is
    /// the one the server actually serves.
    const VECTORS: &str = include_str!("../../hcr_sim/tests/fixtures/vectors.json");

    fn shipped_challenge() -> crate::domain::ChallengeDefinition {
        let vectors: serde_json::Value =
            serde_json::from_str(VECTORS).expect("conformance fixture parses");
        serde_json::from_value(vectors["challenge"].clone()).expect("challenge parses")
    }

    /// The whole port stands or falls here.
    ///
    /// Every rule this file encodes — key order, the dropped `servo` key, the
    /// lexicographic voxel sort, JavaScript's integer formatting — is only
    /// checkable in aggregate, because the digest is all the frontend exposes.
    /// One wrong byte anywhere and no Cutter Grid submission would ever be
    /// accepted, with nothing but `SIGNATURE_MISMATCH` to say why.
    #[test]
    fn shipped_challenge_matches_the_bundled_profile() {
        let signature = cutter_grid_challenge_signature_v2(&shipped_challenge());
        assert_eq!(
            signature,
            SHIPPED_CHALLENGE_SIGNATURE_V2,
            "signature document was:\n{}",
            signature_document(&shipped_challenge())
        );
    }

    /// A joint range edit must move the signature.
    ///
    /// This is the property the whole mechanism exists for: the certified profile
    /// was planned against particular joint travel, so widening it has to
    /// invalidate every plan built before the change rather than let them through
    /// against an arm that no longer matches.
    #[test]
    fn widening_a_joint_range_changes_the_signature() {
        let challenge = shipped_challenge();
        let before = cutter_grid_challenge_signature_v2(&challenge);

        let mut widened = challenge;
        widened.robot_config.joints[0].max_angle_deg += 1.0;
        let after = cutter_grid_challenge_signature_v2(&widened);

        assert_ne!(before, after);
    }

    #[test]
    fn integral_numbers_lose_their_fractional_part() {
        assert_eq!(number(45.0), "45");
        assert_eq!(number(-0.0), "0");
        assert_eq!(number(0.0), "0");
        assert_eq!(number(72.5), "72.5");
        assert_eq!(number(0.16), "0.16");
        assert_eq!(number(-1.0), "-1");
    }
}
