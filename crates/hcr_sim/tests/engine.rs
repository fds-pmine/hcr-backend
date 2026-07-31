//! Parity tests for the Rust engine.
//!
//! Assertions are derived from the TypeScript engine's semantics
//! (`src/features/**`) and from the shipped challenge
//! (`src/data/challenges/defaultChallenge.ts`), not from observing this port's
//! own output — otherwise the test would only prove the port is self-consistent.

use hcr_contract::*;
use hcr_sim::*;
use std::collections::BTreeSet;

// ---------------------------------------------------------------------------
// Fixtures — geometry, joints, voxel lattice and scoring taken verbatim from
// defaultChallenge.ts. Hair sets are small and explicit so expectations can be
// computed by hand.
// ---------------------------------------------------------------------------

fn joint(
    id: &str,
    axis: Axis,
    min: f64,
    max: f64,
    initial: f64,
    speed: f64,
) -> JointConfig {
    JointConfig {
        id: id.into(),
        name: id.into(),
        axis,
        min_angle_deg: min,
        max_angle_deg: max,
        initial_angle_deg: initial,
        speed_deg_per_sec: speed,
    }
}

/// The five joints of the shipped challenge.
fn default_joints() -> Vec<JointConfig> {
    vec![
        joint("baseYaw", Axis::Y, -60.0, 60.0, -45.0, 60.0),
        joint("shoulderRoll", Axis::X, -45.0, 45.0, 0.0, 45.0),
        joint("shoulder", Axis::Z, -20.0, 100.0, 45.0, 45.0),
        joint("elbow", Axis::Z, -135.0, 10.0, -80.0, 60.0),
        joint("wrist", Axis::Z, -100.0, 100.0, 35.0, 75.0),
    ]
}

/// Joints identical to the shipped ones except every initial angle is zero,
/// which makes the kinematic chain analytically checkable.
fn zeroed_joints() -> Vec<JointConfig> {
    default_joints()
        .into_iter()
        .map(|mut j| {
            j.initial_angle_deg = 0.0;
            j
        })
        .collect()
}

fn default_geometry() -> RobotGeometryConfig {
    RobotGeometryConfig {
        base_position: [0.0, 0.0, 0.0],
        shoulder_height: 0.4,
        upper_arm_length: 1.05,
        forearm_length: 0.9,
        tool_length: 0.35,
        tool_radius: 0.12,
        collision: RobotCollisionConfig {
            link_radius: 0.075,
            joint_radius: 0.18,
            tool_shaft_radius: 0.075,
            head_clearance: 0.02,
        },
    }
}

fn default_voxel_config() -> VoxelConfig {
    VoxelConfig {
        origin: [1.35, 1.5, 0.0],
        size: 0.16,
        head_center: [1.35, 1.42, 0.0],
        head_scale: [0.68, 0.86, 0.68],
    }
}

fn default_scoring() -> ScoringConfig {
    ScoringConfig {
        weights: ScoreWeights {
            completion: 0.6,
            efficiency: 0.25,
            time: 0.15,
        },
        reference_program_cost: 6.25,
        reference_time_ms: 5_645.0,
        command_weight: 0.25,
    }
}

fn coord(x: i32, y: i32, z: i32) -> VoxelCoord {
    VoxelCoord { x, y, z }
}

fn hairstyle(id: &str, voxels: Vec<VoxelCoord>) -> HairstyleDefinition {
    HairstyleDefinition {
        id: id.into(),
        name: id.into(),
        voxels,
    }
}

fn challenge(
    joints: Vec<JointConfig>,
    initial: Vec<VoxelCoord>,
    target: Vec<VoxelCoord>,
) -> ChallengeDefinition {
    ChallengeDefinition {
        id: "test".into(),
        name: "test".into(),
        description: String::new(),
        robot_config: RobotConfig {
            joints,
            geometry: default_geometry(),
        },
        voxel_config: default_voxel_config(),
        initial_hair: hairstyle("initial", initial),
        target_hair: hairstyle("target", target),
        allowed_blocks: vec![
            AllowedBlockType::SetJointAngle,
            AllowedBlockType::Wait,
            AllowedBlockType::Repeat,
        ],
        starter_workspace: None,
        scoring: default_scoring(),
    }
}

fn set_angle(joint_id: &str, angle_deg: f64, block: &str) -> ProgramNode {
    ProgramNode::SetJointAngle {
        joint_id: joint_id.into(),
        angle_deg,
        source_block_id: block.into(),
    }
}

fn wait(duration_ms: f64, block: &str) -> ProgramNode {
    ProgramNode::Wait {
        duration_ms,
        source_block_id: block.into(),
    }
}

fn program(nodes: Vec<ProgramNode>, source_block_count: u32) -> Program {
    Program {
        nodes,
        source_block_count,
    }
}

fn approx(actual: f64, expected: f64, tolerance: f64) {
    assert!(
        (actual - expected).abs() <= tolerance,
        "expected {expected}, got {actual} (tolerance {tolerance})"
    );
}

// ---------------------------------------------------------------------------
// Kinematics — exact values, derived analytically from the rotation matrices.
// ---------------------------------------------------------------------------

#[test]
fn all_angles_zero_extends_along_positive_x() {
    let config = RobotConfig {
        joints: zeroed_joints(),
        geometry: default_geometry(),
    };
    let pose = compute_robot_pose(&config, &JointAngles::initial(&config)).unwrap();

    // Every rotation is the identity, so the links sum along +X from the shoulder.
    assert_eq!(pose.shoulder, [0.0, 0.4, 0.0]);
    approx(pose.end_effector[0], 1.05 + 0.9 + 0.35, 1e-12);
    approx(pose.end_effector[1], 0.4, 1e-12);
    approx(pose.end_effector[2], 0.0, 1e-12);
}

#[test]
fn base_yaw_90_degrees_swings_the_arm_onto_negative_z() {
    let config = RobotConfig {
        joints: zeroed_joints(),
        geometry: default_geometry(),
    };
    let mut angles = JointAngles::initial(&config);
    // rotationY(90°) maps [L,0,0] to [0,0,-L].
    angles.set("baseYaw", 90.0);
    let pose = compute_robot_pose(&config, &angles).unwrap();

    approx(pose.end_effector[0], 0.0, 1e-12);
    approx(pose.end_effector[1], 0.4, 1e-12);
    approx(pose.end_effector[2], -(1.05 + 0.9 + 0.35), 1e-12);
}

#[test]
fn shoulder_roll_rotates_about_x_into_positive_y() {
    let config = RobotConfig {
        joints: zeroed_joints(),
        geometry: default_geometry(),
    };
    let mut angles = JointAngles::initial(&config);
    // rotationZ(90°) maps [L,0,0] to [0,L,0]; the reach folds onto +Y.
    angles.set("shoulder", 90.0);
    let pose = compute_robot_pose(&config, &angles).unwrap();

    approx(pose.end_effector[0], 0.0, 1e-12);
    approx(pose.end_effector[1], 0.4 + 1.05 + 0.9 + 0.35, 1e-12);
    approx(pose.end_effector[2], 0.0, 1e-12);
}

#[test]
fn missing_joint_angle_is_rejected() {
    let config = RobotConfig {
        joints: zeroed_joints(),
        geometry: default_geometry(),
    };
    let mut angles = JointAngles::initial(&config);
    angles.set("wrist", f64::NAN);

    assert!(matches!(
        compute_robot_pose(&config, &angles),
        Err(SimError::MissingJoint { .. })
    ));
}

// ---------------------------------------------------------------------------
// Voxels
// ---------------------------------------------------------------------------

#[test]
fn iou_matches_the_typescript_rules() {
    let empty = VoxelSet::new();
    // SPEC v0.3 §10.3: two empty sets score 100.
    assert_eq!(calculate_voxel_iou(&empty, &empty), 100.0);

    let a: VoxelSet = [coord(0, 0, 0), coord(1, 0, 0)].into_iter().collect();
    assert_eq!(calculate_voxel_iou(&a, &a), 100.0);

    // One side empty scores 0.
    assert_eq!(calculate_voxel_iou(&a, &empty), 0.0);

    let b: VoxelSet = [coord(2, 0, 0), coord(3, 0, 0)].into_iter().collect();
    assert_eq!(calculate_voxel_iou(&a, &b), 0.0);

    // Half overlap: |∩| = 1, |∪| = 3.
    let c: VoxelSet = [coord(1, 0, 0), coord(9, 0, 0)].into_iter().collect();
    approx(calculate_voxel_iou(&a, &c), 100.0 / 3.0, 1e-12);
}

#[test]
fn result_hash_sorts_keys_as_strings_not_numbers() {
    // "10,0,0" precedes "2,0,0" lexicographically but not numerically. The hash
    // must be insensitive to insertion order and reflect the byte sort.
    let forward: VoxelSet = [coord(10, 0, 0), coord(2, 0, 0)].into_iter().collect();
    let reverse: VoxelSet = [coord(2, 0, 0), coord(10, 0, 0)].into_iter().collect();
    assert_eq!(result_voxels_hash(&forward), result_voxels_hash(&reverse));

    // Sanity: distinct contents hash differently, and the digest is hex SHA-256.
    let other: VoxelSet = [coord(2, 0, 0)].into_iter().collect();
    assert_ne!(result_voxels_hash(&forward), result_voxels_hash(&other));
    assert_eq!(result_voxels_hash(&forward).len(), 64);
    assert!(result_voxels_hash(&forward).chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn empty_voxel_set_hashes_to_sha256_of_empty_input() {
    // Well-known constant; pins the "join with \n, no trailing separator" rule.
    assert_eq!(
        result_voxels_hash(&VoxelSet::new()),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}

#[test]
fn segment_aabb_hits_misses_and_grazes() {
    let min = [-1.0, -1.0, -1.0];
    let max = [1.0, 1.0, 1.0];

    assert!(segment_intersects_aabb([-2.0, 0.0, 0.0], [2.0, 0.0, 0.0], min, max));
    assert!(!segment_intersects_aabb([-2.0, 5.0, 0.0], [2.0, 5.0, 0.0], min, max));
    // Endpoint inside.
    assert!(segment_intersects_aabb([0.0, 0.0, 0.0], [5.0, 0.0, 0.0], min, max));
    // Degenerate segment (a point) inside and outside.
    assert!(segment_intersects_aabb([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], min, max));
    assert!(!segment_intersects_aabb([9.0, 9.0, 9.0], [9.0, 9.0, 9.0], min, max));
}

#[test]
fn sweep_expands_each_voxel_by_half_size_plus_tool_radius() {
    let voxel_config = default_voxel_config();
    let voxels: VoxelSet = [coord(0, 0, 0)].into_iter().collect();
    // Voxel (0,0,0) sits at the lattice origin; expansion is 0.16/2 + 0.12 = 0.20.
    let center = voxel_config.origin;

    let just_inside = center[1] + 0.19;
    let hits = find_swept_voxel_hits(
        [center[0] - 1.0, just_inside, center[2]],
        [center[0] + 1.0, just_inside, center[2]],
        &voxels,
        &voxel_config,
        0.12,
    );
    assert_eq!(hits.len(), 1, "a pass 0.19 above centre should hit");

    let just_outside = center[1] + 0.21;
    let misses = find_swept_voxel_hits(
        [center[0] - 1.0, just_outside, center[2]],
        [center[0] + 1.0, just_outside, center[2]],
        &voxels,
        &voxel_config,
        0.12,
    );
    assert!(misses.is_empty(), "a pass 0.21 above centre should miss");
}

// ---------------------------------------------------------------------------
// Program expansion and duration
// ---------------------------------------------------------------------------

#[test]
fn repeat_expands_depth_first_in_order() {
    let p = program(
        vec![ProgramNode::Repeat {
            count: 3,
            body: vec![set_angle("baseYaw", 10.0, "a"), wait(5.0, "b")],
            source_block_id: "r".into(),
        }],
        3,
    );

    let commands = expand_program_default(&p).unwrap();
    assert_eq!(commands.len(), 6);
    assert_eq!(commands[0].source_block_id(), "a");
    assert_eq!(commands[1].source_block_id(), "b");
    assert_eq!(commands[5].source_block_id(), "b");
}

#[test]
fn expansion_accepts_exactly_the_limit_and_rejects_one_more() {
    // The TypeScript pushes first and then compares, so `limit` commands pass.
    let at_limit = program(
        vec![ProgramNode::Repeat {
            count: MAX_RUNTIME_COMMANDS as u32,
            body: vec![set_angle("baseYaw", 10.0, "a")],
            source_block_id: "r".into(),
        }],
        2,
    );
    assert_eq!(
        expand_program_default(&at_limit).unwrap().len(),
        MAX_RUNTIME_COMMANDS
    );

    let over_limit = program(
        vec![ProgramNode::Repeat {
            count: MAX_RUNTIME_COMMANDS as u32 + 1,
            body: vec![set_angle("baseYaw", 10.0, "a")],
            source_block_id: "r".into(),
        }],
        2,
    );
    assert!(matches!(
        expand_program_default(&over_limit),
        Err(SimError::CommandLimitExceeded { .. })
    ));
}

#[test]
fn empty_program_is_rejected() {
    assert!(matches!(
        expand_program_default(&program(vec![], 0)),
        Err(SimError::EmptyProgram)
    ));
}

#[test]
fn duration_uses_initial_angles_and_per_joint_speed() {
    let joints = default_joints();
    // baseYaw starts at -45° and runs at 60°/s; driving to +15° is 60° => 1000ms.
    let commands = expand_program_default(&program(
        vec![set_angle("baseYaw", 15.0, "a"), wait(250.0, "b")],
        2,
    ))
    .unwrap();

    approx(
        estimate_program_duration(&commands, &joints).unwrap(),
        1_250.0,
        1e-9,
    );
}

#[test]
fn duration_rejects_out_of_range_angles_and_negative_waits() {
    let joints = default_joints();

    let too_far = expand_program_default(&program(vec![set_angle("baseYaw", 999.0, "a")], 1)).unwrap();
    assert!(matches!(
        estimate_program_duration(&too_far, &joints),
        Err(SimError::AngleOutOfRange { .. })
    ));

    let negative = expand_program_default(&program(vec![wait(-1.0, "a")], 1)).unwrap();
    assert!(matches!(
        estimate_program_duration(&negative, &joints),
        Err(SimError::InvalidWait { .. })
    ));

    let unknown = expand_program_default(&program(vec![set_angle("nope", 0.0, "a")], 1)).unwrap();
    assert!(matches!(
        estimate_program_duration(&unknown, &joints),
        Err(SimError::UnknownJoint { .. })
    ));
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

#[test]
fn score_matches_the_typescript_formula() {
    let target: VoxelSet = [coord(0, 0, 0), coord(1, 0, 0)].into_iter().collect();
    let result = target.clone();
    let metrics = ProgramMetrics {
        source_block_count: 5,
        executed_command_count: 5,
        estimated_duration_ms: 5_645.0,
    };
    let scoring = default_scoring();

    let score = calculate_score(&target, &result, &metrics, &scoring).unwrap();

    // programCost = 5 + 0.25*5 = 6.25, which is exactly referenceProgramCost.
    approx(score.program_cost, 6.25, 1e-12);
    approx(score.completion_score, 100.0, 1e-12);
    approx(score.efficiency_score, 100.0, 1e-12);
    approx(score.time_score, 100.0, 1e-12);
    approx(score.final_score, 100.0, 1e-12);
}

#[test]
fn scores_are_clamped_to_100() {
    let target: VoxelSet = [coord(0, 0, 0)].into_iter().collect();
    let metrics = ProgramMetrics {
        source_block_count: 1,
        executed_command_count: 0,
        estimated_duration_ms: 1.0,
    };
    // A one-block program is far cheaper and faster than the references, so both
    // ratios exceed 100 and must clamp rather than overflow the final score.
    let score = calculate_score(&target, &target, &metrics, &default_scoring()).unwrap();
    assert_eq!(score.efficiency_score, 100.0);
    assert_eq!(score.time_score, 100.0);
    assert_eq!(score.final_score, 100.0);
}

#[test]
fn zero_cost_and_zero_duration_score_full_marks() {
    let empty = VoxelSet::new();
    let metrics = ProgramMetrics {
        source_block_count: 0,
        executed_command_count: 0,
        estimated_duration_ms: 0.0,
    };
    let score = calculate_score(&empty, &empty, &metrics, &default_scoring()).unwrap();
    assert_eq!(score.efficiency_score, 100.0);
    assert_eq!(score.time_score, 100.0);
}

#[test]
fn weights_must_sum_to_one() {
    let mut scoring = default_scoring();
    scoring.weights.completion = 0.9;
    assert!(matches!(
        validate_scoring_config(&scoring),
        Err(SimError::InvalidScoring(_))
    ));

    let mut negative = default_scoring();
    negative.weights.time = -0.15;
    negative.weights.completion = 0.9;
    assert!(matches!(
        validate_scoring_config(&negative),
        Err(SimError::InvalidScoring(_))
    ));
}

// ---------------------------------------------------------------------------
// Head safety
// ---------------------------------------------------------------------------

#[test]
fn arm_far_from_the_head_does_not_collide() {
    let config = RobotConfig {
        joints: zeroed_joints(),
        geometry: default_geometry(),
    };
    // All-zero angles put the tool at [2.3, 0.4, 0]; the head is at [1.35, 1.42, 0]
    // with semi-axes under 0.9, so nothing is in contact.
    let pose = compute_robot_pose(&config, &JointAngles::initial(&config)).unwrap();
    assert!(find_robot_head_collision(&pose, &default_voxel_config(), &config.geometry).is_none());
}

#[test]
fn a_segment_through_the_head_centre_collides() {
    let voxel_config = default_voxel_config();
    assert!(segment_intersects_expanded_ellipsoid_helper(
        [voxel_config.head_center[0] - 2.0, voxel_config.head_center[1], voxel_config.head_center[2]],
        [voxel_config.head_center[0] + 2.0, voxel_config.head_center[1], voxel_config.head_center[2]],
        &voxel_config,
    ));

    // Well above the head, clear of the ellipsoid plus expansion.
    assert!(!segment_intersects_expanded_ellipsoid_helper(
        [voxel_config.head_center[0] - 2.0, voxel_config.head_center[1] + 5.0, voxel_config.head_center[2]],
        [voxel_config.head_center[0] + 2.0, voxel_config.head_center[1] + 5.0, voxel_config.head_center[2]],
        &voxel_config,
    ));
}

fn segment_intersects_expanded_ellipsoid_helper(
    start: Vec3,
    end: Vec3,
    voxel_config: &VoxelConfig,
) -> bool {
    hcr_sim::collision::segment_intersects_expanded_ellipsoid(
        start,
        end,
        voxel_config.head_center,
        voxel_config.head_scale,
        0.0,
    )
}

// ---------------------------------------------------------------------------
// Replay
// ---------------------------------------------------------------------------

/// A motion that stays inside the head-safe yaw band.
///
/// With the other joints at their shipped initial angles the arm is only clear of
/// the head for `baseYaw` in [-60, -35] and [35, 60]; sweeping toward 0° drives
/// the elbow into it at roughly -30°. See `examples/probe.rs`.
fn safe_sweep() -> Program {
    program(vec![set_angle("baseYaw", -35.0, "a")], 1)
}

#[test]
fn replay_is_deterministic_across_runs() {
    let c = challenge(default_joints(), vec![coord(0, 0, 0)], vec![coord(0, 0, 0)]);
    let p = safe_sweep();

    let first = replay(&c, &p, ReplayOptions::default()).unwrap();
    let second = replay(&c, &p, ReplayOptions::default()).unwrap();

    assert_eq!(first.result_voxels_hash, second.result_voxels_hash);
    assert_eq!(first.score, second.score);
    assert_eq!(first.metrics.executed_command_count, second.metrics.executed_command_count);
}

#[test]
fn replay_reports_simulated_time_matching_the_estimate() {
    let c = challenge(default_joints(), vec![], vec![]);
    // baseYaw -45° -> -60° at 60°/s is 15°, exactly 250ms, and stays head-safe.
    let p = program(vec![set_angle("baseYaw", -60.0, "a")], 1);

    let outcome = replay(&c, &p, ReplayOptions::default()).unwrap();

    assert_eq!(outcome.terminal.reason, TerminalReason::Completed);
    assert_eq!(outcome.metrics.executed_command_count, 1);
    approx(outcome.metrics.estimated_duration_ms, 250.0, 1e-9);
    approx(outcome.simulated_ms, 250.0, 1e-6);
}

#[test]
fn a_wait_only_program_consumes_exactly_its_duration() {
    let c = challenge(default_joints(), vec![], vec![]);
    let p = program(vec![wait(250.0, "a"), wait(250.0, "b")], 2);

    let outcome = replay(&c, &p, ReplayOptions::default()).unwrap();

    assert_eq!(outcome.terminal.reason, TerminalReason::Completed);
    assert_eq!(outcome.metrics.executed_command_count, 2);
    approx(outcome.simulated_ms, 500.0, 1e-9);
}

#[test]
fn driving_the_arm_into_the_head_stops_at_the_last_safe_pose() {
    // Sweeping baseYaw from its -45° start toward 0° swings the arm across the
    // head at [1.35, 1.42, 0]; the elbow joint reaches it near -30°.
    let c = challenge(default_joints(), vec![], vec![]);
    let p = program(vec![set_angle("baseYaw", 0.0, "a")], 1);

    let outcome = replay(&c, &p, ReplayOptions::default()).unwrap();

    assert_eq!(
        outcome.terminal.reason,
        TerminalReason::HeadCollision,
        "expected the arm to be stopped by the head constraint"
    );
    assert_eq!(outcome.terminal.joint_id.as_deref(), Some("baseYaw"));
    assert_eq!(outcome.terminal.part_label.as_deref(), Some("Elbow Joint"));
    assert_eq!(outcome.terminal.source_block_id.as_deref(), Some("a"));

    // The probe puts -35° safe and -30° in contact, so the boundary is between.
    let safe = outcome.terminal.safe_angle_deg.unwrap();
    assert!(
        (-35.0..-30.0).contains(&safe),
        "safe angle {safe} should lie between the last safe and first colliding probe samples"
    );

    // Stopped mid-command, so nothing completed.
    assert_eq!(outcome.metrics.executed_command_count, 0);
}

#[test]
fn collision_stop_angle_is_independent_of_tick_size() {
    // The headline determinism claim: sub-stepping is capped at 0.5° regardless
    // of tick size and then bisected, so the reported safe angle converges to the
    // same geometric boundary. See docs/backend/02-DETERMINISM.md §2.
    let c = challenge(default_joints(), vec![], vec![]);
    let p = program(vec![set_angle("baseYaw", 0.0, "a")], 1);

    let fine = replay(
        &c,
        &p,
        ReplayOptions {
            tick_ms: 1.0,
            ..Default::default()
        },
    )
    .unwrap();
    let coarse = replay(
        &c,
        &p,
        ReplayOptions {
            tick_ms: 100.0,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(fine.terminal.reason, TerminalReason::HeadCollision);
    assert_eq!(coarse.terminal.reason, TerminalReason::HeadCollision);
    assert_eq!(fine.terminal.joint_id, coarse.terminal.joint_id);
    approx(
        fine.terminal.safe_angle_deg.unwrap(),
        coarse.terminal.safe_angle_deg.unwrap(),
        1e-3,
    );
}

#[test]
fn oversized_programs_are_rejected_before_execution() {
    let c = challenge(default_joints(), vec![], vec![]);
    let p = program(
        vec![ProgramNode::Repeat {
            count: 20,
            body: vec![ProgramNode::Repeat {
                count: 20,
                body: vec![set_angle("baseYaw", 10.0, "a"), set_angle("baseYaw", -10.0, "b")],
                source_block_id: "inner".into(),
            }],
            source_block_id: "outer".into(),
        }],
        4,
    );

    // 20 * 20 * 2 = 800 commands, over the 500 cap.
    assert!(matches!(
        replay(&c, &p, ReplayOptions::default()),
        Err(SimError::CommandLimitExceeded { .. })
    ));
}

#[test]
fn the_tool_removes_hair_it_sweeps_through() {
    // The tool passes through lattice cell (0,-5,7) partway through the safe
    // sweep from -45° to -35° — about 0.11 from the cell centre, well inside the
    // 0.20 test expansion.
    let on_path = coord(0, -5, 7);
    let c = challenge(default_joints(), vec![on_path], vec![]);

    let outcome = replay(&c, &safe_sweep(), ReplayOptions::default()).unwrap();

    assert_eq!(outcome.terminal.reason, TerminalReason::Completed);

    assert!(
        outcome.remaining_voxels.is_empty(),
        "the swept voxel should have been removed; remaining: {:?}",
        outcome.remaining_voxels
    );
    // Target is empty and the result is now empty too, so completion is 100.
    approx(outcome.score.completion_score, 100.0, 1e-12);
}

#[test]
fn hair_outside_the_tool_path_survives() {
    // A voxel far from the arm is untouched, and completion reflects the miss.
    let far = coord(50, 50, 50);
    let c = challenge(default_joints(), vec![far], vec![]);

    let outcome = replay(&c, &safe_sweep(), ReplayOptions::default()).unwrap();

    assert_eq!(outcome.remaining_voxels, BTreeSet::from([far]));
    // Target empty, result non-empty => IoU 0.
    approx(outcome.score.completion_score, 0.0, 1e-12);
}
