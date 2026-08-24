//! Cross-language acceptance for the compact Cutter Grid V4 planner.
//!
//! The Profile and the compact PTP summary are frontend-generated, checked-in
//! certification assets. Rust consumes the same Profile at service startup, so
//! this test catches semantic drift before a server plan can reach Practice.

#![cfg(feature = "planner")]

use std::collections::BTreeSet;

use hcr_contract::{
    CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION, ChallengeDefinition, CutterGridDirection,
    CutterGridNode, CutterGridProfileV4, CutterGridProgramV4, CutterGridTrajectoryActionV4,
    ProgramMetrics,
};
use hcr_sim::{
    VoxelSet, calculate_score, compile_cutter_grid_program_v4, coord_to_key, key_to_coord,
    plan_cutter_grid_v4,
};
use serde::Deserialize;

const PROFILE_JSON: &str =
    include_str!("../../../../HCR_Simulator_Frontend/tests/fixtures/cutter-grid-profile-v4.json");
const COMPACT_SUMMARY_JSON: &str = include_str!(
    "../../../../HCR_Simulator_Frontend/tests/fixtures/cutter-grid-compact-ptp-v4.json"
);
const CHALLENGE_JSON: &str = include_str!("fixtures/vectors.json");

#[derive(Debug, Deserialize)]
struct ChallengeFixture {
    challenge: ChallengeDefinition,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CompactSummary {
    planner_version: String,
    entry_option_id: String,
    executed_command_count: u32,
    estimated_duration_ms: f64,
    move_primitive_counts: Vec<usize>,
    cut_voxels: Vec<String>,
    result_voxel_count: usize,
    maximum_velocity_ratio: f64,
    maximum_acceleration_ratio: f64,
    maximum_jerk_ratio: f64,
    adaptive_validation_sample_count: u32,
}

fn challenge() -> ChallengeDefinition {
    serde_json::from_str::<ChallengeFixture>(CHALLENGE_JSON)
        .expect("frontend challenge fixture parses")
        .challenge
}

fn profile() -> CutterGridProfileV4 {
    serde_json::from_str(PROFILE_JSON).expect("frontend V4 Profile fixture parses")
}

fn compact_summary() -> CompactSummary {
    serde_json::from_str(COMPACT_SUMMARY_JSON).expect("frontend compact PTP summary fixture parses")
}

fn voxel_set(keys: impl IntoIterator<Item = String>) -> VoxelSet {
    keys.into_iter()
        .map(|key| key_to_coord(&key).expect("fixture voxel key is valid"))
        .collect()
}

fn sorted_cut_voxels(challenge: &ChallengeDefinition, remaining: &VoxelSet) -> Vec<String> {
    let mut cut: Vec<String> = challenge
        .initial_hair
        .voxels
        .iter()
        .filter(|coord| !remaining.contains(coord))
        .map(coord_to_key)
        .collect();
    cut.sort();
    cut
}

fn assert_close(actual: f64, expected: f64, label: &str) {
    assert!(
        (actual - expected).abs() <= 1e-6,
        "{label}: expected {expected}, got {actual}"
    );
}

#[test]
fn rust_reference_plan_matches_the_frontend_v4_compact_summary() {
    let challenge = challenge();
    let profile = profile();
    let expected = compact_summary();
    let compiled = compile_cutter_grid_program_v4(&profile.reference_program)
        .expect("certified reference Program compiles");
    let first = plan_cutter_grid_v4(&challenge, &compiled, &profile)
        .expect("certified reference Program is plannable in Rust");
    let second = plan_cutter_grid_v4(&challenge, &compiled, &profile)
        .expect("a second reference plan is deterministic");

    assert_eq!(first.trajectory_signature, second.trajectory_signature);
    assert_eq!(first.planner_version, expected.planner_version);
    assert_eq!(first.positioning.entry_option_id, expected.entry_option_id);
    assert_eq!(
        first.executed_command_count,
        expected.executed_command_count
    );
    assert_close(
        first.estimated_duration_ms,
        expected.estimated_duration_ms,
        "reference duration",
    );

    let primitive_counts: Vec<usize> = first
        .actions
        .iter()
        .filter_map(|action| match action {
            CutterGridTrajectoryActionV4::Move { primitives, .. } => Some(primitives.len()),
            CutterGridTrajectoryActionV4::Wait { .. } => None,
        })
        .collect();
    assert_eq!(primitive_counts, expected.move_primitive_counts);
    assert!(primitive_counts.iter().all(|count| (1..=2).contains(count)));
    assert_eq!(
        first.expected_result_voxels.len(),
        expected.result_voxel_count
    );
    assert_eq!(
        sorted_cut_voxels(&challenge, &voxel_set(first.expected_result_voxels.clone())),
        expected.cut_voxels
    );
    assert_close(
        first.diagnostics.maximum_velocity_ratio,
        expected.maximum_velocity_ratio,
        "maximum velocity ratio",
    );
    assert_close(
        first.diagnostics.maximum_acceleration_ratio,
        expected.maximum_acceleration_ratio,
        "maximum acceleration ratio",
    );
    assert_close(
        first.diagnostics.maximum_jerk_ratio,
        expected.maximum_jerk_ratio,
        "maximum jerk ratio",
    );
    assert_eq!(
        first.diagnostics.adaptive_validation_sample_count,
        expected.adaptive_validation_sample_count
    );

    let initial: VoxelSet = challenge.initial_hair.voxels.iter().copied().collect();
    let target: VoxelSet = challenge.target_hair.voxels.iter().copied().collect();
    let score = calculate_score(
        &initial,
        &target,
        &voxel_set(first.expected_result_voxels),
        &ProgramMetrics {
            source_block_count: profile.reference_program.source_block_count,
            executed_command_count: first.executed_command_count,
            estimated_duration_ms: first.estimated_duration_ms,
        },
        &challenge.scoring,
    )
    .expect("certified scoring configuration is valid");
    assert_close(score.completion_score, 100.0, "reference completion");
}

#[test]
fn global_ik_regression_uses_a_safe_low_wrist_branch_and_real_sweep() {
    let challenge = challenge();
    let profile = profile();
    let program = CutterGridProgramV4 {
        kind: "cutter-grid".into(),
        version: 1,
        planner_version: CUTTER_GRID_COMPACT_PTP_PLANNER_VERSION.into(),
        nodes: vec![
            CutterGridNode::Move {
                direction: CutterGridDirection::Up,
                distance: 6,
                source_block_id: "up-6".into(),
            },
            CutterGridNode::Move {
                direction: CutterGridDirection::Left,
                distance: 2,
                source_block_id: "left-2".into(),
            },
            CutterGridNode::Move {
                direction: CutterGridDirection::Forward,
                distance: 3,
                source_block_id: "forward-3".into(),
            },
        ],
        source_block_count: 3,
    };
    let compiled = compile_cutter_grid_program_v4(&program).expect("regression Program compiles");
    let plan = plan_cutter_grid_v4(&challenge, &compiled, &profile)
        .expect("the low-wrist branch keeps the regression path connected");

    assert_eq!(plan.end_coord, [-2, 6, -3]);
    let moves: Vec<_> = plan
        .actions
        .iter()
        .filter_map(|action| match action {
            CutterGridTrajectoryActionV4::Move { primitives, .. } => Some(primitives),
            CutterGridTrajectoryActionV4::Wait { .. } => None,
        })
        .collect();
    assert_eq!(moves.len(), 3);
    assert!(
        moves
            .iter()
            .all(|primitives| (1..=2).contains(&primitives.len()))
    );
    let wrist = moves
        .last()
        .and_then(|primitives| primitives.last())
        .and_then(|primitive| primitive.end.joint_angles.get("wrist"))
        .copied()
        .expect("the five-joint primitive includes Wrist");
    assert!(wrist < 100.0, "expected low Wrist branch, got {wrist}");

    let remaining = voxel_set(plan.expected_result_voxels);
    let actual_cuts: BTreeSet<String> = sorted_cut_voxels(&challenge, &remaining)
        .into_iter()
        .collect();
    let expected_cuts: BTreeSet<String> = ["-2,0,4", "-2,1,4", "-1,0,4", "-1,1,4", "-1,2,4"]
        .into_iter()
        .map(String::from)
        .collect();
    assert_eq!(actual_cuts, expected_cuts);
}
