//! Conformance against the TypeScript engine.
//!
//! Vectors are produced by running the **real** `SimulationEngine` from the
//! frontend (`hcr-backend/tools/generate-vectors.ts`) and recording what it did.
//! This test asserts the Rust port agrees. The TypeScript engine is the incumbent
//! definition of correct; nothing here encodes an independent expectation.
//!
//! Regenerate with:
//!   npx vitest run --config hcr-backend/tools/vectors.config.ts

use std::collections::BTreeSet;
use std::path::PathBuf;

use hcr_contract::{ChallengeDefinition, Program, ProgramMetrics, ScoreResult, TerminalReason};
use hcr_sim::{ReplayOptions, coord_to_key, replay};
use serde::Deserialize;

/// Voxel-set agreement threshold.
///
/// Exact equality is deliberately NOT required: `computeRobotPose` uses
/// `sin`/`cos`, and IEEE-754 does not mandate correctly-rounded transcendentals,
/// so V8 and Rust's libm may differ in the last ULP and flip a voxel sitting
/// exactly on an AABB boundary. See `docs/backend/02-DETERMINISM.md` §4.
const MAX_JACCARD_DISTANCE: f64 = 0.01;

/// Score components are pure arithmetic once the voxel set is fixed, so they
/// should agree far more tightly than the geometry does.
const SCORE_TOLERANCE: f64 = 1e-9;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Vectors {
    tick_ms: f64,
    challenge: ChallengeDefinition,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    id: String,
    note: String,
    program: Program,
    expect: Expectation,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Expectation {
    status: String,
    error_message: Option<String>,
    metrics: ProgramMetrics,
    score: Option<ScoreResult>,
    remaining_voxel_count: usize,
    result_voxels_hash: String,
    remaining_voxels: Vec<String>,
}

fn vectors_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/vectors.json")
}

fn jaccard_distance(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    let union = a.union(b).count();
    if union == 0 {
        return 0.0;
    }
    let intersection = a.intersection(b).count();
    (union - intersection) as f64 / union as f64
}

/// Pull the safe angle out of the TypeScript error message, which is the only
/// place it surfaces: `"… stopped at safe angle -34.48°; …"`.
fn parse_safe_angle(message: &str) -> Option<f64> {
    let tail = message.split("safe angle ").nth(1)?;
    let value = tail.split('°').next()?;
    value.trim().parse().ok()
}

#[test]
fn rust_engine_matches_the_typescript_engine() {
    let path = vectors_path();
    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(error) => panic!(
            "conformance vectors missing at {}: {error}\n\
             Regenerate with:\n  \
             npx vitest run --config hcr-backend/tools/vectors.config.ts",
            path.display()
        ),
    };

    let vectors: Vectors = serde_json::from_str(&raw).expect("vectors.json should deserialize");
    assert!(!vectors.cases.is_empty(), "no vectors to check");

    let options = ReplayOptions {
        tick_ms: vectors.tick_ms,
        ..Default::default()
    };

    let mut failures: Vec<String> = Vec::new();
    let mut exact_hash_matches = 0usize;

    for case in &vectors.cases {
        let outcome = match replay(&vectors.challenge, &case.program, options) {
            Ok(outcome) => outcome,
            Err(error) => {
                failures.push(format!("[{}] replay failed: {error}", case.id));
                continue;
            }
        };

        let mut problems: Vec<String> = Vec::new();

        // --- terminal state -------------------------------------------------
        let expected_reason = match case.expect.status.as_str() {
            "completed" => TerminalReason::Completed,
            "error" => TerminalReason::HeadCollision,
            other => {
                problems.push(format!("unhandled TS status {other:?}"));
                TerminalReason::Invalid
            }
        };
        if outcome.terminal.reason != expected_reason {
            problems.push(format!(
                "terminal reason: rust {:?} vs ts {:?} ({})",
                outcome.terminal.reason, expected_reason, case.expect.status
            ));
        }

        // --- collision attribution -----------------------------------------
        if let Some(message) = &case.expect.error_message {
            if let Some(expected_angle) = parse_safe_angle(message) {
                match outcome.terminal.safe_angle_deg {
                    Some(actual) if (actual - expected_angle).abs() <= 0.01 => {}
                    Some(actual) => problems.push(format!(
                        "safe angle: rust {actual:.4} vs ts {expected_angle:.2}"
                    )),
                    None => problems.push("rust reported no safe angle".into()),
                }
            }
            if let Some(part) = &outcome.terminal.part_label {
                if !message.starts_with(part.as_str()) {
                    problems.push(format!(
                        "collision part: rust {part:?} not named first in ts message {message:?}"
                    ));
                }
            }
        }

        // --- metrics --------------------------------------------------------
        if outcome.metrics.executed_command_count != case.expect.metrics.executed_command_count {
            problems.push(format!(
                "executedCommandCount: rust {} vs ts {}",
                outcome.metrics.executed_command_count, case.expect.metrics.executed_command_count
            ));
        }
        if (outcome.metrics.estimated_duration_ms - case.expect.metrics.estimated_duration_ms).abs()
            > 1e-6
        {
            problems.push(format!(
                "estimatedDurationMs: rust {} vs ts {}",
                outcome.metrics.estimated_duration_ms, case.expect.metrics.estimated_duration_ms
            ));
        }

        // --- voxels ---------------------------------------------------------
        let rust_keys: BTreeSet<String> =
            outcome.remaining_voxels.iter().map(coord_to_key).collect();
        let ts_keys: BTreeSet<String> = case.expect.remaining_voxels.iter().cloned().collect();

        assert_eq!(
            ts_keys.len(),
            case.expect.remaining_voxel_count,
            "[{}] vector file is internally inconsistent",
            case.id
        );

        let distance = jaccard_distance(&rust_keys, &ts_keys);
        if distance > MAX_JACCARD_DISTANCE {
            let only_rust: Vec<_> = rust_keys.difference(&ts_keys).take(5).collect();
            let only_ts: Vec<_> = ts_keys.difference(&rust_keys).take(5).collect();
            problems.push(format!(
                "voxel divergence {distance:.5} > {MAX_JACCARD_DISTANCE} \
                 (rust {} vs ts {} voxels; only-rust {only_rust:?}, only-ts {only_ts:?})",
                rust_keys.len(),
                ts_keys.len()
            ));
        }
        if outcome.result_voxels_hash == case.expect.result_voxels_hash {
            exact_hash_matches += 1;
        }

        // --- score ----------------------------------------------------------
        match (&case.expect.score, expected_reason) {
            (Some(expected), _) => {
                let actual = outcome.score;
                for (name, a, b) in [
                    (
                        "completionScore",
                        actual.completion_score,
                        expected.completion_score,
                    ),
                    (
                        "efficiencyScore",
                        actual.efficiency_score,
                        expected.efficiency_score,
                    ),
                    ("timeScore", actual.time_score, expected.time_score),
                    ("finalScore", actual.final_score, expected.final_score),
                    ("programCost", actual.program_cost, expected.program_cost),
                ] {
                    if (a - b).abs() > SCORE_TOLERANCE {
                        problems.push(format!("{name}: rust {a} vs ts {b}"));
                    }
                }
            }
            // The TypeScript engine withholds a score when a run ends in `error`;
            // the Rust engine always computes one. That is intentional — the
            // backend needs a number for a halted run — so a missing TS score is
            // not a mismatch, and the voxel comparison above still applies.
            (None, TerminalReason::HeadCollision) => {}
            (None, other) => problems.push(format!("ts produced no score for terminal {other:?}")),
        }

        if !problems.is_empty() {
            failures.push(format!(
                "[{}] {}\n    - {}",
                case.id,
                case.note,
                problems.join("\n    - ")
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of {} conformance vectors disagreed:\n\n{}",
        failures.len(),
        vectors.cases.len(),
        failures.join("\n")
    );

    eprintln!(
        "conformance: {} vectors matched; {}/{} also agreed on the exact voxel hash",
        vectors.cases.len(),
        exact_hash_matches,
        vectors.cases.len()
    );
}
