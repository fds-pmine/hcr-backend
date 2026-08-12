//! Fixtures shared by the service test suites.

use hcr_contract::*;

fn joint(id: &str, axis: Axis, min: f64, max: f64, initial: f64, speed: f64) -> JointConfig {
    JointConfig {
        id: id.into(),
        name: id.into(),
        axis,
        min_angle_deg: min,
        max_angle_deg: max,
        initial_angle_deg: initial,
        speed_deg_per_sec: speed,
        // Geometric angles: these fixtures exercise the engine, not the
        // hardware mapping.
        servo: None,
    }
}

/// A challenge using the shipped geometry, with a single hair voxel sitting on
/// the tool's path so replay is quick and the score is predictable.
pub fn challenge(id: &str, version: u32, difficulty: f64) -> ChallengeDefinitionDto {
    ChallengeDefinitionDto {
        challenge: ChallengeDefinition {
            id: id.into(),
            name: format!("Challenge {id}"),
            description: "test".into(),
            robot_config: RobotConfig {
                joints: vec![
                    joint("baseYaw", Axis::Y, -60.0, 60.0, -45.0, 60.0),
                    joint("shoulderRoll", Axis::X, -45.0, 45.0, 0.0, 45.0),
                    joint("shoulder", Axis::Z, -20.0, 100.0, 45.0, 45.0),
                    joint("elbow", Axis::Z, -135.0, 10.0, -80.0, 60.0),
                    joint("wrist", Axis::Z, -100.0, 100.0, 35.0, 75.0),
                ],
                geometry: RobotGeometryConfig {
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
                },
            },
            voxel_config: VoxelConfig {
                origin: [1.35, 1.5, 0.0],
                size: 0.16,
                head_center: [1.35, 1.42, 0.0],
                head_scale: [0.68, 0.86, 0.68],
            },
            initial_hair: HairstyleDefinition {
                id: "i".into(),
                name: "i".into(),
                voxels: vec![VoxelCoord { x: 0, y: -5, z: 7 }],
            },
            target_hair: HairstyleDefinition {
                id: "t".into(),
                name: "t".into(),
                voxels: vec![],
            },
            allowed_blocks: vec![
                AllowedBlockType::SetJointAngle,
                AllowedBlockType::Wait,
                AllowedBlockType::Repeat,
            ],
            starter_workspace: None,
            scoring: ScoringConfig {
                weights: ScoreWeights {
                    completion: 0.6,
                    efficiency: 0.25,
                    time: 0.15,
                },
                reference_program_cost: 6.25,
                reference_time_ms: 5_645.0,
                command_weight: 0.25,
            },
        },
        meta: ChallengeMeta {
            version,
            irt: ItemParameters {
                discrimination: 1.2,
                difficulty,
                guessing: 0.0,
            },
            calibration: CalibrationState::Calibrated,
            response_count: 250,
            dimensions: vec![SkillDimension::Kinematics],
            mastery_threshold: 0.5,
            generator: None,
            hardware_compatible: true,
        },
    }
}

/// Stays inside the head-safe yaw band and sweeps the hair voxel.
pub fn safe_program() -> Program {
    Program {
        nodes: vec![ProgramNode::SetJointAngle {
            joint_id: "baseYaw".into(),
            angle_deg: -35.0,
            source_block_id: "a".into(),
        }],
        source_block_count: 1,
    }
}

/// Drives the arm into the head.
///
/// Each integration test binary compiles this module separately, so a fixture
/// only some of them use reads as dead code in the others.
#[allow(dead_code)]
pub fn colliding_program() -> Program {
    Program {
        nodes: vec![ProgramNode::SetJointAngle {
            joint_id: "baseYaw".into(),
            angle_deg: 0.0,
            source_block_id: "a".into(),
        }],
        source_block_count: 1,
    }
}

/// A submission request.
pub fn submission(
    id: &str,
    challenge_id: &str,
    version: u32,
    program: Program,
) -> SubmissionCreate {
    SubmissionCreate {
        submission_id: id.into(),
        challenge_id: challenge_id.into(),
        challenge_version: version,
        program,
        session_id: None,
        match_id: None,
        client_preview: None,
    }
}
