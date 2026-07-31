//! Program IR expansion and duration estimation.
//!
//! Ports the runtime-relevant half of
//! `src/features/blockly/programCompiler.ts` plus `estimateProgramDuration`.
//! Blockly parsing itself stays in the frontend; the server only ever receives
//! already-compiled IR.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use hcr_contract::{JointConfig, MAX_RUNTIME_COMMANDS, Program, ProgramNode, RobotCommand};

use crate::error::SimError;

/// Flatten a program's `repeat` nodes into a linear command list.
///
/// The server always does this itself rather than trusting a client-supplied
/// expansion (`docs/backend/README.md` decision D3), which is what makes the
/// command cap meaningful.
///
/// # Errors
/// Returns [`SimError::CommandLimitExceeded`] once the expansion passes `limit`,
/// and [`SimError::EmptyProgram`] if nothing executable results.
pub fn expand_program(program: &Program, limit: usize) -> Result<Vec<RobotCommand>, SimError> {
    let mut commands = Vec::new();
    append_nodes(&program.nodes, limit, &mut commands)?;

    if commands.is_empty() {
        return Err(SimError::EmptyProgram);
    }

    Ok(commands)
}

/// Expand using the contract's standard cap.
pub fn expand_program_default(program: &Program) -> Result<Vec<RobotCommand>, SimError> {
    expand_program(program, MAX_RUNTIME_COMMANDS)
}

fn append_nodes(
    nodes: &[ProgramNode],
    limit: usize,
    commands: &mut Vec<RobotCommand>,
) -> Result<(), SimError> {
    for node in nodes {
        match node {
            ProgramNode::Repeat { count, body, .. } => {
                for _ in 0..*count {
                    append_nodes(body, limit, commands)?;
                }
            }
            ProgramNode::SetJointAngle {
                joint_id,
                angle_deg,
                source_block_id,
            } => {
                commands.push(RobotCommand::SetJointAngle {
                    joint_id: joint_id.clone(),
                    angle_deg: *angle_deg,
                    source_block_id: source_block_id.clone(),
                });
                // Matches the TypeScript exactly: push first, then compare, so a
                // program of exactly `limit` commands is accepted.
                if commands.len() > limit {
                    return Err(SimError::CommandLimitExceeded {
                        limit,
                        source_block_id: source_block_id.clone(),
                    });
                }
            }
            ProgramNode::Wait {
                duration_ms,
                source_block_id,
            } => {
                commands.push(RobotCommand::Wait {
                    duration_ms: *duration_ms,
                    source_block_id: source_block_id.clone(),
                });
                if commands.len() > limit {
                    return Err(SimError::CommandLimitExceeded {
                        limit,
                        source_block_id: source_block_id.clone(),
                    });
                }
            }
        }
    }
    Ok(())
}

/// Estimated wall-clock duration of an expanded command list.
///
/// This is a *static* property of the program — it never depends on how fast the
/// simulation actually ran — which is what makes the Time score hardware
/// independent (`docs/backend/02-DETERMINISM.md` §8).
///
/// # Errors
/// Mirrors the TypeScript validation: unknown joints, out-of-range angles and
/// negative waits are rejected here rather than at execution time.
pub fn estimate_program_duration(
    commands: &[RobotCommand],
    joints: &[JointConfig],
) -> Result<f64, SimError> {
    let config_by_id: BTreeMap<&str, &JointConfig> =
        joints.iter().map(|j| (j.id.as_str(), j)).collect();
    let mut angles: BTreeMap<&str, f64> = joints
        .iter()
        .map(|j| (j.id.as_str(), j.initial_angle_deg))
        .collect();

    let mut duration_ms = 0.0_f64;

    for command in commands {
        match command {
            RobotCommand::Wait { duration_ms: d, .. } => {
                if !d.is_finite() || *d < 0.0 {
                    return Err(SimError::InvalidWait { duration_ms: *d });
                }
                duration_ms += d;
            }
            RobotCommand::SetJointAngle {
                joint_id,
                angle_deg,
                ..
            } => {
                let config = config_by_id.get(joint_id.as_str()).ok_or_else(|| {
                    SimError::UnknownJoint {
                        joint_id: joint_id.clone(),
                    }
                })?;

                if !angle_deg.is_finite()
                    || *angle_deg < config.min_angle_deg
                    || *angle_deg > config.max_angle_deg
                {
                    return Err(SimError::AngleOutOfRange {
                        joint_id: joint_id.clone(),
                        angle_deg: *angle_deg,
                        min_angle_deg: config.min_angle_deg,
                        max_angle_deg: config.max_angle_deg,
                    });
                }

                let current = angles
                    .get(joint_id.as_str())
                    .copied()
                    .ok_or_else(|| SimError::UnknownJoint {
                        joint_id: joint_id.clone(),
                    })?;

                duration_ms += ((angle_deg - current).abs() / config.speed_deg_per_sec) * 1000.0;
                angles.insert(config.id.as_str(), *angle_deg);
            }
        }
    }

    Ok(duration_ms)
}
