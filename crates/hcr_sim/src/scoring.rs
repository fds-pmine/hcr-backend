//! Score computation.
//!
//! Ports `src/features/scoring/scoring.ts`.

use hcr_contract::{ProgramMetrics, ScoreResult, ScoringConfig};

use crate::error::SimError;
use crate::voxel::{VoxelSet, calculate_trim_score};

const SCORE_MAX: f64 = 100.0;
const WEIGHT_TOLERANCE: f64 = 1e-6;

/// Compute the four scores from a finished run.
///
/// # Errors
/// Returns [`SimError::InvalidScoring`] if the challenge's scoring block is
/// malformed — weights that do not sum to 1, or non-positive references.
pub fn calculate_score(
    initial_voxels: &VoxelSet,
    target_voxels: &VoxelSet,
    result_voxels: &VoxelSet,
    metrics: &ProgramMetrics,
    scoring: &ScoringConfig,
) -> Result<ScoreResult, SimError> {
    validate_scoring_config(scoring)?;

    let completion_score =
        clamp_score(calculate_trim_score(initial_voxels, target_voxels, result_voxels));

    let program_cost = f64::from(metrics.source_block_count)
        + scoring.command_weight * f64::from(metrics.executed_command_count);

    let efficiency_score = if program_cost == 0.0 {
        SCORE_MAX
    } else {
        clamp_score((scoring.reference_program_cost / program_cost) * SCORE_MAX)
    };

    let time_score = if metrics.estimated_duration_ms == 0.0 {
        SCORE_MAX
    } else {
        clamp_score((scoring.reference_time_ms / metrics.estimated_duration_ms) * SCORE_MAX)
    };

    let final_score = clamp_score(
        scoring.weights.completion * completion_score
            + scoring.weights.efficiency * efficiency_score
            + scoring.weights.time * time_score,
    );

    Ok(ScoreResult {
        completion_score,
        efficiency_score,
        time_score,
        final_score,
        program_cost,
    })
}

/// Reject a scoring block the engine cannot use.
pub fn validate_scoring_config(config: &ScoringConfig) -> Result<(), SimError> {
    let weights = [
        config.weights.completion,
        config.weights.efficiency,
        config.weights.time,
    ];

    if weights.iter().any(|w| !w.is_finite() || *w < 0.0) {
        return Err(SimError::InvalidScoring(
            "Score weights must be finite non-negative numbers.",
        ));
    }

    let total: f64 = weights.iter().sum();
    if (total - 1.0).abs() > WEIGHT_TOLERANCE {
        return Err(SimError::InvalidScoring("Score weights must sum to 1."));
    }

    if !config.reference_program_cost.is_finite() || config.reference_program_cost <= 0.0 {
        return Err(SimError::InvalidScoring(
            "Reference program cost must be greater than 0.",
        ));
    }
    if !config.reference_time_ms.is_finite() || config.reference_time_ms <= 0.0 {
        return Err(SimError::InvalidScoring(
            "Reference time must be greater than 0.",
        ));
    }
    if !config.command_weight.is_finite() || config.command_weight < 0.0 {
        return Err(SimError::InvalidScoring(
            "Command weight must be a finite non-negative number.",
        ));
    }

    Ok(())
}

fn clamp_score(score: f64) -> f64 {
    score.clamp(0.0, SCORE_MAX)
}
