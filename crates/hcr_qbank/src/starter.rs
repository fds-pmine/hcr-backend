//! Reference solutions and starter programs for generated challenges.
//!
//! # Why the target is derived from a program, not from geometry
//!
//! The generator used to draw the trim sector on the ellipsoid and call the
//! result the target. Nothing checked the arm could reach it, and it could not:
//! measured across the shipped bank, `Cap Trim 30%` asked for 91 voxels of which
//! 20 were reachable, `Cap Trim 72%` asked 284 with 145 reachable, and
//! `Cap Trim 94%` asked 345 with 232 reachable. Best achievable completion was
//! 77.95, 79.53 and 15.67 — no learner could ever score 100, and on the first
//! two the entire reward for skilled play was ~5 points over doing nothing.
//!
//! An earlier comment in [`crate::generator`] defended this as "a legitimate way
//! for an item to be hard". It is not. An item nobody can finish never shows the
//! success it was built to teach, and its responses are compressed into a band
//! too narrow to carry signal — which is the one thing calibration needs.
//!
//! So the order is inverted. A **reference program** is derived from the item's
//! parameters and replayed through [`hcr_sim`], and whatever hair it leaves
//! standing *becomes* the target. The item is then winnable by construction:
//! that program scores exactly 100, because the target is defined as its result.
//! This is the same rule the authored challenge and the eight lessons already
//! follow — targets come from programs that run.
//!
//! # Why the starter is a prefix
//!
//! The starter is the reference with its carving moves removed: the tool ends up
//! over the crown, and the learner supplies the sweeps that do the cutting. A
//! deeper trim needs more passes, so a harder item withholds more — difficulty
//! is how much of the reference the learner has to rebuild.
//!
//! Deriving the *aim* is arithmetic; deriving a *safe* program is not. The arm
//! cannot reach every azimuth without the head constraint stopping it, so every
//! candidate is replayed through the same executor that scores submissions, and
//! only the longest prefix that runs to completion is kept.

use hcr_contract::{ChallengeDefinition, JointConfig, Program, ProgramNode, TerminalReason, VoxelCoord};
use hcr_sim::{ReplayOptions, replay};
use serde_json::{Value, json};

/// Blockly block type for an absolute joint move.
///
/// Must match `BLOCK_TYPES.setJointAngle` in
/// `src/features/blockly/blockConstants.ts` — the workspace produced here is
/// loaded by that editor, so the two names are one contract.
const BLOCK_TYPE_SET_JOINT_ANGLE: &str = "hcr_set_joint_angle";

/// Field names, matching `BLOCK_FIELDS`.
const FIELD_JOINT_ID: &str = "JOINT_ID";
const FIELD_ANGLE: &str = "ANGLE";

/// The pose that lifts the tool over the crown before the sweep.
///
/// Fixed rather than derived: it depends on the arm, which every item in a
/// family shares, whereas only the sweep depends on the item. Each value is
/// clamped to the joint's configured travel, so a family built on a different
/// arm degrades to that arm's limits instead of producing an invalid program.
const REACH_POSE: &[(&str, f64)] = &[
    ("shoulderRoll", 15.0),
    ("shoulder", 80.0),
    ("elbow", 0.0),
    ("wrist", -80.0),
];

/// Joint the sweep drives.
const SWEEP_JOINT: &str = "baseYaw";

/// Joint raised between passes so a second sweep cuts at a different height.
const HEIGHT_JOINT: &str = "shoulder";

/// Degrees of `shoulder` travel between successive passes.
const PASS_HEIGHT_STEP: f64 = 10.0;

/// Upper bound on passes, so a large `trim_depth` cannot produce a program
/// longer than a learner could plausibly rebuild.
const MAX_PASSES: u32 = 4;

/// One `set joint angle` block: its Blockly id, the joint, and the angle.
type Block = (String, String, f64);

/// A program that solves an item, and the hair it leaves standing.
#[derive(Debug, Clone)]
pub struct ReferenceRun {
    blocks: Vec<Block>,
    /// Hair remaining once the reference has run — the item's target.
    pub remaining: Vec<VoxelCoord>,
    /// How many of the leading blocks merely position the tool.
    positioning: usize,
}

impl ReferenceRun {
    /// The solution itself, as a program.
    ///
    /// Exposed so the winnability invariant can be *checked* rather than
    /// asserted: replay this against the finished challenge and the completion
    /// score must be 100, because the target is defined as what it leaves.
    pub fn program(&self) -> Program {
        to_program(&self.blocks)
    }

    /// The starter workspace: positioning only, with the carving withheld.
    pub fn starter_workspace(&self) -> Option<Value> {
        let visible = &self.blocks[..self.positioning.min(self.blocks.len())];
        if visible.is_empty() {
            return None;
        }
        Some(to_workspace(visible))
    }

    /// Whether the reference actually cuts anything.
    pub fn removes_any(&self, initial_len: usize) -> bool {
        self.remaining.len() < initial_len
    }

    /// Whether replaying this reference against `challenge` scores a perfect
    /// completion.
    ///
    /// The check that makes "winnable by construction" a fact rather than a
    /// claim: it runs the solution through the same executor and scorer a
    /// submission goes through, against the finished challenge.
    pub fn solves(&self, challenge: &ChallengeDefinition) -> bool {
        replay(challenge, &self.program(), ReplayOptions::default()).is_ok_and(|outcome| {
            outcome.terminal.reason == TerminalReason::Completed
                && (outcome.score.completion_score - 100.0).abs() < 1e-6
        })
    }
}

/// Derive a reference solution for an item and replay it.
///
/// `challenge` needs only its initial hairstyle to be final — carving depends on
/// the hair and the geometry, never on the target, so callers pass a challenge
/// whose target is still a placeholder and adopt [`ReferenceRun::remaining`] as
/// the real one.
///
/// `sector_turn` is the generator's `region_turn` (sector centre as a fraction
/// of a full turn), `sector_span` its `region_span` (half-width as a fraction of
/// a half turn), and `passes` how many sweeps the trim depth calls for.
///
/// Returns `None` when nothing safe can be built, which the caller should treat
/// as a degenerate parameter draw and skip.
pub fn derive_reference(
    challenge: &ChallengeDefinition,
    sector_turn: f64,
    sector_span: f64,
    passes: u32,
) -> Option<ReferenceRun> {
    let joints = &challenge.robot_config.joints;

    let mut blocks: Vec<Block> = REACH_POSE
        .iter()
        .filter_map(|(joint_id, angle)| {
            let joint = find_joint(joints, joint_id)?;
            Some((
                format!("starter-{joint_id}"),
                (*joint_id).to_string(),
                place_angle(*angle, joint),
            ))
        })
        .collect();
    if blocks.is_empty() {
        return None;
    }
    let positioning = blocks.len();

    // Sweep between the sector's two edges, alternating direction each pass so a
    // second pass retraces the same arc at a new height rather than jumping.
    let (from, to) = sector_bounds(joints, sector_turn, sector_span)?;
    let height = find_joint(joints, HEIGHT_JOINT);
    // Geometric, because the per-pass step below is arithmetic on it. Only the
    // value that reaches a block is converted.
    let base_height = height.map(|joint| {
        let (min, max) = geometric_range(joint);
        80.0_f64.clamp(min, max)
    });

    for pass in 0..passes.clamp(1, MAX_PASSES) {
        if pass > 0 {
            if let (Some(joint), Some(base)) = (height, base_height) {
                let lowered = place_angle(base - PASS_HEIGHT_STEP * f64::from(pass), joint);
                blocks.push((
                    format!("reference-height-{pass}"),
                    HEIGHT_JOINT.to_string(),
                    lowered,
                ));
            }
        }
        let (start, end) = if pass % 2 == 0 { (from, to) } else { (to, from) };
        blocks.push((
            format!("reference-sweep-{pass}-a"),
            SWEEP_JOINT.to_string(),
            start,
        ));
        blocks.push((
            format!("reference-sweep-{pass}-b"),
            SWEEP_JOINT.to_string(),
            end,
        ));
    }

    // Keep the longest prefix that completes. A sweep the head constraint stops
    // is not a reference solution — the target would encode a run that failed.
    while blocks.len() > positioning {
        if let Some(remaining) = run_to_completion(challenge, &blocks) {
            return Some(ReferenceRun {
                blocks,
                remaining,
                positioning,
            });
        }
        blocks.pop();
    }

    None
}

/// Build the program a block list describes.
fn to_program(blocks: &[Block]) -> Program {
    Program {
        nodes: blocks
            .iter()
            .map(|(block_id, joint_id, angle)| ProgramNode::SetJointAngle {
                joint_id: joint_id.clone(),
                angle_deg: *angle,
                source_block_id: block_id.clone(),
            })
            .collect(),
        source_block_count: blocks.len() as u32,
    }
}

/// Replay `blocks` and return the hair left standing, if the run completed.
fn run_to_completion(
    challenge: &ChallengeDefinition,
    blocks: &[Block],
) -> Option<Vec<VoxelCoord>> {
    let program = to_program(blocks);

    let outcome = replay(challenge, &program, ReplayOptions::default()).ok()?;
    if outcome.terminal.reason != TerminalReason::Completed {
        return None;
    }
    let mut remaining: Vec<VoxelCoord> = outcome.remaining_voxels.into_iter().collect();
    // Deterministic order: the definition is hashed and compared across runs.
    remaining.sort_by_key(|voxel| (voxel.x, voxel.y, voxel.z));
    Some(remaining)
}

/// The two `baseYaw` angles bounding the trimmed sector.
///
/// `rotation_y(baseYaw)` takes the link direction `[1, 0, 0]` to
/// `[cos θ, 0, −sin θ]` (`hcr_sim::kinematics`), and the generator measures a
/// voxel's azimuth as `atan2(dz, dx)`. So the arm's azimuth is `−baseYaw`, and
/// covering azimuths `centre ± half_width` means driving `baseYaw` across
/// `−(centre ± half_width)`, in degrees.
fn sector_bounds(joints: &[JointConfig], sector_turn: f64, sector_span: f64) -> Option<(f64, f64)> {
    let joint = find_joint(joints, SWEEP_JOINT)?;
    let centre = -sector_turn * 360.0;
    let half_width = sector_span * 180.0;
    // Headings are geometric; the joint's own limits are not.
    let (min_deg, max_deg) = geometric_range(joint);

    let resolve = |angle: f64| {
        // The same heading has many representations; prefer one the joint can
        // reach before clamping, which would silently aim somewhere else.
        [angle, angle + 360.0, angle - 360.0]
            .into_iter()
            .find(|candidate| *candidate >= min_deg && *candidate <= max_deg)
            .unwrap_or_else(|| angle.clamp(min_deg, max_deg))
    };

    let low = resolve(centre - half_width);
    let high = resolve(centre + half_width);
    if (low - high).abs() < f64::EPSILON {
        return None;
    }
    Some((place_angle(low, joint), place_angle(high, joint)))
}

/// Serialize to the Blockly workspace shape the editor loads.
///
/// Mirrors `src/data/challenges/starterWorkspace.ts`: one top-level block with
/// the rest chained through `next`.
fn to_workspace(blocks: &[Block]) -> Value {
    let mut chained: Option<Value> = None;
    for (block_id, joint_id, angle) in blocks.iter().rev() {
        let mut block = json!({
            "type": BLOCK_TYPE_SET_JOINT_ANGLE,
            "id": block_id,
            "fields": { FIELD_JOINT_ID: joint_id, FIELD_ANGLE: angle },
        });
        if let Some(next) = chained.take() {
            block["next"] = json!({ "block": next });
        }
        chained = Some(block);
    }

    let mut root = chained.expect("callers never pass an empty block list");
    root["x"] = json!(40);
    root["y"] = json!(40);

    json!({ "blocks": { "languageVersion": 0, "blocks": [root] } })
}

fn find_joint<'a>(joints: &'a [JointConfig], joint_id: &str) -> Option<&'a JointConfig> {
    joints.iter().find(|joint| joint.id == joint_id)
}

/// The joint's travel in **geometric** degrees.
///
/// Everything in this module reasons geometrically — `sector_bounds` derives a
/// heading from an azimuth, `REACH_POSE` describes an arm shape — while the
/// joint's configured limits are servo degrees. A `direction: -1` servo maps its
/// minimum to the geometric maximum, hence the reorder.
pub(crate) fn geometric_range(joint: &JointConfig) -> (f64, f64) {
    match joint.servo.as_ref() {
        None => (joint.min_angle_deg, joint.max_angle_deg),
        Some(servo) => {
            let low = servo.to_geometric_deg(joint.min_angle_deg);
            let high = servo.to_geometric_deg(joint.max_angle_deg);
            (low.min(high), low.max(high))
        }
    }
}

/// Clamp a geometric angle to the joint's travel and return it in the units a
/// program carries — servo degrees wherever the joint drives a servo.
///
/// Every angle this module emits into a block goes through here. Emitting a
/// geometric angle directly would be replayed as a servo command and put the arm
/// somewhere else entirely.
fn place_angle(angle: f64, joint: &JointConfig) -> f64 {
    let (min, max) = geometric_range(joint);
    let clamped = angle.clamp(min, max);
    joint
        .servo
        .as_ref()
        .map_or(clamped, |servo| servo.to_servo_deg(clamped))
}
