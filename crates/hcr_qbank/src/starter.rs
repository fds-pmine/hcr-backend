//! Starter programs for generated challenges.
//!
//! An authored challenge ships a starter workspace its author tuned by hand. A
//! generated one has no author, and the naive substitutes are both bad:
//!
//! * **Copy the prototype's.** Its angles are absolute and tuned to one
//!   haircut — including a `baseYaw` that sweeps one particular side of the
//!   head. [`CapTrimGenerator`](crate::CapTrimGenerator) varies `region_turn`,
//!   which rotates *which side gets trimmed*, so for most seeds the copied
//!   program would look authored and aim at the wrong place. Worse than nothing.
//! * **Ship none.** The player meets an empty canvas beside a toolbox they have
//!   to discover, which is what generated items did before this module.
//!
//! So the starter is derived: the reach pose below is fixed, and the sweep is
//! computed from the sector the generator actually trimmed.
//!
//! # Why it is then simulated
//!
//! Deriving the *aim* is arithmetic; deriving a *safe* program is not. The arm
//! cannot reach every azimuth without the head constraint stopping it, and which
//! angles are reachable depends on geometry this module does not model. So every
//! candidate is replayed through [`hcr_sim`] — the same executor that scores the
//! submission — and only a program that runs to completion is emitted. A starter
//! that halts on the head the first time a learner presses Run would be a worse
//! introduction than a blank canvas.

use hcr_contract::{ChallengeDefinition, JointConfig, Program, ProgramNode, TerminalReason};
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

/// Derive a starter workspace aimed at the sector this item trims.
///
/// `sector_turn` is the generator's `region_turn`: the sector centre as a
/// fraction of a full turn. Returns `None` when no safe candidate exists, which
/// the caller should treat as "this item ships without a starter" rather than as
/// a failure.
pub fn derive_starter_workspace(
    challenge: &ChallengeDefinition,
    sector_turn: f64,
) -> Option<Value> {
    let joints = &challenge.robot_config.joints;

    let mut blocks: Vec<(String, String, f64)> = REACH_POSE
        .iter()
        .filter_map(|(joint_id, angle)| {
            let joint = find_joint(joints, joint_id)?;
            Some((
                format!("starter-{joint_id}"),
                (*joint_id).to_string(),
                clamp_to_joint(*angle, joint),
            ))
        })
        .collect();

    // Try the aimed program first, then the reach pose alone. Dropping the sweep
    // still leaves something worth editing — the tool is over the hair and the
    // player supplies the one angle that decides where to cut.
    if let Some(sweep) = sweep_angle(joints, sector_turn) {
        let mut aimed = blocks.clone();
        aimed.push((
            "starter-base-sweep".to_string(),
            SWEEP_JOINT.to_string(),
            sweep,
        ));
        if runs_clean(challenge, &aimed) {
            return Some(to_workspace(&aimed));
        }
    }

    if blocks.is_empty() || !runs_clean(challenge, &blocks) {
        return None;
    }
    blocks.truncate(REACH_POSE.len());
    Some(to_workspace(&blocks))
}

/// The `baseYaw` that points the arm at the sector centre.
///
/// `rotation_y(baseYaw)` takes the link direction `[1, 0, 0]` to
/// `[cos θ, 0, −sin θ]` (`hcr_sim::kinematics`), and the generator measures a
/// voxel's azimuth as `atan2(dz, dx)`. So the arm's azimuth is `−baseYaw`, and
/// aiming at azimuth `sector_turn · 2π` means driving `baseYaw` to
/// `−sector_turn · 360°`.
fn sweep_angle(joints: &[JointConfig], sector_turn: f64) -> Option<f64> {
    let joint = find_joint(joints, SWEEP_JOINT)?;
    let aimed = -sector_turn * 360.0;

    // The same heading has many representations; prefer one the joint can
    // actually reach before falling back to clamping, which would silently aim
    // somewhere else.
    [aimed, aimed + 360.0, aimed - 360.0]
        .into_iter()
        .find(|angle| *angle >= joint.min_angle_deg && *angle <= joint.max_angle_deg)
        .or(Some(clamp_to_joint(aimed, joint)))
}

/// Whether the program completes without the head constraint stopping it.
fn runs_clean(challenge: &ChallengeDefinition, blocks: &[(String, String, f64)]) -> bool {
    let program = Program {
        nodes: blocks
            .iter()
            .map(|(block_id, joint_id, angle)| ProgramNode::SetJointAngle {
                joint_id: joint_id.clone(),
                angle_deg: *angle,
                source_block_id: block_id.clone(),
            })
            .collect(),
        source_block_count: blocks.len() as u32,
    };

    replay(challenge, &program, ReplayOptions::default())
        .is_ok_and(|outcome| outcome.terminal.reason == TerminalReason::Completed)
}

/// Serialize to the Blockly workspace shape the editor loads.
///
/// Mirrors `src/data/challenges/starterWorkspace.ts`: one top-level block with
/// the rest chained through `next`.
fn to_workspace(blocks: &[(String, String, f64)]) -> Value {
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

fn clamp_to_joint(angle: f64, joint: &JointConfig) -> f64 {
    angle.clamp(joint.min_angle_deg, joint.max_angle_deg)
}
