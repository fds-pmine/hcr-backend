//! Sequential execution of an expanded command list.
//!
//! Ports `src/features/simulation/programExecutor.ts`.
//!
//! Deviation from the TypeScript: movements are *returned* rather than delivered
//! through an `onMovement` callback. Rust's borrow rules make a callback that
//! mutates the voxel set while the executor holds the controller awkward, and
//! deferring removal to the end of the tick is provably equivalent — removal only
//! shrinks the set, so re-removing an already-removed voxel is a no-op.

use alloc::vec::Vec;
use hcr_contract::{RobotCommand, Vec3};

use crate::controller::{BlockedHeadCollision, RobotController};
use crate::error::SimError;

/// One end-effector displacement to sweep against the hair voxels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Movement {
    /// Position at the start of the displacement.
    pub start: Vec3,
    /// Position at the end.
    pub end: Vec3,
}

/// Result of advancing execution by one tick.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExecutorAdvanceResult {
    /// Simulated milliseconds consumed.
    pub consumed_ms: f64,
    /// Commands that finished during this tick.
    pub commands_completed: u32,
    /// Whether every command has now run.
    pub program_completed: bool,
    /// Set when the head constraint halted execution.
    pub blocked_collision: Option<BlockedHeadCollision>,
    /// Displacements produced during this tick, in order.
    pub movements: Vec<Movement>,
}

#[derive(Debug, Clone)]
struct ActiveWait {
    elapsed_ms: f64,
}

/// Walks a command list, driving the controller.
pub struct ProgramExecutor {
    commands: Vec<RobotCommand>,
    command_index: usize,
    has_active_command: bool,
    active_wait: Option<ActiveWait>,
}

impl ProgramExecutor {
    /// Create an executor over `commands`.
    pub fn new(commands: Vec<RobotCommand>) -> Self {
        Self {
            commands,
            command_index: 0,
            has_active_command: false,
            active_wait: None,
        }
    }

    /// Whether every command has run.
    pub fn is_complete(&self) -> bool {
        self.command_index >= self.commands.len()
    }

    /// Index of the next command to run.
    pub fn command_index(&self) -> usize {
        self.command_index
    }

    /// The command currently executing, or the next one up.
    pub fn current_command(&self) -> Option<&RobotCommand> {
        self.commands.get(self.command_index)
    }

    /// Advance execution by `delta_ms`.
    pub fn advance(
        &mut self,
        delta_ms: f64,
        controller: &mut RobotController<'_>,
    ) -> Result<ExecutorAdvanceResult, SimError> {
        if !delta_ms.is_finite() || delta_ms < 0.0 {
            return Err(SimError::Internal(
                "Delta must be a finite non-negative number.",
            ));
        }

        let mut remaining_ms = delta_ms;
        let mut result = ExecutorAdvanceResult::default();
        let mut safety_counter = 0usize;

        while self.command_index < self.commands.len() {
            safety_counter += 1;
            if safety_counter > self.commands.len() + 1 {
                return Err(SimError::Internal("Program executor failed to make progress."));
            }

            let command = self.commands[self.command_index].clone();

            if !self.has_active_command {
                self.has_active_command = true;
                match &command {
                    RobotCommand::SetJointAngle {
                        joint_id,
                        angle_deg,
                        ..
                    } => controller.begin_move(joint_id, *angle_deg)?,
                    RobotCommand::Wait { .. } => {
                        self.active_wait = Some(ActiveWait { elapsed_ms: 0.0 });
                    }
                }
            }

            let command_completed;
            let command_consumed_ms;

            match &command {
                RobotCommand::SetJointAngle { .. } => {
                    let movement = controller.advance_move(remaining_ms)?;
                    command_consumed_ms = movement.consumed_ms;
                    command_completed = movement.completed;
                    if movement.moved {
                        result.movements.push(Movement {
                            start: movement.previous_end_effector,
                            end: movement.current_end_effector,
                        });
                    }
                    if movement.blocked_collision.is_some() {
                        result.blocked_collision = movement.blocked_collision;
                    }
                }
                RobotCommand::Wait { duration_ms, .. } => {
                    let wait = self
                        .active_wait
                        .as_mut()
                        .ok_or(SimError::Internal("Wait command state is missing."))?;
                    let wait_remaining = (duration_ms - wait.elapsed_ms).max(0.0);
                    command_consumed_ms = remaining_ms.min(wait_remaining);
                    wait.elapsed_ms += command_consumed_ms;
                    command_completed = wait.elapsed_ms >= *duration_ms;
                }
            }

            result.consumed_ms += command_consumed_ms;
            remaining_ms = (remaining_ms - command_consumed_ms).max(0.0);

            if result.blocked_collision.is_some() {
                break;
            }
            if !command_completed {
                break;
            }

            self.command_index += 1;
            result.commands_completed += 1;
            self.has_active_command = false;
            self.active_wait = None;

            if remaining_ms == 0.0 && self.next_command_requires_time(controller) {
                break;
            }
        }

        result.program_completed = self.command_index >= self.commands.len();
        Ok(result)
    }

    /// Whether the next command would need simulated time to make progress.
    ///
    /// Zero-cost commands (a zero wait, or a move to the angle already held) are
    /// allowed to run within the same tick; anything else waits for the next one.
    fn next_command_requires_time(&self, controller: &RobotController<'_>) -> bool {
        match self.commands.get(self.command_index) {
            None => false,
            Some(RobotCommand::Wait { duration_ms, .. }) => *duration_ms > 0.0,
            Some(RobotCommand::SetJointAngle {
                joint_id,
                angle_deg,
                ..
            }) => controller
                .angles()
                .get(joint_id)
                .is_none_or(|current| current != *angle_deg),
        }
    }
}
