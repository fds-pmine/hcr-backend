//! Server-only compact Cutter Grid V4 planning primitives.
//!
//! This module deliberately stops at source-program compilation, lattice
//! geometry, and multi-branch endpoint IK.  Profile ownership, compact PTP
//! graph search, dynamics, sweep certification, and HTTP are later phases.
//! Keeping this first slice pure lets those phases share the exact same
//! deterministic action and candidate semantics without importing a web stack.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cmp::Ordering;

use hcr_contract::{
    CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION, ChallengeDefinition, CutterGridCoord,
    CutterGridDirection, CutterGridNode, CutterGridProgramV4, JointConfig, JointId, Vec3,
    VoxelConfig, VoxelCoord,
};

use crate::collision::{find_robot_head_collision, measure_robot_head_clearance};
use crate::kinematics::compute_robot_pose;
use crate::state::JointAngles;
use crate::voxel::voxel_coord_to_world;

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
            minimum_joint_limit_margin: minimum_normalized_joint_limit_margin_v4(
                &joint_angles,
                &challenge.robot_config.joints,
            ),
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
        let ordering = compare_number(
            left.get(&joint.id).copied().unwrap_or(f64::NAN),
            right.get(&joint.id).copied().unwrap_or(f64::NAN),
        );
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

#[cfg(test)]
mod tests {
    use super::*;
    use hcr_contract::{CutterGridNode, CutterGridProgram};

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
}
