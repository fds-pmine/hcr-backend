//! Authoritative replay.
//!
//! Runs a submitted program against a challenge on a fixed tick and produces the
//! score of record. The browser's own run is a preview only — the client is
//! untrusted and editable (`docs/backend/02-DETERMINISM.md` §3).

use alloc::string::{String, ToString};
use hcr_contract::{
    ChallengeDefinition, MAX_RUNTIME_COMMANDS, Program, ProgramMetrics, SIM_TICK_MS, ScoreResult,
    Terminal, TerminalReason,
};

use crate::error::SimError;
use crate::executor::ProgramExecutor;
use crate::controller::RobotController;
use crate::program::{estimate_program_duration, expand_program};
use crate::scoring::calculate_score;
use crate::voxel::{VoxelSet, find_swept_voxel_hits, result_voxels_hash};

/// Tunables for one replay.
#[derive(Debug, Clone, Copy)]
pub struct ReplayOptions {
    /// Simulated milliseconds per tick.
    pub tick_ms: f64,
    /// Hard ceiling on ticks, guarding against pathological programs.
    pub max_ticks: u64,
    /// Atomic-command cap applied during expansion.
    pub command_limit: usize,
}

impl Default for ReplayOptions {
    fn default() -> Self {
        Self {
            tick_ms: SIM_TICK_MS,
            // 20 minutes of simulated time at the canonical tick: far beyond any
            // legitimate program, cheap enough to never be hit by accident.
            max_ticks: 240_000,
            command_limit: MAX_RUNTIME_COMMANDS,
        }
    }
}

/// Everything a replay produces.
#[derive(Debug, Clone)]
pub struct ReplayOutcome {
    /// Authoritative score.
    pub score: ScoreResult,
    /// Program size and timing.
    pub metrics: ProgramMetrics,
    /// Hair left standing when the run ended.
    pub remaining_voxels: VoxelSet,
    /// Canonical hash of `remaining_voxels`.
    pub result_voxels_hash: String,
    /// Why the run stopped, with attribution when it failed.
    pub terminal: Terminal,
    /// Simulated milliseconds consumed.
    pub simulated_ms: f64,
}

/// Replay `program` against `challenge`.
///
/// # Errors
/// Returns [`SimError`] for programs that fail validation — unknown joints,
/// out-of-range angles, oversized expansions, empty programs — and for malformed
/// scoring configuration. A *head collision* is not an error: it is a legitimate
/// terminal state that still produces a score, matching the frontend, which stops
/// at the last safe pose and enters `error` rather than silently correcting.
pub fn replay(
    challenge: &ChallengeDefinition,
    program: &Program,
    options: ReplayOptions,
) -> Result<ReplayOutcome, SimError> {
    let commands = expand_program(program, options.command_limit)?;
    let estimated_duration_ms =
        estimate_program_duration(&commands, &challenge.robot_config.joints)?;

    // Kept alongside the mutable set: completion is scored on the cut, so the
    // starting hairstyle has to survive the run that carves it.
    let initial: VoxelSet = challenge.initial_hair.voxels.iter().copied().collect();
    let mut hair: VoxelSet = initial.clone();
    let target: VoxelSet = challenge.target_hair.voxels.iter().copied().collect();

    let mut controller =
        RobotController::new(&challenge.robot_config, Some(&challenge.voxel_config));
    let mut executor = ProgramExecutor::new(commands);

    let mut executed_command_count: u32 = 0;
    let mut simulated_ms = 0.0_f64;
    let mut terminal = Terminal::completed();
    let mut ticks: u64 = 0;

    while !executor.is_complete() {
        ticks += 1;
        if ticks > options.max_ticks {
            terminal = Terminal {
                reason: TerminalReason::Timeout,
                joint_id: None,
                safe_angle_deg: None,
                source_block_id: executor
                    .current_command()
                    .map(|c| c.source_block_id().to_string()),
                part_label: None,
            };
            break;
        }

        // Capture the source block before advancing: on collision the executor's
        // cursor still points at the offending command, but reading it after the
        // borrow ends keeps the borrow checker happy and the attribution correct.
        let pending_block_id = executor
            .current_command()
            .map(|c| c.source_block_id().to_string());

        let result = executor.advance(options.tick_ms, &mut controller)?;

        simulated_ms += result.consumed_ms;
        executed_command_count += result.commands_completed;

        for movement in &result.movements {
            let hits = find_swept_voxel_hits(
                movement.start,
                movement.end,
                &hair,
                &challenge.voxel_config,
                challenge.robot_config.geometry.tool_radius,
            );
            for hit in hits {
                hair.remove(&hit);
            }
        }

        if let Some(blocked) = result.blocked_collision {
            terminal = Terminal {
                reason: TerminalReason::HeadCollision,
                joint_id: Some(blocked.joint_id),
                safe_angle_deg: Some(blocked.safe_angle_deg),
                source_block_id: pending_block_id,
                part_label: Some(blocked.part.label().to_string()),
            };
            break;
        }

        if result.program_completed {
            break;
        }
    }

    let metrics = ProgramMetrics {
        source_block_count: program.source_block_count,
        executed_command_count,
        estimated_duration_ms,
    };

    let score = calculate_score(&initial, &target, &hair, &metrics, &challenge.scoring)?;
    let result_voxels_hash = result_voxels_hash(&hair);

    Ok(ReplayOutcome {
        score,
        metrics,
        remaining_voxels: hair,
        result_voxels_hash,
        terminal,
        simulated_ms,
    })
}
