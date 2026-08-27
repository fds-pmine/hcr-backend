//! Server-only compact Cutter Grid V4 planning primitives.
//!
//! This module owns source-program compilation, lattice geometry, multi-branch
//! endpoint IK, and compact PTP graph search. Dynamic certification, actual
//! cutter sweeps, plan serialization, and HTTP are added in later phases.
//! The module remains pure so every caller shares the same deterministic
//! action and candidate semantics without importing a web stack.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use serde::Serialize;

use hcr_contract::{
    CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION, ChallengeDefinition, CutterGridCoord,
    CutterGridDirection, CutterGridNode, CutterGridProgramV4, JointConfig, JointId, Vec3,
    VoxelConfig, VoxelCoord,
};

use crate::collision::{find_robot_head_collision, measure_robot_head_clearance};
use crate::kinematics::compute_robot_pose;
use crate::state::JointAngles;
use crate::voxel::{VoxelSet, coord_to_key, find_swept_voxel_hits, voxel_coord_to_world};

/// V4's fixed DLS numerical parameters, shared with the TypeScript Worker.
pub const CUTTER_GRID_V4_IK_MAX_ITERATIONS: usize = 80;
/// Finite-difference joint step, in servo degrees.
pub const CUTTER_GRID_V4_IK_JACOBIAN_STEP_DEG: f64 = 0.1;
/// Damped least-squares regularizer.
pub const CUTTER_GRID_V4_IK_DAMPING: f64 = 0.05;
/// Maximum absolute DLS update per joint and iteration, in degrees.
pub const CUTTER_GRID_V4_IK_MAX_UPDATE_DEG: f64 = 2.0;
/// V4 endpoint candidates stay unquantized; this is kept for explicit parity
/// with the historical solver and documentation.
pub const CUTTER_GRID_V4_IK_ANGLE_QUANTUM_DEG: f64 = 0.1;
/// Initial V4 Halton candidate budget per endpoint.
pub const CUTTER_GRID_V4_INITIAL_SEED_BUDGET: usize = 12;
/// One targeted V4 endpoint expansion budget.
pub const CUTTER_GRID_V4_EXPANDED_SEED_BUDGET: usize = 48;
/// Maximum logical commands after repeat expansion.
pub const CUTTER_GRID_V4_MAX_LOGICAL_COMMANDS: usize = 500;
/// The player-visible Move distance bounds.
pub const CUTTER_GRID_V4_MIN_MOVE_DISTANCE: u32 = 1;
/// The player-visible Move distance bounds.
pub const CUTTER_GRID_V4_MAX_MOVE_DISTANCE: u32 = 12;
/// The player-visible Repeat count bounds.
pub const CUTTER_GRID_V4_MIN_REPEAT_COUNT: u32 = 1;
/// The player-visible Repeat count bounds.
pub const CUTTER_GRID_V4_MAX_REPEAT_COUNT: u32 = 20;
/// The player-visible Wait duration ceiling.
pub const CUTTER_GRID_V4_MAX_WAIT_MS: f64 = 5_000.0;

/// Why a V4 source program could not become executable actions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CutterGridCompileErrorV4Code {
    /// The source Program is not the compact V4 Cutter Grid language.
    InvalidEnvelope,
    /// A Move distance is outside the inclusive 1..=12 integer range.
    InvalidDistance,
    /// A Wait is negative, non-finite, or exceeds five seconds.
    InvalidWait,
    /// A Repeat count is outside the inclusive 1..=20 range.
    InvalidRepeat,
    /// A Repeat has no executable body.
    EmptyRepeat,
    /// Repeat expansion exceeded the 500 logical-command cap.
    CommandLimitExceeded,
    /// No visible Move or Wait remains after validation.
    EmptyProgram,
}

impl CutterGridCompileErrorV4Code {
    /// Stable diagnostic spelling for later HTTP error mapping.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidEnvelope => "invalid-envelope",
            Self::InvalidDistance => "invalid-distance",
            Self::InvalidWait => "invalid-wait",
            Self::InvalidRepeat => "invalid-repeat",
            Self::EmptyRepeat => "empty-repeat",
            Self::CommandLimitExceeded => "command-limit-exceeded",
            Self::EmptyProgram => "empty-program",
        }
    }
}

/// Structured source-program failure with Blockly attribution where available.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutterGridCompileErrorV4 {
    /// Stable failure class.
    pub code: CutterGridCompileErrorV4Code,
    /// Source Blockly block that caused the failure, when there is one.
    pub source_block_id: Option<String>,
}

impl CutterGridCompileErrorV4 {
    fn new(code: CutterGridCompileErrorV4Code, source_block_id: Option<&str>) -> Self {
        Self {
            code,
            source_block_id: source_block_id.map(String::from),
        }
    }
}

/// One repeat-expanded, player-visible V4 action.
///
/// Unlike V1/V2 verification actions, `Move N` remains one leaf.  Its logical
/// command cost is still `N`, preserving existing scoring semantics while
/// allowing a compact PTP planner to emit only one or two primitives later.
#[derive(Debug, Clone, PartialEq)]
pub enum CutterGridExecutableActionV4 {
    /// One visible fixed-axis Move.
    Move {
        /// Stable occurrence id: `sourceBlockId#expandedActionIndex`.
        occurrence_id: String,
        /// Blockly block that created this occurrence.
        source_block_id: String,
        /// Fixed world-axis direction.
        direction: CutterGridDirection,
        /// Whole-cell distance, 1 through 12.
        distance: u32,
        /// Logical coordinate before the Move.
        start_coord: CutterGridCoord,
        /// Logical coordinate after the complete Move.
        end_coord: CutterGridCoord,
        /// Logical scoring cost, equal to `distance`.
        logical_command_count: u32,
    },
    /// One visible hold.
    Wait {
        /// Stable occurrence id: `sourceBlockId#expandedActionIndex`.
        occurrence_id: String,
        /// Blockly block that created this occurrence.
        source_block_id: String,
        /// Hold duration in milliseconds.
        duration_ms: f64,
        /// Logical scoring cost, always one.
        logical_command_count: u32,
    },
}

impl CutterGridExecutableActionV4 {
    /// Blockly block that produced this action.
    pub fn source_block_id(&self) -> &str {
        match self {
            Self::Move {
                source_block_id, ..
            }
            | Self::Wait {
                source_block_id, ..
            } => source_block_id,
        }
    }

    /// Stable repeat-expanded action occurrence.
    pub fn occurrence_id(&self) -> &str {
        match self {
            Self::Move { occurrence_id, .. } | Self::Wait { occurrence_id, .. } => occurrence_id,
        }
    }

    /// Logical command cost charged to the player.
    pub const fn logical_command_count(&self) -> u32 {
        match self {
            Self::Move {
                logical_command_count,
                ..
            }
            | Self::Wait {
                logical_command_count,
                ..
            } => *logical_command_count,
        }
    }
}

/// Fully validated compact planner input.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterGridV4CompiledProgram {
    /// Original V4 source tree.
    pub program: CutterGridProgramV4,
    /// Repeat-expanded visible actions.
    pub executable_actions: Vec<CutterGridExecutableActionV4>,
    /// Sum of Move distance and Wait costs, never more than 500.
    pub executed_command_count: u32,
}

/// Compile the player-facing V4 source tree into visible planning actions.
///
/// This is intentionally independent of a Challenge/Profile; physical bounds
/// and endpoint IK are the next planning step.  The compact language keeps V1's
/// serialized node shape, but only accepts the V4 planner stamp.
pub fn compile_cutter_grid_program_v4(
    program: &CutterGridProgramV4,
) -> Result<CutterGridV4CompiledProgram, CutterGridCompileErrorV4> {
    if program.kind != "cutter-grid"
        || program.version != 1
        || program.planner_version != CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION
    {
        return Err(CutterGridCompileErrorV4::new(
            CutterGridCompileErrorV4Code::InvalidEnvelope,
            None,
        ));
    }

    let mut executable_actions = Vec::new();
    let mut coord = [0_i32, 0_i32, 0_i32];
    let mut command_count = 0_usize;
    append_program_nodes(
        &program.nodes,
        &mut executable_actions,
        &mut coord,
        &mut command_count,
    )?;

    if executable_actions.is_empty() {
        return Err(CutterGridCompileErrorV4::new(
            CutterGridCompileErrorV4Code::EmptyProgram,
            None,
        ));
    }

    Ok(CutterGridV4CompiledProgram {
        program: program.clone(),
        executable_actions,
        executed_command_count: command_count as u32,
    })
}

fn append_program_nodes(
    nodes: &[CutterGridNode],
    actions: &mut Vec<CutterGridExecutableActionV4>,
    coord: &mut CutterGridCoord,
    command_count: &mut usize,
) -> Result<(), CutterGridCompileErrorV4> {
    for node in nodes {
        match node {
            CutterGridNode::Repeat {
                count,
                body,
                source_block_id,
            } => {
                if !(CUTTER_GRID_V4_MIN_REPEAT_COUNT..=CUTTER_GRID_V4_MAX_REPEAT_COUNT)
                    .contains(count)
                {
                    return Err(CutterGridCompileErrorV4::new(
                        CutterGridCompileErrorV4Code::InvalidRepeat,
                        Some(source_block_id),
                    ));
                }
                if body.is_empty() {
                    return Err(CutterGridCompileErrorV4::new(
                        CutterGridCompileErrorV4Code::EmptyRepeat,
                        Some(source_block_id),
                    ));
                }
                for _ in 0..*count {
                    append_program_nodes(body, actions, coord, command_count)?;
                }
            }
            CutterGridNode::Move {
                direction,
                distance,
                source_block_id,
            } => {
                if !(CUTTER_GRID_V4_MIN_MOVE_DISTANCE..=CUTTER_GRID_V4_MAX_MOVE_DISTANCE)
                    .contains(distance)
                {
                    return Err(CutterGridCompileErrorV4::new(
                        CutterGridCompileErrorV4Code::InvalidDistance,
                        Some(source_block_id),
                    ));
                }
                add_logical_cost(command_count, *distance as usize, source_block_id)?;
                let start_coord = *coord;
                let end_coord = move_cutter_grid_coord_by_distance(*coord, *direction, *distance);
                let occurrence_id = occurrence_id(source_block_id, actions.len());
                actions.push(CutterGridExecutableActionV4::Move {
                    occurrence_id,
                    source_block_id: source_block_id.clone(),
                    direction: *direction,
                    distance: *distance,
                    start_coord,
                    end_coord,
                    logical_command_count: *distance,
                });
                *coord = end_coord;
            }
            CutterGridNode::Wait {
                duration_ms,
                source_block_id,
            } => {
                if !duration_ms.is_finite()
                    || *duration_ms < 0.0
                    || *duration_ms > CUTTER_GRID_V4_MAX_WAIT_MS
                {
                    return Err(CutterGridCompileErrorV4::new(
                        CutterGridCompileErrorV4Code::InvalidWait,
                        Some(source_block_id),
                    ));
                }
                add_logical_cost(command_count, 1, source_block_id)?;
                let occurrence_id = occurrence_id(source_block_id, actions.len());
                actions.push(CutterGridExecutableActionV4::Wait {
                    occurrence_id,
                    source_block_id: source_block_id.clone(),
                    duration_ms: *duration_ms,
                    logical_command_count: 1,
                });
            }
        }
    }
    Ok(())
}

fn add_logical_cost(
    command_count: &mut usize,
    cost: usize,
    source_block_id: &str,
) -> Result<(), CutterGridCompileErrorV4> {
    *command_count += cost;
    if *command_count > CUTTER_GRID_V4_MAX_LOGICAL_COMMANDS {
        return Err(CutterGridCompileErrorV4::new(
            CutterGridCompileErrorV4Code::CommandLimitExceeded,
            Some(source_block_id),
        ));
    }
    Ok(())
}

fn occurrence_id(source_block_id: &str, action_index: usize) -> String {
    format!("{source_block_id}#{action_index}")
}

fn move_cutter_grid_coord_by_distance(
    mut coord: CutterGridCoord,
    direction: CutterGridDirection,
    distance: u32,
) -> CutterGridCoord {
    let delta = direction.delta();
    for _ in 0..distance {
        for axis in 0..3 {
            coord[axis] += delta[axis];
        }
    }
    coord
}

/// Convert a V4 logical grid coordinate to its fixed world-space endpoint.
pub fn cutter_grid_coord_to_world_v4(
    logical_coord: CutterGridCoord,
    origin_hair_coord: CutterGridCoord,
    voxel_config: &VoxelConfig,
) -> Vec3 {
    let hair_coord = VoxelCoord {
        x: logical_coord[0] + origin_hair_coord[0],
        y: logical_coord[1] + origin_hair_coord[1],
        z: logical_coord[2] + origin_hair_coord[2],
    };
    voxel_coord_to_world(&hair_coord, voxel_config.origin, voxel_config.size)
}

/// A candidate entry pose used as one deterministic IK seed source.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterGridIkEntrySeedV4 {
    /// Stable profile entry id.
    pub id: String,
    /// Servo degrees in the challenge's joint order.
    pub joint_angles: BTreeMap<JointId, f64>,
}

/// One collision-free, unquantized endpoint IK branch.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterGridIkCandidateV4 {
    /// Stable only within a target/layer namespace.
    pub id: String,
    /// Servo degrees in the Challenge's configured joint ids.
    pub joint_angles: BTreeMap<JointId, f64>,
    /// Forward-kinematics end-effector location.
    pub end_effector: Vec3,
    /// Euclidean end-effector error to the requested target.
    pub error: f64,
    /// DLS iterations performed (fixed at 80 for a normal projection).
    pub iterations: u32,
    /// Conservative signed clearance from the head constraint.
    pub minimum_head_clearance: f64,
    /// Smallest normalized margin to a configured joint limit.
    pub minimum_joint_limit_margin: f64,
}

/// Deterministic options for one endpoint candidate enumeration.
#[derive(Debug, Clone)]
pub struct CutterGridIkOptionsV4<'a> {
    /// Endpoint error tolerance. V4 uses `voxelSize / 16`.
    pub max_error: f64,
    /// Candidates from the preceding endpoint layer, used as first seeds.
    pub previous_layer: &'a [CutterGridIkCandidateV4],
    /// Certified origin poses, used after preceding-layer seeds.
    pub entry_options: &'a [CutterGridIkEntrySeedV4],
    /// Fixed Halton budget. V4 uses 12 initially and 48 for one expansion.
    pub seed_budget: usize,
    /// Maximum retained, diversified branch count.
    pub candidate_limit: usize,
    /// Stable namespace for candidate ids.
    pub candidate_namespace: &'a str,
}

/// Enumerate deterministic, collision-free, unquantized endpoint IK branches.
///
/// The seed order is the V4 contract: previous-layer candidates sorted by id,
/// entry candidates sorted by id, Servo initial pose, joint midpoint, then a
/// fixed five-dimensional Halton sequence. Later graph phases choose among the
/// retained branches; this function never commits to a locally convenient one.
pub fn enumerate_cutter_grid_ik_candidates_v4(
    challenge: &ChallengeDefinition,
    target: Vec3,
    options: CutterGridIkOptionsV4<'_>,
) -> Vec<CutterGridIkCandidateV4> {
    if !options.max_error.is_finite() || options.max_error < 0.0 || options.candidate_limit == 0 {
        return Vec::new();
    }

    let seeds = ladder_seeds(challenge, &options);
    let mut candidates = Vec::new();
    for seed in seeds {
        let Some((joint_angles, end_effector, error, iterations)) =
            project_dls_seed(challenge, target, &seed)
        else {
            continue;
        };
        if error > options.max_error {
            continue;
        }
        let Ok(pose) = compute_robot_pose(
            &challenge.robot_config,
            &JointAngles::from_ordered(joint_angles.clone()),
        ) else {
            continue;
        };
        if find_robot_head_collision(
            &pose,
            &challenge.voxel_config,
            &challenge.robot_config.geometry,
        )
        .is_some()
        {
            continue;
        }
        let minimum_joint_limit_margin = minimum_normalized_joint_limit_margin_v4(
            &joint_angles,
            &challenge.robot_config.joints,
        );
        candidates.push(CutterGridIkCandidateV4 {
            id: stable_candidate_id(
                options.candidate_namespace,
                &joint_angles,
                &challenge.robot_config.joints,
            ),
            joint_angles,
            end_effector,
            error,
            iterations,
            minimum_head_clearance: measure_robot_head_clearance(
                &pose,
                &challenge.voxel_config,
                &challenge.robot_config.geometry,
            ),
            minimum_joint_limit_margin,
        });
    }

    let unique = deduplicate_candidates(candidates, &challenge.robot_config.joints);
    let anchors = match options.seed_budget {
        CUTTER_GRID_V4_EXPANDED_SEED_BUDGET => enumerate_cutter_grid_ik_candidates_v4(
            challenge,
            target,
            CutterGridIkOptionsV4 {
                seed_budget: CUTTER_GRID_V4_INITIAL_SEED_BUDGET,
                ..options.clone()
            },
        ),
        _ => Vec::new(),
    };
    diversify_candidates(
        unique,
        &challenge.robot_config.joints,
        options.candidate_limit,
        &anchors,
    )
}

/// Root-mean-square normalized movement between two ordered joint states.
pub fn normalized_joint_distance_v4(
    angles: &BTreeMap<JointId, f64>,
    reference: &BTreeMap<JointId, f64>,
    joints: &[JointConfig],
) -> f64 {
    let sum = joints.iter().fold(0.0_f64, |sum, joint| {
        let span = joint.max_angle_deg - joint.min_angle_deg;
        let angle = angles.get(&joint.id).copied().unwrap_or(f64::INFINITY);
        let previous = reference
            .get(&joint.id)
            .copied()
            .unwrap_or(f64::NEG_INFINITY);
        sum + ((angle - previous) / span).powi(2)
    });
    libm::sqrt(sum)
}

/// The smallest normalized margin from any configured joint range.
pub fn minimum_normalized_joint_limit_margin_v4(
    angles: &BTreeMap<JointId, f64>,
    joints: &[JointConfig],
) -> f64 {
    joints.iter().fold(f64::INFINITY, |margin, joint| {
        let span = joint.max_angle_deg - joint.min_angle_deg;
        let angle = angles.get(&joint.id).copied().unwrap_or(f64::NAN);
        margin.min(((angle - joint.min_angle_deg) / span).min((joint.max_angle_deg - angle) / span))
    })
}

fn project_dls_seed(
    challenge: &ChallengeDefinition,
    target: Vec3,
    seed: &BTreeMap<JointId, f64>,
) -> Option<(BTreeMap<JointId, f64>, Vec3, f64, u32)> {
    let joints = &challenge.robot_config.joints;
    let mut angles = ordered_clamped_angles(seed, joints);
    for _ in 0..CUTTER_GRID_V4_IK_MAX_ITERATIONS {
        let pose = compute_robot_pose(
            &challenge.robot_config,
            &JointAngles::from_ordered(angles.clone()),
        )
        .ok()?;
        let error = subtract(target, pose.end_effector);
        let jacobian = numerical_jacobian(challenge, &angles, pose.end_effector)?;
        let update = damped_least_squares(jacobian, error);
        for (index, joint) in joints.iter().enumerate() {
            let current = angles.get(&joint.id).copied()?;
            let delta_degrees = clamp(
                radians_to_degrees(update[index]),
                -CUTTER_GRID_V4_IK_MAX_UPDATE_DEG,
                CUTTER_GRID_V4_IK_MAX_UPDATE_DEG,
            );
            angles.insert(
                joint.id.clone(),
                clamp(
                    current + delta_degrees,
                    joint.min_angle_deg,
                    joint.max_angle_deg,
                ),
            );
        }
    }
    let pose = compute_robot_pose(
        &challenge.robot_config,
        &JointAngles::from_ordered(angles.clone()),
    )
    .ok()?;
    let error = distance(pose.end_effector, target);
    Some((
        angles,
        pose.end_effector,
        error,
        CUTTER_GRID_V4_IK_MAX_ITERATIONS as u32,
    ))
}

fn numerical_jacobian(
    challenge: &ChallengeDefinition,
    angles: &BTreeMap<JointId, f64>,
    current: Vec3,
) -> Option<[[f64; 5]; 3]> {
    let joints = &challenge.robot_config.joints;
    if joints.len() != 5 {
        return None;
    }
    let mut jacobian = [[0.0_f64; 5]; 3];
    for (joint_index, joint) in joints.iter().enumerate() {
        let angle = angles.get(&joint.id).copied()?;
        let direction = if angle + CUTTER_GRID_V4_IK_JACOBIAN_STEP_DEG <= joint.max_angle_deg {
            1.0
        } else {
            -1.0
        };
        let mut sample_angles = angles.clone();
        sample_angles.insert(
            joint.id.clone(),
            angle + CUTTER_GRID_V4_IK_JACOBIAN_STEP_DEG * direction,
        );
        let sample = compute_robot_pose(
            &challenge.robot_config,
            &JointAngles::from_ordered(sample_angles),
        )
        .ok()?
        .end_effector;
        let denominator = degrees_to_radians(CUTTER_GRID_V4_IK_JACOBIAN_STEP_DEG * direction);
        for axis in 0..3 {
            jacobian[axis][joint_index] = (sample[axis] - current[axis]) / denominator;
        }
    }
    Some(jacobian)
}

/// `Jᵀ (J Jᵀ + λ² I)⁻¹ error` for the fixed 3×5 endpoint Jacobian.
fn damped_least_squares(jacobian: [[f64; 5]; 3], error: Vec3) -> [f64; 5] {
    let mut normal = [[0.0_f64; 3]; 3];
    for row in 0..3 {
        for column in 0..3 {
            let product = (0..5).fold(0.0_f64, |sum, joint| {
                sum + jacobian[row][joint] * jacobian[column][joint]
            });
            normal[row][column] = product
                + if row == column {
                    CUTTER_GRID_V4_IK_DAMPING.powi(2)
                } else {
                    0.0
                };
        }
    }
    let Some(inverse) = invert_3(normal) else {
        return [0.0; 5];
    };
    let projected = [
        dot(inverse[0], error),
        dot(inverse[1], error),
        dot(inverse[2], error),
    ];
    let mut result = [0.0_f64; 5];
    for joint in 0..5 {
        result[joint] = (0..3).fold(0.0_f64, |sum, axis| {
            sum + jacobian[axis][joint] * projected[axis]
        });
    }
    result
}

fn invert_3(matrix: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let [[a, b, c], [d, e, f], [g, h, i]] = matrix;
    let determinant = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if determinant.abs() < 1e-12 {
        return None;
    }
    Some(
        [
            [e * i - f * h, c * h - b * i, b * f - c * e],
            [f * g - d * i, a * i - c * g, c * d - a * f],
            [d * h - e * g, b * g - a * h, a * e - b * d],
        ]
        .map(|row| row.map(|value| value / determinant)),
    )
}

fn ladder_seeds(
    challenge: &ChallengeDefinition,
    options: &CutterGridIkOptionsV4<'_>,
) -> Vec<BTreeMap<JointId, f64>> {
    let joints = &challenge.robot_config.joints;
    let mut previous = options.previous_layer.to_vec();
    previous.sort_by(|left, right| left.id.cmp(&right.id));
    let mut entries = options.entry_options.to_vec();
    entries.sort_by(|left, right| left.id.cmp(&right.id));

    let mut seeds = previous
        .into_iter()
        .map(|candidate| ordered_clamped_angles(&candidate.joint_angles, joints))
        .collect::<Vec<_>>();
    seeds.extend(
        entries
            .into_iter()
            .map(|entry| ordered_clamped_angles(&entry.joint_angles, joints)),
    );
    seeds.push(initial_angles(joints));
    seeds.push(midpoint_angles(joints));

    const PRIMES: [u32; 5] = [2, 3, 5, 7, 11];
    for index in 1..=options.seed_budget {
        let mut seed = BTreeMap::new();
        for (joint_index, joint) in joints.iter().enumerate() {
            let fraction = radical_inverse(index as u32, *PRIMES.get(joint_index).unwrap_or(&13));
            seed.insert(
                joint.id.clone(),
                joint.min_angle_deg + fraction * (joint.max_angle_deg - joint.min_angle_deg),
            );
        }
        seeds.push(seed);
    }
    deduplicate_seeds(seeds, joints)
}

fn deduplicate_candidates(
    candidates: Vec<CutterGridIkCandidateV4>,
    joints: &[JointConfig],
) -> Vec<CutterGridIkCandidateV4> {
    let mut result: Vec<CutterGridIkCandidateV4> = Vec::new();
    for candidate in candidates {
        if result.iter().any(|known| {
            normalized_joint_distance_v4(&known.joint_angles, &candidate.joint_angles, joints)
                <= 0.01
        }) {
            continue;
        }
        result.push(candidate);
    }
    result
}

fn diversify_candidates(
    mut candidates: Vec<CutterGridIkCandidateV4>,
    joints: &[JointConfig],
    limit: usize,
    anchors: &[CutterGridIkCandidateV4],
) -> Vec<CutterGridIkCandidateV4> {
    candidates.sort_by(|left, right| compare_candidate_static(left, right, joints));
    if candidates.len() <= limit {
        return candidates;
    }

    let mut selected = Vec::new();
    for anchor in anchors {
        if selected.len() == limit {
            break;
        }
        if let Some(candidate) = candidates
            .iter()
            .find(|candidate| candidate.id == anchor.id)
        {
            selected.push(candidate.clone());
        }
    }
    selected.dedup_by(|left, right| left.id == right.id);
    let mut remaining = candidates
        .into_iter()
        .filter(|candidate| !selected.iter().any(|known| known.id == candidate.id))
        .collect::<Vec<_>>();
    if selected.is_empty() && !remaining.is_empty() {
        selected.push(remaining.remove(0));
    }
    while selected.len() < limit && !remaining.is_empty() {
        let mut best_index = 0_usize;
        let mut best_distance = f64::NEG_INFINITY;
        for (index, candidate) in remaining.iter().enumerate() {
            let distance = selected.iter().fold(f64::INFINITY, |nearest, chosen| {
                nearest.min(normalized_joint_distance_v4(
                    &candidate.joint_angles,
                    &chosen.joint_angles,
                    joints,
                ))
            });
            if distance > best_distance + 1e-12 {
                best_distance = distance;
                best_index = index;
            }
        }
        selected.push(remaining.remove(best_index));
    }
    selected
}

fn compare_candidate_static(
    left: &CutterGridIkCandidateV4,
    right: &CutterGridIkCandidateV4,
    joints: &[JointConfig],
) -> Ordering {
    compare_number(left.error, right.error)
        .then_with(|| compare_number(right.minimum_head_clearance, left.minimum_head_clearance))
        .then_with(|| {
            compare_number(
                right.minimum_joint_limit_margin,
                left.minimum_joint_limit_margin,
            )
        })
        .then_with(|| {
            compare_number(
                midpoint_distance(&left.joint_angles, joints),
                midpoint_distance(&right.joint_angles, joints),
            )
        })
        .then_with(|| compare_angles(&left.joint_angles, &right.joint_angles, joints))
}

fn initial_angles(joints: &[JointConfig]) -> BTreeMap<JointId, f64> {
    joints
        .iter()
        .map(|joint| (joint.id.clone(), joint.initial_angle_deg))
        .collect()
}

fn midpoint_angles(joints: &[JointConfig]) -> BTreeMap<JointId, f64> {
    joints
        .iter()
        .map(|joint| {
            (
                joint.id.clone(),
                (joint.min_angle_deg + joint.max_angle_deg) / 2.0,
            )
        })
        .collect()
}

fn ordered_clamped_angles(
    angles: &BTreeMap<JointId, f64>,
    joints: &[JointConfig],
) -> BTreeMap<JointId, f64> {
    joints
        .iter()
        .map(|joint| {
            (
                joint.id.clone(),
                clamp(
                    angles
                        .get(&joint.id)
                        .copied()
                        .unwrap_or(joint.initial_angle_deg),
                    joint.min_angle_deg,
                    joint.max_angle_deg,
                ),
            )
        })
        .collect()
}

fn deduplicate_seeds(
    seeds: Vec<BTreeMap<JointId, f64>>,
    joints: &[JointConfig],
) -> Vec<BTreeMap<JointId, f64>> {
    let mut result = Vec::new();
    let mut seen = Vec::<String>::new();
    for seed in seeds {
        let key = joints
            .iter()
            .map(|joint| format!("{:.6}", seed.get(&joint.id).copied().unwrap_or(f64::NAN)))
            .collect::<Vec<_>>()
            .join(",");
        if seen.iter().any(|known| known == &key) {
            continue;
        }
        seen.push(key);
        result.push(seed);
    }
    result
}

fn stable_candidate_id(
    namespace: &str,
    angles: &BTreeMap<JointId, f64>,
    joints: &[JointConfig],
) -> String {
    let joined = joints
        .iter()
        .map(|joint| format!("{:.9}", angles.get(&joint.id).copied().unwrap_or(f64::NAN)))
        .collect::<Vec<_>>()
        .join(",");
    format!("{namespace}:{joined}")
}

fn midpoint_distance(angles: &BTreeMap<JointId, f64>, joints: &[JointConfig]) -> f64 {
    let sum = joints.iter().fold(0.0_f64, |sum, joint| {
        let span = joint.max_angle_deg - joint.min_angle_deg;
        let midpoint = (joint.min_angle_deg + joint.max_angle_deg) / 2.0;
        let angle = angles.get(&joint.id).copied().unwrap_or(f64::NAN);
        sum + ((angle - midpoint) / span).powi(2)
    });
    libm::sqrt(sum)
}

fn compare_angles(
    left: &BTreeMap<JointId, f64>,
    right: &BTreeMap<JointId, f64>,
    joints: &[JointConfig],
) -> Ordering {
    for joint in joints {
        // TypeScript's `compareAngles` is deliberately exact here. These are
        // the final deterministic tie-breakers after tolerant score fields;
        // applying the score tolerance a second time can select a different
        // entry branch across the frontend/Rust boundary.
        let ordering = left
            .get(&joint.id)
            .copied()
            .unwrap_or(f64::NAN)
            .total_cmp(&right.get(&joint.id).copied().unwrap_or(f64::NAN));
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn radical_inverse(mut value: u32, base: u32) -> f64 {
    let mut result = 0.0_f64;
    let mut fraction = 1.0 / f64::from(base);
    while value > 0 {
        result += f64::from(value % base) * fraction;
        value /= base;
        fraction /= f64::from(base);
    }
    result
}

fn compare_number(left: f64, right: f64) -> Ordering {
    if (left - right).abs() < 1e-12 {
        Ordering::Equal
    } else {
        left.total_cmp(&right)
    }
}

fn subtract(left: Vec3, right: Vec3) -> Vec3 {
    [left[0] - right[0], left[1] - right[1], left[2] - right[2]]
}

fn dot(left: [f64; 3], right: Vec3) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}

fn distance(left: Vec3, right: Vec3) -> f64 {
    libm::sqrt(
        (left[0] - right[0]).powi(2) + (left[1] - right[1]).powi(2) + (left[2] - right[2]).powi(2),
    )
}

fn clamp(value: f64, min: f64, max: f64) -> f64 {
    value.max(min).min(max)
}

fn degrees_to_radians(value: f64) -> f64 {
    value * core::f64::consts::PI / 180.0
}

fn radians_to_degrees(value: f64) -> f64 {
    value * 180.0 / core::f64::consts::PI
}

// ---------------------------------------------------------------------------
// Phase 3: compact PTP geometry and global endpoint graph
// ---------------------------------------------------------------------------

/// V4's minimum duration for one synchronized PTP primitive.
pub const CUTTER_GRID_V4_MIN_PTP_DURATION_MS: f64 = 160.0;
/// Maximum joint change between geometric PTP certificate samples.
pub const CUTTER_GRID_V4_PTP_MAX_JOINT_SAMPLE_DELTA_DEG: f64 = 0.5;
/// Fixed upper bound on one compact PTP retiming loop.
pub const CUTTER_GRID_V4_MAX_RETIMING_ATTEMPTS: usize = 48;
/// Adaptive certificate starts with at least eight intervals.
pub const CUTTER_GRID_V4_ADAPTIVE_MIN_SUBDIVISION_DEPTH: usize = 3;
/// Adaptive certificate never expands an interval beyond this depth.
pub const CUTTER_GRID_V4_ADAPTIVE_MAX_SUBDIVISION_DEPTH: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PtpCertificateFailureV4 {
    JointLimit,
    HeadCollision,
    SamplingLimit,
}

/// A sampled synchronized PTP state used only for geometry certification.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterGridPtpSampleV4 {
    /// Milliseconds from the primitive's beginning.
    pub time_ms: f64,
    /// Servo joint positions.
    pub joint_angles: BTreeMap<JointId, f64>,
    /// Servo joint velocities.
    pub joint_velocities_deg_per_sec: BTreeMap<JointId, f64>,
    /// Servo joint accelerations.
    pub joint_accelerations_deg_per_sec2: BTreeMap<JointId, f64>,
    /// Servo joint jerks.
    pub joint_jerks_deg_per_sec3: BTreeMap<JointId, f64>,
    /// Forward-kinematics tool position.
    pub end_effector: Vec3,
}

/// Proven safety summary for one compact PTP primitive.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterGridPtpCertificateV4 {
    /// Minimum signed head clearance over checked samples.
    pub minimum_head_clearance: f64,
    /// Minimum normalized joint-limit margin over checked samples.
    pub minimum_joint_limit_margin: f64,
    /// Largest normalized step between neighboring samples.
    pub maximum_normalized_joint_step: f64,
    /// Number of checked samples, including both endpoints.
    pub sample_count: u32,
}

/// Exact dynamic-limit ratios for a compact PTP primitive.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterGridPtpDynamicsV4 {
    /// Largest analytic speed / hard-speed-limit ratio.
    pub maximum_velocity_ratio: f64,
    /// Largest analytic acceleration / hard-acceleration-limit ratio.
    pub maximum_acceleration_ratio: f64,
    /// Largest analytic jerk / hard-jerk-limit ratio.
    pub maximum_jerk_ratio: f64,
    /// All hard limits are satisfied.
    pub valid: bool,
}

/// Adaptive collision proof. Dense samples remain planner-local evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterGridPtpAdaptiveCertificateV4 {
    /// Ordered PTP samples used by the actual tool sweep.
    pub samples: Vec<CutterGridPtpSampleV4>,
    /// Minimum signed head clearance.
    pub minimum_head_clearance: f64,
    /// Minimum normalized joint-limit margin.
    pub minimum_joint_limit_margin: f64,
    /// Largest normalized joint step between retained samples.
    pub maximum_normalized_joint_step: f64,
}

#[derive(Debug, Clone)]
struct RetimedPtpV4 {
    primitive: hcr_contract::CutterGridSyncPtpPrimitiveV4,
    certificate: CutterGridPtpAdaptiveCertificateV4,
    dynamics: CutterGridPtpDynamicsV4,
    maximum_end_effector_chord_deviation: f64,
}

/// Planner failure with enough context for the service to create a wire error.
#[derive(Debug, Clone, PartialEq)]
pub struct CutterGridPlanningFailureV4 {
    /// Stable V4 reason.
    pub code: hcr_contract::CutterGridPlanningErrorCodeV4,
    /// Pipeline stage that generated the reason.
    pub stage: hcr_contract::CutterGridPlanningStageV4,
    /// Blockly source block, when known.
    pub source_block_id: Option<String>,
    /// Visible action index, when known.
    pub action_index: Option<u32>,
    /// Action whose candidate budget was expanded, when any.
    pub expanded_action_index: Option<u32>,
    /// Logical endpoint involved in the failure, when any.
    pub target_coord: Option<CutterGridCoord>,
}

impl CutterGridPlanningFailureV4 {
    fn for_action(
        code: hcr_contract::CutterGridPlanningErrorCodeV4,
        stage: hcr_contract::CutterGridPlanningStageV4,
        action_index: usize,
        action: &CutterGridExecutableActionV4,
        target_coord: Option<CutterGridCoord>,
    ) -> Self {
        Self {
            code,
            stage,
            source_block_id: Some(action.source_block_id().into()),
            action_index: Some(action_index as u32),
            expanded_action_index: None,
            target_coord,
        }
    }
}

/// One action after global endpoint-branch selection but before dynamics/sweep.
#[derive(Debug, Clone)]
pub enum CutterGridGeometryActionV4 {
    /// A visible Move realized by one direct PTP or a one-via two-PTP route.
    Move {
        /// Original compiled action.
        action: CutterGridExecutableActionV4,
        /// Exactly one or two compact primitives.
        primitives: Vec<hcr_contract::CutterGridSyncPtpPrimitiveV4>,
    },
    /// A visible hold.
    Wait {
        /// Original compiled action.
        action: CutterGridExecutableActionV4,
    },
}

/// Frozen Phase 3 geometry selected over all Move endpoints.
#[derive(Debug, Clone)]
pub struct CutterGridV4GeometryPlan {
    /// Server-owned Profile entry selected for the whole program.
    pub entry_option_id: String,
    /// System-only positioning primitive selected with the entry.
    pub positioning_primitive: hcr_contract::CutterGridSyncPtpPrimitiveV4,
    /// Logical player origin.
    pub start_coord: CutterGridCoord,
    /// Final logical player coordinate.
    pub end_coord: CutterGridCoord,
    /// Visible Move/Wait actions in program order.
    pub actions: Vec<CutterGridGeometryActionV4>,
    /// Compact deterministic graph diagnostics; Phase 4 fills dynamics fields.
    pub diagnostics: hcr_contract::CutterGridPlanningDiagnosticsV4,
}

/// Create a zero-boundary-derivative synchronized quintic PTP primitive.
pub fn create_cutter_grid_sync_ptp_primitive_v4(
    challenge: &ChallengeDefinition,
    start_angles: &BTreeMap<JointId, f64>,
    end_angles: &BTreeMap<JointId, f64>,
) -> hcr_contract::CutterGridSyncPtpPrimitiveV4 {
    let zero = challenge
        .robot_config
        .joints
        .iter()
        .map(|joint| (joint.id.clone(), 0.0))
        .collect::<BTreeMap<_, _>>();
    hcr_contract::CutterGridSyncPtpPrimitiveV4 {
        kind: "sync-ptp".into(),
        interpolation: "synchronized-quintic".into(),
        duration_ms: CUTTER_GRID_V4_MIN_PTP_DURATION_MS,
        start: hcr_contract::CutterTrajectoryBoundaryStateV4 {
            joint_angles: ordered_clamped_angles(start_angles, &challenge.robot_config.joints),
            joint_velocities_deg_per_sec: zero.clone(),
            joint_accelerations_deg_per_sec2: zero.clone(),
        },
        end: hcr_contract::CutterTrajectoryBoundaryStateV4 {
            joint_angles: ordered_clamped_angles(end_angles, &challenge.robot_config.joints),
            joint_velocities_deg_per_sec: zero.clone(),
            joint_accelerations_deg_per_sec2: zero,
        },
    }
}

/// Create a synchronized quintic primitive with explicit boundary derivatives.
///
/// This is used only for a certified two-primitive detour.  It refuses an
/// incomplete or non-finite boundary rather than clamping it into a different
/// physical command.
pub fn create_cutter_grid_sync_ptp_with_boundary_states_v4(
    challenge: &ChallengeDefinition,
    start: &hcr_contract::CutterTrajectoryBoundaryStateV4,
    end: &hcr_contract::CutterTrajectoryBoundaryStateV4,
    duration_ms: f64,
) -> Option<hcr_contract::CutterGridSyncPtpPrimitiveV4> {
    if !duration_ms.is_finite() || duration_ms < CUTTER_GRID_V4_MIN_PTP_DURATION_MS {
        return None;
    }
    let start = ordered_boundary_state(challenge, start)?;
    let end = ordered_boundary_state(challenge, end)?;
    Some(hcr_contract::CutterGridSyncPtpPrimitiveV4 {
        kind: "sync-ptp".into(),
        interpolation: "synchronized-quintic".into(),
        duration_ms,
        start,
        end,
    })
}

/// Evaluate a compact quintic PTP primitive at a clamped absolute time.
pub fn evaluate_cutter_grid_sync_ptp_v4(
    challenge: &ChallengeDefinition,
    primitive: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
    time_ms: f64,
) -> Option<CutterGridPtpSampleV4> {
    if primitive.kind != "sync-ptp"
        || primitive.interpolation != "synchronized-quintic"
        || !primitive.duration_ms.is_finite()
        || primitive.duration_ms < CUTTER_GRID_V4_MIN_PTP_DURATION_MS
    {
        return None;
    }
    if time_ms <= 0.0 {
        return evaluate_ptp_boundary_v4(challenge, &primitive.start, 0.0);
    }
    if time_ms >= primitive.duration_ms {
        return evaluate_ptp_boundary_v4(challenge, &primitive.end, primitive.duration_ms);
    }
    let seconds = primitive.duration_ms / 1_000.0;
    let time_seconds = clamp(time_ms, 0.0, primitive.duration_ms) / 1_000.0;
    let mut joint_angles = BTreeMap::new();
    let mut velocities = BTreeMap::new();
    let mut accelerations = BTreeMap::new();
    let mut jerks = BTreeMap::new();
    for joint in &challenge.robot_config.joints {
        let start = primitive.start.joint_angles.get(&joint.id).copied()?;
        let end = primitive.end.joint_angles.get(&joint.id).copied()?;
        let start_velocity = primitive
            .start
            .joint_velocities_deg_per_sec
            .get(&joint.id)
            .copied()?;
        let start_acceleration = primitive
            .start
            .joint_accelerations_deg_per_sec2
            .get(&joint.id)
            .copied()?;
        let end_velocity = primitive
            .end
            .joint_velocities_deg_per_sec
            .get(&joint.id)
            .copied()?;
        let end_acceleration = primitive
            .end
            .joint_accelerations_deg_per_sec2
            .get(&joint.id)
            .copied()?;
        let curve = quintic_boundary_curve_v4(
            start,
            start_velocity,
            start_acceleration,
            end,
            end_velocity,
            end_acceleration,
            seconds,
            time_seconds,
        );
        joint_angles.insert(joint.id.clone(), curve.position);
        velocities.insert(joint.id.clone(), curve.velocity);
        accelerations.insert(joint.id.clone(), curve.acceleration);
        jerks.insert(joint.id.clone(), curve.jerk);
    }
    let pose = compute_robot_pose(
        &challenge.robot_config,
        &JointAngles::from_ordered(joint_angles.clone()),
    )
    .ok()?;
    Some(CutterGridPtpSampleV4 {
        time_ms: clamp(time_ms, 0.0, primitive.duration_ms),
        joint_angles,
        joint_velocities_deg_per_sec: velocities,
        joint_accelerations_deg_per_sec2: accelerations,
        joint_jerks_deg_per_sec3: jerks,
        end_effector: pose.end_effector,
    })
}

/// Certify joint limits and head clearance for the compact PTP geometry.
pub fn certify_cutter_grid_sync_ptp_geometry_v4(
    challenge: &ChallengeDefinition,
    primitive: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
) -> Option<CutterGridPtpCertificateV4> {
    let max_delta = challenge
        .robot_config
        .joints
        .iter()
        .try_fold(0.0_f64, |max, joint| {
            let start = primitive.start.joint_angles.get(&joint.id).copied()?;
            let end = primitive.end.joint_angles.get(&joint.id).copied()?;
            Some(max.max((end - start).abs()))
        })?;
    let sample_count =
        (libm::ceil(max_delta / CUTTER_GRID_V4_PTP_MAX_JOINT_SAMPLE_DELTA_DEG) as u32).max(1);
    let mut previous: Option<CutterGridPtpSampleV4> = None;
    let mut minimum_head_clearance = f64::INFINITY;
    let mut minimum_joint_limit_margin = f64::INFINITY;
    let mut maximum_normalized_joint_step = 0.0_f64;
    for index in 0..=sample_count {
        let sample = evaluate_cutter_grid_sync_ptp_v4(
            challenge,
            primitive,
            primitive.duration_ms * f64::from(index) / f64::from(sample_count),
        )?;
        if !within_joint_limits(&sample.joint_angles, &challenge.robot_config.joints) {
            return None;
        }
        let pose = compute_robot_pose(
            &challenge.robot_config,
            &JointAngles::from_ordered(sample.joint_angles.clone()),
        )
        .ok()?;
        if find_robot_head_collision(
            &pose,
            &challenge.voxel_config,
            &challenge.robot_config.geometry,
        )
        .is_some()
        {
            return None;
        }
        minimum_head_clearance = minimum_head_clearance.min(measure_robot_head_clearance(
            &pose,
            &challenge.voxel_config,
            &challenge.robot_config.geometry,
        ));
        minimum_joint_limit_margin =
            minimum_joint_limit_margin.min(minimum_normalized_joint_limit_margin_v4(
                &sample.joint_angles,
                &challenge.robot_config.joints,
            ));
        if let Some(previous) = &previous {
            maximum_normalized_joint_step =
                maximum_normalized_joint_step.max(normalized_joint_distance_v4(
                    &previous.joint_angles,
                    &sample.joint_angles,
                    &challenge.robot_config.joints,
                ));
        }
        previous = Some(sample);
    }
    Some(CutterGridPtpCertificateV4 {
        minimum_head_clearance,
        minimum_joint_limit_margin,
        maximum_normalized_joint_step,
        sample_count: sample_count + 1,
    })
}

/// Measure conservative exact extrema of the serialized quintic's q/v/a/j.
pub fn measure_cutter_grid_sync_ptp_dynamics_v4(
    challenge: &ChallengeDefinition,
    primitive: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
    limits: &hcr_contract::CutterGridMotionLimitsV4,
) -> Option<CutterGridPtpDynamicsV4> {
    let duration_seconds = primitive.duration_ms / 1_000.0;
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return None;
    }
    let mut maximum_velocity_ratio = 0.0_f64;
    let mut maximum_acceleration_ratio = 0.0_f64;
    let mut maximum_jerk_ratio = 0.0_f64;
    for joint in &challenge.robot_config.joints {
        let joint_limits = limits.joints.get(&joint.id)?;
        if joint_limits.max_velocity_deg_per_sec <= 0.0
            || joint_limits.max_acceleration_deg_per_sec2 <= 0.0
            || joint_limits.max_jerk_deg_per_sec3 <= 0.0
        {
            return None;
        }
        let coefficients = quintic_coefficients_v4(
            primitive.start.joint_angles.get(&joint.id).copied()?,
            primitive
                .start
                .joint_velocities_deg_per_sec
                .get(&joint.id)
                .copied()?,
            primitive
                .start
                .joint_accelerations_deg_per_sec2
                .get(&joint.id)
                .copied()?,
            primitive.end.joint_angles.get(&joint.id).copied()?,
            primitive
                .end
                .joint_velocities_deg_per_sec
                .get(&joint.id)
                .copied()?,
            primitive
                .end
                .joint_accelerations_deg_per_sec2
                .get(&joint.id)
                .copied()?,
            duration_seconds,
        )?;
        let bounds = exact_quintic_dynamic_bounds_v4(coefficients, duration_seconds);
        maximum_velocity_ratio = maximum_velocity_ratio
            .max(bounds.maximum_velocity / joint_limits.max_velocity_deg_per_sec);
        maximum_acceleration_ratio = maximum_acceleration_ratio
            .max(bounds.maximum_acceleration / joint_limits.max_acceleration_deg_per_sec2);
        maximum_jerk_ratio =
            maximum_jerk_ratio.max(bounds.maximum_jerk / joint_limits.max_jerk_deg_per_sec3);
    }
    Some(CutterGridPtpDynamicsV4 {
        maximum_velocity_ratio,
        maximum_acceleration_ratio,
        maximum_jerk_ratio,
        valid: maximum_velocity_ratio <= 1.0 + 1e-12
            && maximum_acceleration_ratio <= 1.0 + 1e-12
            && maximum_jerk_ratio <= 1.0 + 1e-12,
    })
}

/// Prove a compact PTP safe using deterministic adaptive subdivision.
pub fn certify_cutter_grid_sync_ptp_adaptive_v4(
    challenge: &ChallengeDefinition,
    primitive: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
) -> Result<CutterGridPtpAdaptiveCertificateV4, hcr_contract::CutterGridPlanningErrorCodeV4> {
    let start = evaluate_cutter_grid_sync_ptp_v4(challenge, primitive, 0.0)
        .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
    let end = evaluate_cutter_grid_sync_ptp_v4(challenge, primitive, primitive.duration_ms)
        .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
    let mut samples = Vec::new();
    adaptive_ptp_visit_v4(challenge, primitive, start, end, 0, &mut samples).map_err(|reason| {
        match reason {
            PtpCertificateFailureV4::JointLimit | PtpCertificateFailureV4::HeadCollision => {
                hcr_contract::CutterGridPlanningErrorCodeV4::PtpCollision
            }
            PtpCertificateFailureV4::SamplingLimit => {
                hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed
            }
        }
    })?;
    samples.sort_by(|left, right| compare_number(left.time_ms, right.time_ms));
    samples.dedup_by(|left, right| (left.time_ms - right.time_ms).abs() <= 1e-9);
    if samples.len() < 2 {
        return Err(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed);
    }
    let mut minimum_head_clearance = f64::INFINITY;
    let mut minimum_joint_limit_margin = f64::INFINITY;
    let mut maximum_normalized_joint_step = 0.0_f64;
    for (index, sample) in samples.iter().enumerate() {
        let pose = compute_robot_pose(
            &challenge.robot_config,
            &JointAngles::from_ordered(sample.joint_angles.clone()),
        )
        .map_err(|_| hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
        minimum_head_clearance = minimum_head_clearance.min(measure_robot_head_clearance(
            &pose,
            &challenge.voxel_config,
            &challenge.robot_config.geometry,
        ));
        minimum_joint_limit_margin =
            minimum_joint_limit_margin.min(minimum_normalized_joint_limit_margin_v4(
                &sample.joint_angles,
                &challenge.robot_config.joints,
            ));
        if index > 0 {
            maximum_normalized_joint_step =
                maximum_normalized_joint_step.max(normalized_joint_distance_v4(
                    &samples[index - 1].joint_angles,
                    &sample.joint_angles,
                    &challenge.robot_config.joints,
                ));
        }
    }
    Ok(CutterGridPtpAdaptiveCertificateV4 {
        samples,
        minimum_head_clearance,
        minimum_joint_limit_margin,
        maximum_normalized_joint_step,
    })
}

/// Select a full-program compact PTP geometry plan from a server-owned profile.
pub fn plan_cutter_grid_v4_geometry(
    challenge: &ChallengeDefinition,
    compiled: &CutterGridV4CompiledProgram,
    profile: &hcr_contract::CutterGridProfileV4,
) -> Result<CutterGridV4GeometryPlan, CutterGridPlanningFailureV4> {
    validate_profile(challenge, compiled, profile)?;
    let layers = build_endpoint_layers(challenge, compiled, profile)?;
    if layers.is_empty() {
        return Ok(build_wait_only_geometry_plan(challenge, compiled, profile));
    }
    let entry_seeds = profile
        .entry_options
        .iter()
        .map(|entry| CutterGridIkEntrySeedV4 {
            id: entry.id.clone(),
            joint_angles: entry.joint_angles.clone(),
        })
        .collect::<Vec<_>>();
    let mut candidates = generate_endpoint_candidates(
        challenge,
        &layers,
        &entry_seeds,
        CUTTER_GRID_V4_INITIAL_SEED_BUDGET,
        12,
    );
    let mut expanded_action_index = None;
    if let Some(missing) = candidates.iter().position(Vec::is_empty) {
        let layer = &layers[missing];
        expanded_action_index = Some(layer.action_index as u32);
        let previous = missing
            .checked_sub(1)
            .and_then(|index| candidates.get(index))
            .cloned()
            .unwrap_or_default();
        candidates[missing] = candidates_for_layer(
            challenge,
            layer,
            &previous,
            &entry_seeds,
            CUTTER_GRID_V4_EXPANDED_SEED_BUDGET,
            24,
        );
        if candidates[missing].is_empty() {
            return Err(no_endpoint_candidate(layer, expanded_action_index));
        }
    }

    let mut first_disconnected = 0_usize;
    for attempt in 0..2 {
        for neighbor_limit in [4_usize, 8, usize::MAX] {
            let search =
                select_global_path(challenge, profile, &layers, &candidates, neighbor_limit);
            if let Some(path) = search.path {
                return Ok(build_geometry_plan(
                    compiled,
                    profile,
                    &layers,
                    &candidates,
                    path,
                    expanded_action_index,
                ));
            }
            if neighbor_limit == usize::MAX {
                first_disconnected = search.first_disconnected_layer.unwrap_or(0);
            }
        }
        if attempt == 1 || expanded_action_index.is_some() {
            break;
        }
        let disconnected = first_disconnected;
        let layer = &layers[disconnected];
        expanded_action_index = Some(layer.action_index as u32);
        let previous = disconnected
            .checked_sub(1)
            .and_then(|index| candidates.get(index))
            .cloned()
            .unwrap_or_default();
        candidates[disconnected] = candidates_for_layer(
            challenge,
            layer,
            &previous,
            &entry_seeds,
            CUTTER_GRID_V4_EXPANDED_SEED_BUDGET,
            24,
        );
        if candidates[disconnected].is_empty() {
            return Err(no_endpoint_candidate(layer, expanded_action_index));
        }
    }
    let layer = &layers[first_disconnected];
    let mut failure = CutterGridPlanningFailureV4::for_action(
        hcr_contract::CutterGridPlanningErrorCodeV4::EndpointPtpDisconnected,
        hcr_contract::CutterGridPlanningStageV4::PtpEdge,
        layer.action_index,
        &layer.action,
        Some(move_end_coord(&layer.action)),
    );
    failure.expanded_action_index = expanded_action_index;
    Err(failure)
}

/// Build a complete, dynamically certified and locally signed V4 plan.
pub fn plan_cutter_grid_v4(
    challenge: &ChallengeDefinition,
    compiled: &CutterGridV4CompiledProgram,
    profile: &hcr_contract::CutterGridProfileV4,
) -> Result<hcr_contract::CutterTrajectoryPlanV4, CutterGridPlanningFailureV4> {
    let geometry = plan_cutter_grid_v4_geometry(challenge, compiled, profile)?;
    finalize_cutter_grid_v4_geometry_plan(challenge, compiled, profile, geometry)
}

fn finalize_cutter_grid_v4_geometry_plan(
    challenge: &ChallengeDefinition,
    compiled: &CutterGridV4CompiledProgram,
    profile: &hcr_contract::CutterGridProfileV4,
    geometry: CutterGridV4GeometryPlan,
) -> Result<hcr_contract::CutterTrajectoryPlanV4, CutterGridPlanningFailureV4> {
    let positioning = retime_one_ptp_v4(
        challenge,
        &geometry.positioning_primitive,
        &profile.motion_limits,
    )
    .map_err(|code| {
        system_failure_v4(
            code,
            hcr_contract::CutterGridPlanningStageV4::MotionCertificate,
        )
    })?;
    assert_zero_hair_contact_v4(challenge, &positioning.certificate.samples).map_err(|code| {
        system_failure_v4(
            code,
            hcr_contract::CutterGridPlanningStageV4::SweepCertificate,
        )
    })?;

    let mut metrics = DynamicMetricsV4::default();
    merge_dynamic_metrics_v4(&mut metrics, &positioning);
    let mut remaining_hair = challenge
        .initial_hair
        .voxels
        .iter()
        .copied()
        .collect::<crate::voxel::VoxelSet>();
    let mut actions = Vec::with_capacity(geometry.actions.len());
    for (action_index, geometry_action) in geometry.actions.iter().enumerate() {
        match geometry_action {
            CutterGridGeometryActionV4::Wait { action } => {
                let CutterGridExecutableActionV4::Wait {
                    occurrence_id,
                    source_block_id,
                    duration_ms,
                    logical_command_count,
                } = action
                else {
                    return Err(serialization_failure_v4(action_index, action));
                };
                actions.push(hcr_contract::CutterGridTrajectoryActionV4::Wait {
                    occurrence_id: occurrence_id.clone(),
                    source_block_id: source_block_id.clone(),
                    duration_ms: *duration_ms,
                    logical_command_count: *logical_command_count,
                    expected_cut_voxels: Vec::new(),
                });
            }
            CutterGridGeometryActionV4::Move { action, primitives } => {
                let CutterGridExecutableActionV4::Move {
                    occurrence_id,
                    source_block_id,
                    direction,
                    distance,
                    start_coord,
                    end_coord,
                    logical_command_count,
                } = action
                else {
                    return Err(serialization_failure_v4(action_index, action));
                };
                let certified = retime_move_ptps_v4(challenge, primitives, &profile.motion_limits)
                    .map_err(|code| {
                        action_failure_v4(
                            code,
                            hcr_contract::CutterGridPlanningStageV4::MotionCertificate,
                            action_index,
                            action,
                        )
                    })?;
                for primitive in &certified {
                    merge_dynamic_metrics_v4(&mut metrics, primitive);
                }
                let sweep = collect_actual_sweep_v4(challenge, &certified, &remaining_hair);
                for hit in &sweep.cut_voxels {
                    remaining_hair.remove(hit);
                }
                actions.push(hcr_contract::CutterGridTrajectoryActionV4::Move {
                    occurrence_id: occurrence_id.clone(),
                    source_block_id: source_block_id.clone(),
                    direction: *direction,
                    distance: *distance,
                    start_coord: *start_coord,
                    end_coord: *end_coord,
                    logical_command_count: *logical_command_count,
                    primitives: certified.into_iter().map(|item| item.primitive).collect(),
                    contact_events: sweep.contact_events,
                    expected_cut_voxels: sorted_voxel_keys_v4(&sweep.cut_voxels),
                });
            }
        }
    }
    let entry_signature = stable_fnv_signature_v4(&positioning.primitive).map_err(|_| {
        system_failure_v4(
            hcr_contract::CutterGridPlanningErrorCodeV4::PlanSignatureMismatch,
            hcr_contract::CutterGridPlanningStageV4::Serialization,
        )
    })?;
    let estimated_duration_ms = actions
        .iter()
        .map(|action| match action {
            hcr_contract::CutterGridTrajectoryActionV4::Move { primitives, .. } => primitives
                .iter()
                .map(|primitive| primitive.duration_ms)
                .sum::<f64>(),
            hcr_contract::CutterGridTrajectoryActionV4::Wait { duration_ms, .. } => *duration_ms,
        })
        .sum();
    let mut diagnostics = geometry.diagnostics;
    diagnostics.actual_speed_scale =
        profile.motion_limits.requested_speed_scale * metrics.maximum_velocity_ratio.min(1.0);
    diagnostics.maximum_velocity_ratio = metrics.maximum_velocity_ratio;
    diagnostics.maximum_acceleration_ratio = metrics.maximum_acceleration_ratio;
    diagnostics.maximum_jerk_ratio = metrics.maximum_jerk_ratio;
    diagnostics.adaptive_validation_sample_count = metrics.adaptive_validation_sample_count;
    diagnostics.maximum_normalized_joint_step = diagnostics
        .maximum_normalized_joint_step
        .max(metrics.maximum_normalized_joint_step);
    diagnostics.maximum_end_effector_chord_deviation = diagnostics
        .maximum_end_effector_chord_deviation
        .max(metrics.maximum_end_effector_chord_deviation);
    diagnostics.minimum_head_clearance = finite_min_v4(
        diagnostics.minimum_head_clearance,
        metrics.minimum_head_clearance,
    );
    diagnostics.minimum_joint_limit_margin = finite_min_v4(
        diagnostics.minimum_joint_limit_margin,
        metrics.minimum_joint_limit_margin,
    );
    let mut plan = hcr_contract::CutterTrajectoryPlanV4 {
        kind: "cutter-grid-trajectory".into(),
        version: hcr_contract::CUTTER_TRAJECTORY_PLAN_V4_VERSION,
        planner_version: CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION.into(),
        challenge_signature: profile.challenge_signature.clone(),
        positioning: hcr_contract::CutterGridPositioningPlanV4 {
            entry_option_id: geometry.entry_option_id,
            primitives: vec![positioning.primitive],
            trajectory_signature: entry_signature,
        },
        start_coord: geometry.start_coord,
        end_coord: geometry.end_coord,
        actions,
        expected_result_voxels: sorted_voxel_keys_v4(&remaining_hair),
        estimated_duration_ms,
        executed_command_count: compiled.executed_command_count,
        motion_limits: profile.motion_limits.clone(),
        motion_limits_signature: profile.motion_limits_signature.clone(),
        diagnostics,
        trajectory_signature: String::new(),
    };
    plan.trajectory_signature = stable_plan_signature_v4(&plan).map_err(|_| {
        system_failure_v4(
            hcr_contract::CutterGridPlanningErrorCodeV4::PlanSignatureMismatch,
            hcr_contract::CutterGridPlanningStageV4::Serialization,
        )
    })?;
    Ok(plan)
}

#[derive(Debug, Clone)]
struct EndpointLayer {
    action_index: usize,
    action: CutterGridExecutableActionV4,
    target_world: Vec3,
}

#[derive(Debug, Clone)]
struct PtpConnection {
    primitives: Vec<hcr_contract::CutterGridSyncPtpPrimitiveV4>,
    primitive_count: u32,
    maximum_normalized_joint_step: f64,
    displacement_squared: f64,
    minimum_head_clearance: f64,
    minimum_joint_limit_margin: f64,
}

#[derive(Debug, Clone)]
struct CompactPath {
    entry_index: usize,
    candidates: Vec<CutterGridIkCandidateV4>,
    connections: Vec<PtpConnection>,
}

fn validate_profile(
    challenge: &ChallengeDefinition,
    compiled: &CutterGridV4CompiledProgram,
    profile: &hcr_contract::CutterGridProfileV4,
) -> Result<(), CutterGridPlanningFailureV4> {
    if compiled.program.planner_version != CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION
        || profile.version != 4
        || profile.planner_version != CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION
        || profile.challenge_signature
            != hcr_contract::cutter_grid_challenge_signature_v2(challenge)
        || !profile.certification.passed
        || !profile.certification.entry_zero_contact
        || profile.profile_signature.is_empty()
        || profile.motion_limits_signature.is_empty()
        || profile.entry_options.len() < 2
        || !profile_entries_are_well_formed(challenge, profile)
        || !profile_roadmap_is_well_formed(challenge, profile)
    {
        return Err(CutterGridPlanningFailureV4 {
            code: hcr_contract::CutterGridPlanningErrorCodeV4::ProfileV4Mismatch,
            stage: hcr_contract::CutterGridPlanningStageV4::Profile,
            source_block_id: None,
            action_index: None,
            expanded_action_index: None,
            target_coord: None,
        });
    }
    Ok(())
}

fn profile_entries_are_well_formed(
    challenge: &ChallengeDefinition,
    profile: &hcr_contract::CutterGridProfileV4,
) -> bool {
    let initial = initial_angles(&challenge.robot_config.joints);
    let mut ids = BTreeSet::new();
    profile.entry_options.iter().all(|entry| {
        let primitive = &entry.positioning_primitive;
        !entry.id.is_empty()
            && ids.insert(entry.id.as_str())
            && !entry.positioning_signature.is_empty()
            && within_joint_limits(&entry.joint_angles, &challenge.robot_config.joints)
            && primitive.kind == "sync-ptp"
            && primitive.interpolation == "synchronized-quintic"
            && primitive.start.joint_angles == initial
            && primitive.end.joint_angles == entry.joint_angles
            && certify_cutter_grid_sync_ptp_geometry_v4(challenge, primitive).is_some()
    })
}

fn profile_roadmap_is_well_formed(
    challenge: &ChallengeDefinition,
    profile: &hcr_contract::CutterGridProfileV4,
) -> bool {
    const ROADMAP_NODE_COUNT: usize = 256;
    const ROADMAP_NEIGHBORS_PER_NODE: usize = 8;
    if profile.roadmap.signature.is_empty()
        || profile.roadmap.nodes.len() != ROADMAP_NODE_COUNT
        || profile.roadmap.edges.len() != ROADMAP_NODE_COUNT * ROADMAP_NEIGHBORS_PER_NODE
    {
        return false;
    }
    let node_ids = profile
        .roadmap
        .nodes
        .iter()
        .map(|node| node.id.as_str())
        .collect::<BTreeSet<_>>();
    if node_ids.len() != ROADMAP_NODE_COUNT
        || profile.roadmap.nodes.iter().any(|node| {
            node.id.is_empty()
                || !within_joint_limits(&node.joint_angles, &challenge.robot_config.joints)
                || certify_cutter_grid_sync_ptp_geometry_v4(
                    challenge,
                    &create_cutter_grid_sync_ptp_primitive_v4(
                        challenge,
                        &node.joint_angles,
                        &node.joint_angles,
                    ),
                )
                .is_none()
        })
    {
        return false;
    }
    let mut edge_ids = BTreeSet::new();
    profile.roadmap.edges.iter().all(|edge| {
        edge.from_node_id != edge.to_node_id
            && node_ids.contains(edge.from_node_id.as_str())
            && node_ids.contains(edge.to_node_id.as_str())
            && edge_ids.insert(format!("{}>{}", edge.from_node_id, edge.to_node_id))
    }) && profile.roadmap.nodes.iter().all(|node| {
        profile
            .roadmap
            .edges
            .iter()
            .filter(|edge| edge.from_node_id == node.id)
            .count()
            == ROADMAP_NEIGHBORS_PER_NODE
    })
}

fn build_endpoint_layers(
    challenge: &ChallengeDefinition,
    compiled: &CutterGridV4CompiledProgram,
    profile: &hcr_contract::CutterGridProfileV4,
) -> Result<Vec<EndpointLayer>, CutterGridPlanningFailureV4> {
    let mut layers = Vec::new();
    for (action_index, action) in compiled.executable_actions.iter().enumerate() {
        let CutterGridExecutableActionV4::Move { end_coord, .. } = action else {
            continue;
        };
        if !bounds_contain(&profile.bounds, *end_coord) {
            return Err(CutterGridPlanningFailureV4::for_action(
                hcr_contract::CutterGridPlanningErrorCodeV4::OutOfBounds,
                hcr_contract::CutterGridPlanningStageV4::Endpoint,
                action_index,
                action,
                Some(*end_coord),
            ));
        }
        layers.push(EndpointLayer {
            action_index,
            action: action.clone(),
            target_world: cutter_grid_coord_to_world_v4(
                *end_coord,
                profile.origin_hair_coord,
                &challenge.voxel_config,
            ),
        });
    }
    Ok(layers)
}

fn generate_endpoint_candidates(
    challenge: &ChallengeDefinition,
    layers: &[EndpointLayer],
    entries: &[CutterGridIkEntrySeedV4],
    seed_budget: usize,
    candidate_limit: usize,
) -> Vec<Vec<CutterGridIkCandidateV4>> {
    let mut result: Vec<Vec<CutterGridIkCandidateV4>> = Vec::new();
    for layer in layers {
        let previous = result.last().cloned().unwrap_or_default();
        result.push(candidates_for_layer(
            challenge,
            layer,
            &previous,
            entries,
            seed_budget,
            candidate_limit,
        ));
    }
    result
}

fn candidates_for_layer(
    challenge: &ChallengeDefinition,
    layer: &EndpointLayer,
    previous_layer: &[CutterGridIkCandidateV4],
    entries: &[CutterGridIkEntrySeedV4],
    seed_budget: usize,
    candidate_limit: usize,
) -> Vec<CutterGridIkCandidateV4> {
    let candidate_namespace = format!("v4-endpoint-{}", layer.action.occurrence_id());
    enumerate_cutter_grid_ik_candidates_v4(
        challenge,
        layer.target_world,
        CutterGridIkOptionsV4 {
            max_error: challenge.voxel_config.size / 16.0,
            previous_layer,
            entry_options: entries,
            seed_budget,
            candidate_limit,
            candidate_namespace: &candidate_namespace,
        },
    )
}

#[derive(Debug, Clone)]
struct GlobalPathSearchV4 {
    path: Option<CompactPath>,
    first_disconnected_layer: Option<usize>,
}

fn select_global_path(
    challenge: &ChallengeDefinition,
    profile: &hcr_contract::CutterGridProfileV4,
    layers: &[EndpointLayer],
    candidates: &[Vec<CutterGridIkCandidateV4>],
    neighbor_limit: usize,
) -> GlobalPathSearchV4 {
    let mut active: BTreeMap<String, CompactPath> = BTreeMap::new();
    let mut cache = BTreeMap::<String, Option<PtpConnection>>::new();
    let Some(first_layer) = candidates.first() else {
        return GlobalPathSearchV4 {
            path: None,
            first_disconnected_layer: Some(0),
        };
    };
    if first_layer.is_empty() {
        return GlobalPathSearchV4 {
            path: None,
            first_disconnected_layer: Some(0),
        };
    }
    for (entry_index, entry) in profile.entry_options.iter().enumerate() {
        for candidate in nearest_candidates(
            &entry.joint_angles,
            first_layer,
            &challenge.robot_config.joints,
            neighbor_limit,
        ) {
            let Some(connection) = connect_candidates(
                challenge,
                profile,
                &entry.id,
                &entry.joint_angles,
                &candidate.id,
                &candidate.joint_angles,
                &mut cache,
            ) else {
                continue;
            };
            accept_path(
                &mut active,
                CompactPath {
                    entry_index,
                    candidates: vec![candidate.clone()],
                    connections: vec![connection],
                },
                &challenge.robot_config.joints,
            );
        }
    }
    if active.is_empty() {
        return GlobalPathSearchV4 {
            path: None,
            first_disconnected_layer: Some(0),
        };
    }
    for layer_index in 1..layers.len() {
        let mut next = BTreeMap::new();
        for path in active.values() {
            let Some(previous) = path.candidates.last() else {
                return GlobalPathSearchV4 {
                    path: None,
                    first_disconnected_layer: Some(layer_index),
                };
            };
            let Some(next_layer) = candidates.get(layer_index) else {
                return GlobalPathSearchV4 {
                    path: None,
                    first_disconnected_layer: Some(layer_index),
                };
            };
            for candidate in nearest_candidates(
                &previous.joint_angles,
                next_layer,
                &challenge.robot_config.joints,
                neighbor_limit,
            ) {
                let Some(connection) = connect_candidates(
                    challenge,
                    profile,
                    &previous.id,
                    &previous.joint_angles,
                    &candidate.id,
                    &candidate.joint_angles,
                    &mut cache,
                ) else {
                    continue;
                };
                let mut next_path = path.clone();
                next_path.candidates.push(candidate.clone());
                next_path.connections.push(connection);
                accept_path(&mut next, next_path, &challenge.robot_config.joints);
            }
        }
        if next.is_empty() {
            return GlobalPathSearchV4 {
                path: None,
                first_disconnected_layer: Some(layer_index),
            };
        }
        active = next;
    }
    GlobalPathSearchV4 {
        path: active
            .into_values()
            .min_by(|left, right| compare_path(left, right, &challenge.robot_config.joints)),
        first_disconnected_layer: None,
    }
}

fn connect_candidates(
    challenge: &ChallengeDefinition,
    profile: &hcr_contract::CutterGridProfileV4,
    start_id: &str,
    start: &BTreeMap<JointId, f64>,
    end_id: &str,
    end: &BTreeMap<JointId, f64>,
    cache: &mut BTreeMap<String, Option<PtpConnection>>,
) -> Option<PtpConnection> {
    let key = format!("{start_id}>{end_id}");
    if let Some(cached) = cache.get(&key) {
        return cached.clone();
    }
    let direct = direct_connection(challenge, start, end)
        .or_else(|| single_roadmap_detour(challenge, profile, start, end));
    cache.insert(key, direct.clone());
    direct
}

fn direct_connection(
    challenge: &ChallengeDefinition,
    start: &BTreeMap<JointId, f64>,
    end: &BTreeMap<JointId, f64>,
) -> Option<PtpConnection> {
    let primitive = create_cutter_grid_sync_ptp_primitive_v4(challenge, start, end);
    let certificate = certify_cutter_grid_sync_ptp_geometry_v4(challenge, &primitive)?;
    Some(PtpConnection {
        primitives: vec![primitive],
        primitive_count: 1,
        maximum_normalized_joint_step: certificate.maximum_normalized_joint_step,
        displacement_squared: normalized_joint_distance_v4(
            start,
            end,
            &challenge.robot_config.joints,
        )
        .powi(2),
        minimum_head_clearance: certificate.minimum_head_clearance,
        minimum_joint_limit_margin: certificate.minimum_joint_limit_margin,
    })
}

fn single_roadmap_detour(
    challenge: &ChallengeDefinition,
    profile: &hcr_contract::CutterGridProfileV4,
    start: &BTreeMap<JointId, f64>,
    end: &BTreeMap<JointId, f64>,
) -> Option<PtpConnection> {
    let mut nodes = profile
        .roadmap
        .nodes
        .iter()
        .filter(|node| {
            profile
                .roadmap
                .edges
                .iter()
                .any(|edge| edge.from_node_id == node.id || edge.to_node_id == node.id)
        })
        .collect::<Vec<_>>();
    nodes.sort_by(|left, right| {
        let left_distance =
            normalized_joint_distance_v4(start, &left.joint_angles, &challenge.robot_config.joints)
                + normalized_joint_distance_v4(
                    &left.joint_angles,
                    end,
                    &challenge.robot_config.joints,
                );
        let right_distance = normalized_joint_distance_v4(
            start,
            &right.joint_angles,
            &challenge.robot_config.joints,
        ) + normalized_joint_distance_v4(
            &right.joint_angles,
            end,
            &challenge.robot_config.joints,
        );
        compare_number(left_distance, right_distance).then_with(|| left.id.cmp(&right.id))
    });
    for node in nodes {
        let Some(first) = direct_connection(challenge, start, &node.joint_angles) else {
            continue;
        };
        let Some(second) = direct_connection(challenge, &node.joint_angles, end) else {
            continue;
        };
        return Some(PtpConnection {
            primitives: vec![first.primitives[0].clone(), second.primitives[0].clone()],
            primitive_count: 2,
            maximum_normalized_joint_step: first
                .maximum_normalized_joint_step
                .max(second.maximum_normalized_joint_step),
            displacement_squared: first.displacement_squared + second.displacement_squared,
            minimum_head_clearance: first
                .minimum_head_clearance
                .min(second.minimum_head_clearance),
            minimum_joint_limit_margin: first
                .minimum_joint_limit_margin
                .min(second.minimum_joint_limit_margin),
        });
    }
    None
}

fn nearest_candidates<'a>(
    start: &BTreeMap<JointId, f64>,
    candidates: &'a [CutterGridIkCandidateV4],
    joints: &[JointConfig],
    limit: usize,
) -> Vec<&'a CutterGridIkCandidateV4> {
    let mut ordered = candidates.iter().collect::<Vec<_>>();
    ordered.sort_by(|left, right| {
        compare_number(
            normalized_joint_distance_v4(start, &left.joint_angles, joints),
            normalized_joint_distance_v4(start, &right.joint_angles, joints),
        )
        .then_with(|| left.id.cmp(&right.id))
    });
    ordered.truncate(limit);
    ordered
}

fn accept_path(
    states: &mut BTreeMap<String, CompactPath>,
    path: CompactPath,
    joints: &[JointConfig],
) {
    let key = format!(
        "{}|{}",
        path.entry_index,
        path.candidates
            .last()
            .map(|candidate| candidate.id.as_str())
            .unwrap_or_default()
    );
    if states
        .get(&key)
        .is_none_or(|known| compare_path(&path, known, joints) == Ordering::Less)
    {
        states.insert(key, path);
    }
}

fn compare_path(left: &CompactPath, right: &CompactPath, joints: &[JointConfig]) -> Ordering {
    let left_primitives = left
        .connections
        .iter()
        .map(|connection| connection.primitive_count)
        .sum::<u32>();
    let right_primitives = right
        .connections
        .iter()
        .map(|connection| connection.primitive_count)
        .sum::<u32>();
    let left_max_step = left.connections.iter().fold(0.0_f64, |max, connection| {
        max.max(connection.maximum_normalized_joint_step)
    });
    let right_max_step = right.connections.iter().fold(0.0_f64, |max, connection| {
        max.max(connection.maximum_normalized_joint_step)
    });
    let left_duration = left
        .connections
        .iter()
        .flat_map(|connection| connection.primitives.iter())
        .map(|primitive| primitive.duration_ms)
        .sum::<f64>();
    let right_duration = right
        .connections
        .iter()
        .flat_map(|connection| connection.primitives.iter())
        .map(|primitive| primitive.duration_ms)
        .sum::<f64>();
    let left_displacement = left
        .connections
        .iter()
        .map(|connection| connection.displacement_squared)
        .sum::<f64>();
    let right_displacement = right
        .connections
        .iter()
        .map(|connection| connection.displacement_squared)
        .sum::<f64>();
    let left_clearance = left
        .connections
        .iter()
        .fold(f64::INFINITY, |min, connection| {
            min.min(connection.minimum_head_clearance)
        });
    let right_clearance = right
        .connections
        .iter()
        .fold(f64::INFINITY, |min, connection| {
            min.min(connection.minimum_head_clearance)
        });
    let left_margin = left
        .connections
        .iter()
        .fold(f64::INFINITY, |min, connection| {
            min.min(connection.minimum_joint_limit_margin)
        });
    let right_margin = right
        .connections
        .iter()
        .fold(f64::INFINITY, |min, connection| {
            min.min(connection.minimum_joint_limit_margin)
        });
    left_primitives
        .cmp(&right_primitives)
        .then_with(|| compare_number(left_max_step, right_max_step))
        .then_with(|| compare_number(left_duration, right_duration))
        .then_with(|| compare_number(left_displacement, right_displacement))
        .then_with(|| compare_number(right_clearance, left_clearance))
        .then_with(|| compare_number(right_margin, left_margin))
        .then_with(|| left.entry_index.cmp(&right.entry_index))
        .then_with(|| {
            left.candidates
                .iter()
                .zip(&right.candidates)
                .map(|(l, r)| compare_angles(&l.joint_angles, &r.joint_angles, joints))
                .find(|order| *order != Ordering::Equal)
                .unwrap_or(Ordering::Equal)
        })
}

fn build_geometry_plan(
    compiled: &CutterGridV4CompiledProgram,
    profile: &hcr_contract::CutterGridProfileV4,
    layers: &[EndpointLayer],
    candidates: &[Vec<CutterGridIkCandidateV4>],
    selected: CompactPath,
    expanded_action_index: Option<u32>,
) -> CutterGridV4GeometryPlan {
    let connection_by_action = layers
        .iter()
        .enumerate()
        .map(|(index, layer)| (layer.action_index, selected.connections[index].clone()))
        .collect::<BTreeMap<_, _>>();
    let actions = compiled
        .executable_actions
        .iter()
        .enumerate()
        .map(|(action_index, action)| match action {
            CutterGridExecutableActionV4::Move { .. } => CutterGridGeometryActionV4::Move {
                action: action.clone(),
                primitives: connection_by_action[&action_index].primitives.clone(),
            },
            CutterGridExecutableActionV4::Wait { .. } => CutterGridGeometryActionV4::Wait {
                action: action.clone(),
            },
        })
        .collect::<Vec<_>>();
    let entry = &profile.entry_options[selected.entry_index];
    let connections = &selected.connections;
    CutterGridV4GeometryPlan {
        entry_option_id: entry.id.clone(),
        positioning_primitive: entry.positioning_primitive.clone(),
        start_coord: [0, 0, 0],
        end_coord: layers
            .last()
            .map(|layer| move_end_coord(&layer.action))
            .unwrap_or([0, 0, 0]),
        actions,
        diagnostics: hcr_contract::CutterGridPlanningDiagnosticsV4 {
            endpoint_layer_count: candidates.len() as u32,
            candidate_counts: candidates.iter().map(|layer| layer.len() as u32).collect(),
            expanded_action_index,
            direct_primitive_count: connections
                .iter()
                .filter(|connection| connection.primitive_count == 1)
                .count() as u32,
            detour_primitive_count: connections
                .iter()
                .filter(|connection| connection.primitive_count == 2)
                .count() as u32
                * 2,
            minimum_head_clearance: connections.iter().fold(f64::INFINITY, |min, connection| {
                min.min(connection.minimum_head_clearance)
            }),
            minimum_joint_limit_margin: connections
                .iter()
                .fold(f64::INFINITY, |min, connection| {
                    min.min(connection.minimum_joint_limit_margin)
                }),
            maximum_normalized_joint_step: connections.iter().fold(0.0_f64, |max, connection| {
                max.max(connection.maximum_normalized_joint_step)
            }),
            maximum_end_effector_chord_deviation: 0.0,
            requested_speed_scale: profile.motion_limits.requested_speed_scale,
            actual_speed_scale: 0.0,
            maximum_velocity_ratio: f64::INFINITY,
            maximum_acceleration_ratio: f64::INFINITY,
            maximum_jerk_ratio: f64::INFINITY,
            adaptive_validation_sample_count: 0,
        },
    }
}

fn build_wait_only_geometry_plan(
    challenge: &ChallengeDefinition,
    compiled: &CutterGridV4CompiledProgram,
    profile: &hcr_contract::CutterGridProfileV4,
) -> CutterGridV4GeometryPlan {
    let (entry_index, entry) = profile
        .entry_options
        .iter()
        .enumerate()
        .min_by(|(_, left), (_, right)| left.id.cmp(&right.id))
        .expect("validated V4 Profile has at least two entries");
    CutterGridV4GeometryPlan {
        entry_option_id: entry.id.clone(),
        positioning_primitive: entry.positioning_primitive.clone(),
        start_coord: [0, 0, 0],
        end_coord: [0, 0, 0],
        actions: compiled
            .executable_actions
            .iter()
            .map(|action| CutterGridGeometryActionV4::Wait {
                action: action.clone(),
            })
            .collect(),
        diagnostics: hcr_contract::CutterGridPlanningDiagnosticsV4 {
            endpoint_layer_count: 0,
            candidate_counts: Vec::new(),
            expanded_action_index: None,
            direct_primitive_count: 0,
            detour_primitive_count: 0,
            minimum_head_clearance: entry.minimum_head_clearance,
            minimum_joint_limit_margin: minimum_normalized_joint_limit_margin_v4(
                &profile.entry_options[entry_index].joint_angles,
                &challenge.robot_config.joints,
            ),
            maximum_normalized_joint_step: 0.0,
            maximum_end_effector_chord_deviation: 0.0,
            requested_speed_scale: profile.motion_limits.requested_speed_scale,
            actual_speed_scale: 0.0,
            maximum_velocity_ratio: f64::INFINITY,
            maximum_acceleration_ratio: f64::INFINITY,
            maximum_jerk_ratio: f64::INFINITY,
            adaptive_validation_sample_count: 0,
        },
    }
}

fn no_endpoint_candidate(
    layer: &EndpointLayer,
    expanded_action_index: Option<u32>,
) -> CutterGridPlanningFailureV4 {
    let mut failure = CutterGridPlanningFailureV4::for_action(
        hcr_contract::CutterGridPlanningErrorCodeV4::EndpointIkNotConverged,
        hcr_contract::CutterGridPlanningStageV4::Endpoint,
        layer.action_index,
        &layer.action,
        Some(move_end_coord(&layer.action)),
    );
    failure.expanded_action_index = expanded_action_index;
    failure
}

fn move_end_coord(action: &CutterGridExecutableActionV4) -> CutterGridCoord {
    match action {
        CutterGridExecutableActionV4::Move { end_coord, .. } => *end_coord,
        CutterGridExecutableActionV4::Wait { .. } => [0, 0, 0],
    }
}

fn bounds_contain(bounds: &hcr_contract::CutterGridBoundsV4, coord: CutterGridCoord) -> bool {
    (0..3).all(|axis| coord[axis] >= bounds.min[axis] && coord[axis] <= bounds.max[axis])
}

fn within_joint_limits(angles: &BTreeMap<JointId, f64>, joints: &[JointConfig]) -> bool {
    joints.iter().all(|joint| {
        angles.get(&joint.id).is_some_and(|angle| {
            angle.is_finite()
                && *angle >= joint.min_angle_deg - 1e-9
                && *angle <= joint.max_angle_deg + 1e-9
        })
    })
}

fn ordered_boundary_state(
    challenge: &ChallengeDefinition,
    state: &hcr_contract::CutterTrajectoryBoundaryStateV4,
) -> Option<hcr_contract::CutterTrajectoryBoundaryStateV4> {
    let mut joint_angles = BTreeMap::new();
    let mut joint_velocities_deg_per_sec = BTreeMap::new();
    let mut joint_accelerations_deg_per_sec2 = BTreeMap::new();
    for joint in &challenge.robot_config.joints {
        let angle = state.joint_angles.get(&joint.id).copied()?;
        let velocity = state.joint_velocities_deg_per_sec.get(&joint.id).copied()?;
        let acceleration = state
            .joint_accelerations_deg_per_sec2
            .get(&joint.id)
            .copied()?;
        if !angle.is_finite() || !velocity.is_finite() || !acceleration.is_finite() {
            return None;
        }
        joint_angles.insert(joint.id.clone(), angle);
        joint_velocities_deg_per_sec.insert(joint.id.clone(), velocity);
        joint_accelerations_deg_per_sec2.insert(joint.id.clone(), acceleration);
    }
    Some(hcr_contract::CutterTrajectoryBoundaryStateV4 {
        joint_angles,
        joint_velocities_deg_per_sec,
        joint_accelerations_deg_per_sec2,
    })
}

fn evaluate_ptp_boundary_v4(
    challenge: &ChallengeDefinition,
    boundary: &hcr_contract::CutterTrajectoryBoundaryStateV4,
    time_ms: f64,
) -> Option<CutterGridPtpSampleV4> {
    let joint_angles = ordered_boundary_state(challenge, boundary)?.joint_angles;
    let pose = compute_robot_pose(
        &challenge.robot_config,
        &JointAngles::from_ordered(joint_angles.clone()),
    )
    .ok()?;
    Some(CutterGridPtpSampleV4 {
        time_ms,
        joint_angles,
        joint_velocities_deg_per_sec: boundary.joint_velocities_deg_per_sec.clone(),
        joint_accelerations_deg_per_sec2: boundary.joint_accelerations_deg_per_sec2.clone(),
        // Mirrors the browser evaluator's boundary representation. Analytic
        // jerk limits are measured separately from quintic coefficients.
        joint_jerks_deg_per_sec3: challenge
            .robot_config
            .joints
            .iter()
            .map(|joint| (joint.id.clone(), 0.0))
            .collect(),
        end_effector: pose.end_effector,
    })
}

#[derive(Debug, Clone, Copy)]
struct QuinticCoefficientsV4 {
    a0: f64,
    a1: f64,
    a2: f64,
    a3: f64,
    a4: f64,
    a5: f64,
}

#[derive(Debug, Clone, Copy)]
struct QuinticDynamicBoundsV4 {
    maximum_velocity: f64,
    maximum_acceleration: f64,
    maximum_jerk: f64,
}

#[derive(Debug, Clone, Copy)]
struct QuinticCurveV4 {
    position: f64,
    velocity: f64,
    acceleration: f64,
    jerk: f64,
}

fn quintic_boundary_curve_v4(
    start_position: f64,
    start_velocity: f64,
    start_acceleration: f64,
    end_position: f64,
    end_velocity: f64,
    end_acceleration: f64,
    duration_seconds: f64,
    time_seconds: f64,
) -> QuinticCurveV4 {
    let coefficients = quintic_coefficients_v4(
        start_position,
        start_velocity,
        start_acceleration,
        end_position,
        end_velocity,
        end_acceleration,
        duration_seconds,
    )
    .expect("validated PTP duration is positive and finite");
    let time2 = time_seconds.powi(2);
    let time3 = time_seconds.powi(3);
    let time4 = time_seconds.powi(4);
    let time5 = time_seconds.powi(5);
    QuinticCurveV4 {
        position: coefficients.a0
            + coefficients.a1 * time_seconds
            + coefficients.a2 * time2
            + coefficients.a3 * time3
            + coefficients.a4 * time4
            + coefficients.a5 * time5,
        velocity: coefficients.a1
            + 2.0 * coefficients.a2 * time_seconds
            + 3.0 * coefficients.a3 * time2
            + 4.0 * coefficients.a4 * time3
            + 5.0 * coefficients.a5 * time4,
        acceleration: 2.0 * coefficients.a2
            + 6.0 * coefficients.a3 * time_seconds
            + 12.0 * coefficients.a4 * time2
            + 20.0 * coefficients.a5 * time3,
        jerk: 6.0 * coefficients.a3
            + 24.0 * coefficients.a4 * time_seconds
            + 60.0 * coefficients.a5 * time2,
    }
}

fn quintic_coefficients_v4(
    start_position: f64,
    start_velocity: f64,
    start_acceleration: f64,
    end_position: f64,
    end_velocity: f64,
    end_acceleration: f64,
    duration_seconds: f64,
) -> Option<QuinticCoefficientsV4> {
    if !duration_seconds.is_finite() || duration_seconds <= 0.0 {
        return None;
    }
    let duration2 = duration_seconds.powi(2);
    let duration3 = duration_seconds.powi(3);
    let duration4 = duration_seconds.powi(4);
    let duration5 = duration_seconds.powi(5);
    Some(QuinticCoefficientsV4 {
        a0: start_position,
        a1: start_velocity,
        a2: start_acceleration / 2.0,
        a3: (20.0 * (end_position - start_position)
            - (8.0 * end_velocity + 12.0 * start_velocity) * duration_seconds
            - (3.0 * start_acceleration - end_acceleration) * duration2)
            / (2.0 * duration3),
        a4: (30.0 * (start_position - end_position)
            + (14.0 * end_velocity + 16.0 * start_velocity) * duration_seconds
            + (3.0 * start_acceleration - 2.0 * end_acceleration) * duration2)
            / (2.0 * duration4),
        a5: (12.0 * (end_position - start_position)
            - (6.0 * end_velocity + 6.0 * start_velocity) * duration_seconds
            - (start_acceleration - end_acceleration) * duration2)
            / (2.0 * duration5),
    })
}

fn exact_quintic_dynamic_bounds_v4(
    coefficients: QuinticCoefficientsV4,
    duration_seconds: f64,
) -> QuinticDynamicBoundsV4 {
    let velocity = |time: f64| quintic_boundary_values_v4(coefficients, time).velocity;
    let acceleration = |time: f64| quintic_boundary_values_v4(coefficients, time).acceleration;
    let jerk = |time: f64| quintic_boundary_values_v4(coefficients, time).jerk;
    let jerk_roots = bounded_quadratic_roots_v4(
        60.0 * coefficients.a5,
        24.0 * coefficients.a4,
        6.0 * coefficients.a3,
        duration_seconds,
    );
    let acceleration_roots = roots_from_monotone_partitions_v4(
        acceleration,
        &with_interval_bounds(&jerk_roots, duration_seconds),
    );
    let jerk_vertex = if coefficients.a5.abs() <= 1e-15 {
        Vec::new()
    } else {
        bounded_roots_v4(
            &[-coefficients.a4 / (5.0 * coefficients.a5)],
            duration_seconds,
        )
    };
    QuinticDynamicBoundsV4 {
        maximum_velocity: maximum_absolute_v4(
            velocity,
            &with_interval_bounds(&acceleration_roots, duration_seconds),
        ),
        maximum_acceleration: maximum_absolute_v4(
            acceleration,
            &with_interval_bounds(&jerk_roots, duration_seconds),
        ),
        maximum_jerk: maximum_absolute_v4(
            jerk,
            &with_interval_bounds(&jerk_vertex, duration_seconds),
        ),
    }
}

fn quintic_boundary_values_v4(coefficients: QuinticCoefficientsV4, time: f64) -> QuinticCurveV4 {
    let time2 = time.powi(2);
    let time3 = time.powi(3);
    let time4 = time.powi(4);
    let time5 = time.powi(5);
    QuinticCurveV4 {
        position: coefficients.a0
            + coefficients.a1 * time
            + coefficients.a2 * time2
            + coefficients.a3 * time3
            + coefficients.a4 * time4
            + coefficients.a5 * time5,
        velocity: coefficients.a1
            + 2.0 * coefficients.a2 * time
            + 3.0 * coefficients.a3 * time2
            + 4.0 * coefficients.a4 * time3
            + 5.0 * coefficients.a5 * time4,
        acceleration: 2.0 * coefficients.a2
            + 6.0 * coefficients.a3 * time
            + 12.0 * coefficients.a4 * time2
            + 20.0 * coefficients.a5 * time3,
        jerk: 6.0 * coefficients.a3
            + 24.0 * coefficients.a4 * time
            + 60.0 * coefficients.a5 * time2,
    }
}

fn bounded_quadratic_roots_v4(a: f64, b: f64, c: f64, maximum: f64) -> Vec<f64> {
    if a.abs() <= 1e-15 {
        return if b.abs() <= 1e-15 {
            Vec::new()
        } else {
            bounded_roots_v4(&[-c / b], maximum)
        };
    }
    let discriminant = b * b - 4.0 * a * c;
    if discriminant < -1e-12 {
        return Vec::new();
    }
    let root = libm::sqrt(discriminant.max(0.0));
    bounded_roots_v4(&[(-b - root) / (2.0 * a), (-b + root) / (2.0 * a)], maximum)
}

fn roots_from_monotone_partitions_v4(
    evaluate: impl Fn(f64) -> f64,
    partitions: &[f64],
) -> Vec<f64> {
    let mut ordered = partitions
        .iter()
        .copied()
        .map(|value| round_precision_v4(value, 12))
        .collect::<Vec<_>>();
    ordered.sort_by(|left, right| compare_number(*left, *right));
    ordered.dedup_by(|left, right| (*left - *right).abs() <= 1e-12);
    let mut roots = Vec::new();
    for value in &ordered {
        if evaluate(*value).abs() <= 1e-9 {
            roots.push(*value);
        }
    }
    for pair in ordered.windows(2) {
        let mut low = pair[0];
        let mut high = pair[1];
        let mut low_value = evaluate(low);
        let high_value = evaluate(high);
        if low_value == 0.0 || high_value == 0.0 || low_value * high_value > 0.0 {
            continue;
        }
        for _ in 0..80 {
            let middle = (low + high) / 2.0;
            let middle_value = evaluate(middle);
            if middle_value == 0.0 {
                low = middle;
                high = middle;
                break;
            }
            if low_value * middle_value <= 0.0 {
                high = middle;
            } else {
                low = middle;
                low_value = middle_value;
            }
        }
        roots.push((low + high) / 2.0);
    }
    roots.sort_by(|left, right| compare_number(*left, *right));
    roots.dedup_by(|left, right| (*left - *right).abs() <= 1e-12);
    roots
}

fn bounded_roots_v4(values: &[f64], maximum: f64) -> Vec<f64> {
    let mut roots = values
        .iter()
        .copied()
        .filter(|value| value.is_finite() && *value > 0.0 && *value < maximum)
        .map(|value| round_precision_v4(value, 12))
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| compare_number(*left, *right));
    roots.dedup_by(|left, right| (*left - *right).abs() <= 1e-12);
    roots
}

fn with_interval_bounds(interior: &[f64], maximum: f64) -> Vec<f64> {
    let mut values = Vec::with_capacity(interior.len() + 2);
    values.push(0.0);
    values.extend_from_slice(interior);
    values.push(maximum);
    values
}

fn maximum_absolute_v4(evaluate: impl Fn(f64) -> f64, points: &[f64]) -> f64 {
    points
        .iter()
        .map(|time| evaluate(*time).abs())
        .fold(0.0_f64, f64::max)
}

fn round_precision_v4(value: f64, digits: i32) -> f64 {
    let scale = 10.0_f64.powi(digits);
    (value * scale).round() / scale
}

fn adaptive_ptp_visit_v4(
    challenge: &ChallengeDefinition,
    primitive: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
    start: CutterGridPtpSampleV4,
    end: CutterGridPtpSampleV4,
    depth: usize,
    output: &mut Vec<CutterGridPtpSampleV4>,
) -> Result<(), PtpCertificateFailureV4> {
    let middle_time = (start.time_ms + end.time_ms) / 2.0;
    let middle = evaluate_cutter_grid_sync_ptp_v4(challenge, primitive, middle_time)
        .ok_or(PtpCertificateFailureV4::SamplingLimit)?;
    let start_clearance = inspect_ptp_sample_v4(challenge, &start)?;
    let middle_clearance = inspect_ptp_sample_v4(challenge, &middle)?;
    let end_clearance = inspect_ptp_sample_v4(challenge, &end)?;
    let maximum_joint_delta = challenge
        .robot_config
        .joints
        .iter()
        .map(|joint| {
            let start_angle = start.joint_angles[&joint.id];
            let middle_angle = middle.joint_angles[&joint.id];
            let end_angle = end.joint_angles[&joint.id];
            (middle_angle - start_angle)
                .abs()
                .max((end_angle - middle_angle).abs())
        })
        .fold(0.0_f64, f64::max);
    let interval_clearance = start_clearance.min(middle_clearance).min(end_clearance);
    let near_head = interval_clearance <= challenge.voxel_config.size;
    let maximum_joint_limit = if near_head { 0.25 } else { 1.0 };
    let maximum_end_effector_distance =
        challenge.voxel_config.size / if near_head { 16.0 } else { 8.0 };
    let displacement_bound =
        conservative_link_displacement_bound_v4(challenge, &start.joint_angles, &end.joint_angles);
    let needs_subdivision = depth < CUTTER_GRID_V4_ADAPTIVE_MIN_SUBDIVISION_DEPTH
        || maximum_joint_delta > maximum_joint_limit + 1e-12
        || distance(start.end_effector, end.end_effector) > maximum_end_effector_distance + 1e-12
        || interval_clearance <= displacement_bound + 1e-12;
    if needs_subdivision {
        if depth >= CUTTER_GRID_V4_ADAPTIVE_MAX_SUBDIVISION_DEPTH {
            return Err(PtpCertificateFailureV4::SamplingLimit);
        }
        adaptive_ptp_visit_v4(
            challenge,
            primitive,
            start,
            middle.clone(),
            depth + 1,
            output,
        )?;
        adaptive_ptp_visit_v4(challenge, primitive, middle, end, depth + 1, output)?;
    } else {
        output.push(start);
        output.push(middle);
        output.push(end);
    }
    Ok(())
}

fn inspect_ptp_sample_v4(
    challenge: &ChallengeDefinition,
    sample: &CutterGridPtpSampleV4,
) -> Result<f64, PtpCertificateFailureV4> {
    if !within_joint_limits(&sample.joint_angles, &challenge.robot_config.joints) {
        return Err(PtpCertificateFailureV4::JointLimit);
    }
    let pose = compute_robot_pose(
        &challenge.robot_config,
        &JointAngles::from_ordered(sample.joint_angles.clone()),
    )
    .map_err(|_| PtpCertificateFailureV4::JointLimit)?;
    if find_robot_head_collision(
        &pose,
        &challenge.voxel_config,
        &challenge.robot_config.geometry,
    )
    .is_some()
    {
        return Err(PtpCertificateFailureV4::HeadCollision);
    }
    Ok(measure_robot_head_clearance(
        &pose,
        &challenge.voxel_config,
        &challenge.robot_config.geometry,
    ))
}

fn conservative_link_displacement_bound_v4(
    challenge: &ChallengeDefinition,
    start: &BTreeMap<JointId, f64>,
    end: &BTreeMap<JointId, f64>,
) -> f64 {
    let geometry = &challenge.robot_config.geometry;
    let maximum_reach = geometry.upper_arm_length + geometry.forearm_length + geometry.tool_length;
    let angular_travel_radians = challenge
        .robot_config
        .joints
        .iter()
        .map(|joint| (end[&joint.id] - start[&joint.id]).abs().to_radians())
        .sum::<f64>();
    maximum_reach * angular_travel_radians
}

fn retime_move_ptps_v4(
    challenge: &ChallengeDefinition,
    primitives: &[hcr_contract::CutterGridSyncPtpPrimitiveV4],
    limits: &hcr_contract::CutterGridMotionLimitsV4,
) -> Result<Vec<RetimedPtpV4>, hcr_contract::CutterGridPlanningErrorCodeV4> {
    match primitives {
        [primitive] => Ok(vec![retime_one_ptp_v4(challenge, primitive, limits)?]),
        [first, second] => retime_detour_ptps_v4(challenge, first, second, limits),
        _ => Err(hcr_contract::CutterGridPlanningErrorCodeV4::MotionPrimitiveBudgetExhausted),
    }
}

fn retime_one_ptp_v4(
    challenge: &ChallengeDefinition,
    geometry: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
    limits: &hcr_contract::CutterGridMotionLimitsV4,
) -> Result<RetimedPtpV4, hcr_contract::CutterGridPlanningErrorCodeV4> {
    let mut duration_ms = geometry.duration_ms.max(CUTTER_GRID_V4_MIN_PTP_DURATION_MS);
    for _ in 0..CUTTER_GRID_V4_MAX_RETIMING_ATTEMPTS {
        let primitive = create_cutter_grid_sync_ptp_with_boundary_states_v4(
            challenge,
            &geometry.start,
            &geometry.end,
            duration_ms,
        )
        .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
        let dynamics = measure_cutter_grid_sync_ptp_dynamics_v4(challenge, &primitive, limits)
            .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
        if dynamics.valid {
            let certificate = certify_cutter_grid_sync_ptp_adaptive_v4(challenge, &primitive)?;
            return Ok(RetimedPtpV4 {
                maximum_end_effector_chord_deviation: ptp_chord_deviation_v4(challenge, &primitive)
                    .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?,
                primitive,
                certificate,
                dynamics,
            });
        }
        duration_ms = round_duration_ms_v4(duration_ms * required_duration_scale_v4(&dynamics));
    }
    Err(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)
}

fn retime_detour_ptps_v4(
    challenge: &ChallengeDefinition,
    first_geometry: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
    second_geometry: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
    limits: &hcr_contract::CutterGridMotionLimitsV4,
) -> Result<Vec<RetimedPtpV4>, hcr_contract::CutterGridPlanningErrorCodeV4> {
    let mut first_duration_ms = first_geometry
        .duration_ms
        .max(CUTTER_GRID_V4_MIN_PTP_DURATION_MS);
    let mut second_duration_ms = second_geometry
        .duration_ms
        .max(CUTTER_GRID_V4_MIN_PTP_DURATION_MS);
    for _ in 0..CUTTER_GRID_V4_MAX_RETIMING_ATTEMPTS {
        let shared = shared_detour_boundary_v4(
            challenge,
            &first_geometry.start,
            &first_geometry.end,
            &second_geometry.end,
            first_duration_ms,
            second_duration_ms,
        )
        .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
        let first = create_cutter_grid_sync_ptp_with_boundary_states_v4(
            challenge,
            &first_geometry.start,
            &shared,
            first_duration_ms,
        )
        .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
        let second = create_cutter_grid_sync_ptp_with_boundary_states_v4(
            challenge,
            &shared,
            &second_geometry.end,
            second_duration_ms,
        )
        .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
        let first_dynamics = measure_cutter_grid_sync_ptp_dynamics_v4(challenge, &first, limits)
            .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
        let second_dynamics = measure_cutter_grid_sync_ptp_dynamics_v4(challenge, &second, limits)
            .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?;
        if first_dynamics.valid && second_dynamics.valid {
            return Ok(vec![
                RetimedPtpV4 {
                    certificate: certify_cutter_grid_sync_ptp_adaptive_v4(challenge, &first)?,
                    maximum_end_effector_chord_deviation: ptp_chord_deviation_v4(challenge, &first)
                        .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?,
                    primitive: first,
                    dynamics: first_dynamics,
                },
                RetimedPtpV4 {
                    certificate: certify_cutter_grid_sync_ptp_adaptive_v4(challenge, &second)?,
                    maximum_end_effector_chord_deviation: ptp_chord_deviation_v4(
                        challenge, &second,
                    )
                    .ok_or(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)?,
                    primitive: second,
                    dynamics: second_dynamics,
                },
            ]);
        }
        let scale = required_duration_scale_v4(&first_dynamics)
            .max(required_duration_scale_v4(&second_dynamics));
        first_duration_ms = round_duration_ms_v4(first_duration_ms * scale);
        second_duration_ms = round_duration_ms_v4(second_duration_ms * scale);
    }
    Err(hcr_contract::CutterGridPlanningErrorCodeV4::PtpCertificateFailed)
}

fn shared_detour_boundary_v4(
    challenge: &ChallengeDefinition,
    start: &hcr_contract::CutterTrajectoryBoundaryStateV4,
    via: &hcr_contract::CutterTrajectoryBoundaryStateV4,
    end: &hcr_contract::CutterTrajectoryBoundaryStateV4,
    first_duration_ms: f64,
    second_duration_ms: f64,
) -> Option<hcr_contract::CutterTrajectoryBoundaryStateV4> {
    let first_seconds = first_duration_ms / 1_000.0;
    let second_seconds = second_duration_ms / 1_000.0;
    if first_seconds <= 0.0 || second_seconds <= 0.0 {
        return None;
    }
    let mut joint_angles = BTreeMap::new();
    let mut joint_velocities_deg_per_sec = BTreeMap::new();
    let mut joint_accelerations_deg_per_sec2 = BTreeMap::new();
    for joint in &challenge.robot_config.joints {
        let start_angle = start.joint_angles.get(&joint.id).copied()?;
        let via_angle = via.joint_angles.get(&joint.id).copied()?;
        let end_angle = end.joint_angles.get(&joint.id).copied()?;
        let toward_via = (via_angle - start_angle) / first_seconds;
        let away_from_via = (end_angle - via_angle) / second_seconds;
        let velocity = if toward_via * away_from_via > 0.0 {
            2.0 * toward_via * away_from_via / (toward_via + away_from_via)
        } else {
            0.0
        };
        joint_angles.insert(joint.id.clone(), via_angle);
        joint_velocities_deg_per_sec.insert(joint.id.clone(), velocity);
        joint_accelerations_deg_per_sec2.insert(joint.id.clone(), 0.0);
    }
    Some(hcr_contract::CutterTrajectoryBoundaryStateV4 {
        joint_angles,
        joint_velocities_deg_per_sec,
        joint_accelerations_deg_per_sec2,
    })
}

fn required_duration_scale_v4(dynamics: &CutterGridPtpDynamicsV4) -> f64 {
    1.05_f64
        .max(dynamics.maximum_velocity_ratio * 1.01)
        .max(libm::sqrt(dynamics.maximum_acceleration_ratio) * 1.01)
        .max(libm::cbrt(dynamics.maximum_jerk_ratio) * 1.01)
}

fn round_duration_ms_v4(value: f64) -> f64 {
    libm::ceil(value.max(CUTTER_GRID_V4_MIN_PTP_DURATION_MS))
}

#[derive(Debug, Clone)]
struct DynamicMetricsV4 {
    maximum_velocity_ratio: f64,
    maximum_acceleration_ratio: f64,
    maximum_jerk_ratio: f64,
    adaptive_validation_sample_count: u32,
    maximum_normalized_joint_step: f64,
    maximum_end_effector_chord_deviation: f64,
    minimum_head_clearance: f64,
    minimum_joint_limit_margin: f64,
}

impl Default for DynamicMetricsV4 {
    fn default() -> Self {
        Self {
            maximum_velocity_ratio: 0.0,
            maximum_acceleration_ratio: 0.0,
            maximum_jerk_ratio: 0.0,
            adaptive_validation_sample_count: 0,
            maximum_normalized_joint_step: 0.0,
            maximum_end_effector_chord_deviation: 0.0,
            minimum_head_clearance: f64::INFINITY,
            minimum_joint_limit_margin: f64::INFINITY,
        }
    }
}

fn merge_dynamic_metrics_v4(metrics: &mut DynamicMetricsV4, item: &RetimedPtpV4) {
    metrics.maximum_velocity_ratio = metrics
        .maximum_velocity_ratio
        .max(item.dynamics.maximum_velocity_ratio);
    metrics.maximum_acceleration_ratio = metrics
        .maximum_acceleration_ratio
        .max(item.dynamics.maximum_acceleration_ratio);
    metrics.maximum_jerk_ratio = metrics
        .maximum_jerk_ratio
        .max(item.dynamics.maximum_jerk_ratio);
    metrics.adaptive_validation_sample_count = metrics
        .adaptive_validation_sample_count
        .saturating_add(item.certificate.samples.len() as u32);
    metrics.maximum_normalized_joint_step = metrics
        .maximum_normalized_joint_step
        .max(item.certificate.maximum_normalized_joint_step);
    metrics.maximum_end_effector_chord_deviation = metrics
        .maximum_end_effector_chord_deviation
        .max(item.maximum_end_effector_chord_deviation);
    metrics.minimum_head_clearance = finite_min_v4(
        metrics.minimum_head_clearance,
        item.certificate.minimum_head_clearance,
    );
    metrics.minimum_joint_limit_margin = finite_min_v4(
        metrics.minimum_joint_limit_margin,
        item.certificate.minimum_joint_limit_margin,
    );
}

fn finite_min_v4(left: f64, right: f64) -> f64 {
    if left.is_finite() {
        left.min(right)
    } else {
        right
    }
}

fn ptp_chord_deviation_v4(
    challenge: &ChallengeDefinition,
    primitive: &hcr_contract::CutterGridSyncPtpPrimitiveV4,
) -> Option<f64> {
    let start = evaluate_cutter_grid_sync_ptp_v4(challenge, primitive, 0.0)?.end_effector;
    let end =
        evaluate_cutter_grid_sync_ptp_v4(challenge, primitive, primitive.duration_ms)?.end_effector;
    let mut maximum = 0.0_f64;
    for progress in [0.25_f64, 0.5, 0.75] {
        let actual = evaluate_cutter_grid_sync_ptp_v4(
            challenge,
            primitive,
            primitive.duration_ms * progress,
        )?
        .end_effector;
        maximum = maximum.max(point_segment_distance_v4(actual, start, end));
    }
    Some(maximum)
}

fn point_segment_distance_v4(point: Vec3, start: Vec3, end: Vec3) -> f64 {
    let direction = [end[0] - start[0], end[1] - start[1], end[2] - start[2]];
    let length_squared = direction
        .iter()
        .map(|component| component * component)
        .sum::<f64>();
    let progress = if length_squared == 0.0 {
        0.0
    } else {
        clamp(
            ((point[0] - start[0]) * direction[0]
                + (point[1] - start[1]) * direction[1]
                + (point[2] - start[2]) * direction[2])
                / length_squared,
            0.0,
            1.0,
        )
    };
    distance(
        point,
        [
            start[0] + direction[0] * progress,
            start[1] + direction[1] * progress,
            start[2] + direction[2] * progress,
        ],
    )
}

#[derive(Debug, Clone)]
struct CutterGridSweepV4 {
    cut_voxels: VoxelSet,
    contact_events: Vec<hcr_contract::CutterGridContactEventV4>,
}

fn collect_actual_sweep_v4(
    challenge: &ChallengeDefinition,
    primitives: &[RetimedPtpV4],
    remaining_hair: &VoxelSet,
) -> CutterGridSweepV4 {
    let mut cut_voxels = VoxelSet::new();
    let mut events = BTreeMap::<i64, VoxelSet>::new();
    let mut elapsed_ms = 0.0_f64;
    for item in primitives {
        for pair in item.certificate.samples.windows(2) {
            for hit in find_swept_voxel_hits(
                pair[0].end_effector,
                pair[1].end_effector,
                remaining_hair,
                &challenge.voxel_config,
                challenge.robot_config.geometry.tool_radius,
            ) {
                if !cut_voxels.insert(hit) {
                    continue;
                }
                let time_key = ((elapsed_ms + pair[1].time_ms) * 1_000.0).round() as i64;
                events.entry(time_key).or_default().insert(hit);
            }
        }
        elapsed_ms += item.primitive.duration_ms;
    }
    CutterGridSweepV4 {
        cut_voxels,
        contact_events: events
            .into_iter()
            .map(
                |(time_key, voxels)| hcr_contract::CutterGridContactEventV4 {
                    time_ms: time_key as f64 / 1_000.0,
                    voxel_keys: sorted_voxel_keys_v4(&voxels),
                },
            )
            .collect(),
    }
}

fn assert_zero_hair_contact_v4(
    challenge: &ChallengeDefinition,
    samples: &[CutterGridPtpSampleV4],
) -> Result<(), hcr_contract::CutterGridPlanningErrorCodeV4> {
    let hair = challenge
        .initial_hair
        .voxels
        .iter()
        .copied()
        .collect::<VoxelSet>();
    if samples.windows(2).any(|pair| {
        !find_swept_voxel_hits(
            pair[0].end_effector,
            pair[1].end_effector,
            &hair,
            &challenge.voxel_config,
            challenge.robot_config.geometry.tool_radius,
        )
        .is_empty()
    }) {
        Err(hcr_contract::CutterGridPlanningErrorCodeV4::ActualSweepCertificationFailed)
    } else {
        Ok(())
    }
}

fn sorted_voxel_keys_v4(voxels: &VoxelSet) -> Vec<String> {
    let mut keys = voxels.iter().map(coord_to_key).collect::<Vec<_>>();
    keys.sort();
    keys
}

fn stable_fnv_signature_v4(value: &impl Serialize) -> Result<String, serde_json::Error> {
    serde_json::to_string(value).map(|document| hcr_contract::fnv1a64(&document))
}

fn stable_plan_signature_v4(
    plan: &hcr_contract::CutterTrajectoryPlanV4,
) -> Result<String, serde_json::Error> {
    let mut unsigned = plan.clone();
    // Rust's V4 integrity signature is intentionally implementation-local.
    // Emptying the self field gives a stable canonical preimage without claiming
    // byte-for-byte JSON compatibility with the TypeScript Worker.
    unsigned.trajectory_signature.clear();
    stable_fnv_signature_v4(&unsigned)
}

fn system_failure_v4(
    code: hcr_contract::CutterGridPlanningErrorCodeV4,
    stage: hcr_contract::CutterGridPlanningStageV4,
) -> CutterGridPlanningFailureV4 {
    CutterGridPlanningFailureV4 {
        code,
        stage,
        source_block_id: Some("system-positioning".into()),
        action_index: None,
        expanded_action_index: None,
        target_coord: Some([0, 0, 0]),
    }
}

fn action_failure_v4(
    code: hcr_contract::CutterGridPlanningErrorCodeV4,
    stage: hcr_contract::CutterGridPlanningStageV4,
    action_index: usize,
    action: &CutterGridExecutableActionV4,
) -> CutterGridPlanningFailureV4 {
    CutterGridPlanningFailureV4::for_action(
        code,
        stage,
        action_index,
        action,
        Some(move_end_coord(action)),
    )
}

fn serialization_failure_v4(
    action_index: usize,
    action: &CutterGridExecutableActionV4,
) -> CutterGridPlanningFailureV4 {
    action_failure_v4(
        hcr_contract::CutterGridPlanningErrorCodeV4::PlanSignatureMismatch,
        hcr_contract::CutterGridPlanningStageV4::Serialization,
        action_index,
        action,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use hcr_contract::{CutterGridNode, CutterGridProgram};

    fn shipped_challenge() -> ChallengeDefinition {
        let vectors: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/vectors.json"))
                .expect("fixture parses");
        serde_json::from_value(vectors["challenge"].clone()).expect("challenge parses")
    }

    fn program(nodes: Vec<CutterGridNode>) -> CutterGridProgramV4 {
        CutterGridProgram {
            kind: "cutter-grid".into(),
            version: 1,
            planner_version: CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION.into(),
            nodes,
            source_block_count: 1,
        }
    }

    #[test]
    fn compiler_keeps_move_distance_visible_and_repeat_occurrences_stable() {
        let compiled = compile_cutter_grid_program_v4(&program(vec![CutterGridNode::Repeat {
            count: 2,
            body: vec![CutterGridNode::Move {
                direction: CutterGridDirection::Forward,
                distance: 3,
                source_block_id: "forward".into(),
            }],
            source_block_id: "repeat".into(),
        }]))
        .expect("program compiles");

        assert_eq!(compiled.executed_command_count, 6);
        assert_eq!(compiled.executable_actions.len(), 2);
        assert!(matches!(
            &compiled.executable_actions[0],
            CutterGridExecutableActionV4::Move {
                occurrence_id,
                start_coord: [0, 0, 0],
                end_coord: [0, 0, -3],
                logical_command_count: 3,
                ..
            } if occurrence_id == "forward#0"
        ));
        assert!(matches!(
            &compiled.executable_actions[1],
            CutterGridExecutableActionV4::Move {
                occurrence_id,
                start_coord: [0, 0, -3],
                end_coord: [0, 0, -6],
                ..
            } if occurrence_id == "forward#1"
        ));
    }

    #[test]
    fn compiler_enforces_direction_distance_wait_and_command_boundaries() {
        for (direction, expected) in [
            (CutterGridDirection::Right, [1, 0, 0]),
            (CutterGridDirection::Left, [-1, 0, 0]),
            (CutterGridDirection::Up, [0, 1, 0]),
            (CutterGridDirection::Down, [0, -1, 0]),
            (CutterGridDirection::Forward, [0, 0, -1]),
            (CutterGridDirection::Backward, [0, 0, 1]),
        ] {
            let compiled = compile_cutter_grid_program_v4(&program(vec![CutterGridNode::Move {
                direction,
                distance: 1,
                source_block_id: "move".into(),
            }]))
            .expect("direction compiles");
            assert!(matches!(
                compiled.executable_actions[0],
                CutterGridExecutableActionV4::Move { end_coord, .. } if end_coord == expected
            ));
        }

        let invalid = compile_cutter_grid_program_v4(&program(vec![CutterGridNode::Move {
            direction: CutterGridDirection::Right,
            distance: 13,
            source_block_id: "too-far".into(),
        }]))
        .expect_err("distance 13 fails");
        assert_eq!(invalid.code, CutterGridCompileErrorV4Code::InvalidDistance);

        let mut nodes = Vec::new();
        for index in 0..500 {
            nodes.push(CutterGridNode::Wait {
                duration_ms: 0.0,
                source_block_id: format!("wait-{index}"),
            });
        }
        assert_eq!(
            compile_cutter_grid_program_v4(&program(nodes.clone()))
                .expect("exact 500 is accepted")
                .executed_command_count,
            500
        );
        nodes.push(CutterGridNode::Wait {
            duration_ms: 0.0,
            source_block_id: "overflow".into(),
        });
        assert_eq!(
            compile_cutter_grid_program_v4(&program(nodes))
                .expect_err("501 fails")
                .code,
            CutterGridCompileErrorV4Code::CommandLimitExceeded
        );
    }

    #[test]
    fn logical_coordinate_conversion_uses_profile_origin_and_voxel_spacing() {
        let config = VoxelConfig {
            origin: [1.0, 2.0, 3.0],
            size: 0.1,
            head_center: [0.0, 0.0, 0.0],
            head_scale: [1.0, 1.0, 1.0],
        };
        assert_eq!(
            cutter_grid_coord_to_world_v4([-2, 6, -3], [0, -5, 8], &config),
            [0.8, 2.1, 3.5]
        );
    }

    #[test]
    fn halton_sequence_and_joint_distance_are_deterministic() {
        assert_eq!(radical_inverse(1, 2), 0.5);
        assert_eq!(radical_inverse(2, 2), 0.25);
        let joints = vec![JointConfig {
            id: "baseYaw".into(),
            name: "Base".into(),
            axis: hcr_contract::Axis::Y,
            min_angle_deg: 0.0,
            max_angle_deg: 100.0,
            initial_angle_deg: 50.0,
            speed_deg_per_sec: 1.0,
            servo: None,
        }];
        let left = BTreeMap::from([(String::from("baseYaw"), 50.0)]);
        let right = BTreeMap::from([(String::from("baseYaw"), 75.0)]);
        assert_eq!(normalized_joint_distance_v4(&left, &right, &joints), 0.25);
        assert_eq!(
            minimum_normalized_joint_limit_margin_v4(&left, &joints),
            0.5
        );
    }

    #[test]
    fn direct_compact_ptp_preserves_endpoints_and_certifies_the_safe_initial_pose() {
        let challenge = shipped_challenge();
        let angles = initial_angles(&challenge.robot_config.joints);
        let primitive = create_cutter_grid_sync_ptp_primitive_v4(&challenge, &angles, &angles);
        let start =
            evaluate_cutter_grid_sync_ptp_v4(&challenge, &primitive, 0.0).expect("start evaluates");
        let end = evaluate_cutter_grid_sync_ptp_v4(&challenge, &primitive, primitive.duration_ms)
            .expect("end evaluates");

        assert_eq!(primitive.duration_ms, CUTTER_GRID_V4_MIN_PTP_DURATION_MS);
        assert_eq!(start.joint_angles, angles);
        assert_eq!(end.joint_angles, angles);
        assert!(certify_cutter_grid_sync_ptp_geometry_v4(&challenge, &primitive).is_some());
    }

    #[test]
    fn quintic_boundary_evaluator_preserves_explicit_endpoint_derivatives() {
        let challenge = shipped_challenge();
        let start_angles = initial_angles(&challenge.robot_config.joints);
        let mut end_angles = start_angles.clone();
        let joint = &challenge.robot_config.joints[0];
        end_angles.insert(
            joint.id.clone(),
            (start_angles[&joint.id] + 1.0).min(joint.max_angle_deg - 0.001),
        );
        let zero = challenge
            .robot_config
            .joints
            .iter()
            .map(|joint| (joint.id.clone(), 0.0))
            .collect::<BTreeMap<_, _>>();
        let mut end_velocity = zero.clone();
        end_velocity.insert(joint.id.clone(), 2.0);
        let start = hcr_contract::CutterTrajectoryBoundaryStateV4 {
            joint_angles: start_angles.clone(),
            joint_velocities_deg_per_sec: zero.clone(),
            joint_accelerations_deg_per_sec2: zero.clone(),
        };
        let end = hcr_contract::CutterTrajectoryBoundaryStateV4 {
            joint_angles: end_angles.clone(),
            joint_velocities_deg_per_sec: end_velocity.clone(),
            joint_accelerations_deg_per_sec2: zero,
        };
        let primitive =
            create_cutter_grid_sync_ptp_with_boundary_states_v4(&challenge, &start, &end, 500.0)
                .expect("complete finite boundary is accepted");
        let closing = evaluate_cutter_grid_sync_ptp_v4(&challenge, &primitive, 500.0)
            .expect("closing endpoint evaluates");

        assert_eq!(closing.joint_angles, end_angles);
        assert_eq!(closing.joint_velocities_deg_per_sec, end_velocity);
    }
}
