//! Diagnostic: what does the cap-trim family actually produce?
//!
//! Difficulty-model coefficients are meaningless until you know the range each
//! feature really takes. Run with:
//!   cargo run -p hcr_qbank --example family_probe

use hcr_contract::*;
use hcr_qbank::*;

fn prototype() -> ChallengeDefinition {
    let joint = |id: &str, axis: Axis, min: f64, max: f64, initial: f64, speed: f64| JointConfig {
        id: id.into(),
        name: id.into(),
        axis,
        min_angle_deg: min,
        max_angle_deg: max,
        initial_angle_deg: initial,
        speed_deg_per_sec: speed,
    };

    ChallengeDefinition {
        id: "prototype".into(),
        name: "Prototype".into(),
        description: String::new(),
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
            voxels: vec![],
        },
        target_hair: HairstyleDefinition {
            id: "t".into(),
            name: "t".into(),
            voxels: vec![],
        },
        allowed_blocks: vec![AllowedBlockType::SetJointAngle],
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
    }
}

fn span(label: &str, values: &mut [f64]) {
    if values.is_empty() {
        println!("  {label:<18} (no samples)");
        return;
    }
    values.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = values[values.len() / 2];
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    println!(
        "  {label:<18} min {:>8.3}  median {:>8.3}  mean {:>8.3}  max {:>8.3}",
        values[0],
        median,
        mean,
        values[values.len() - 1]
    );
}

fn main() {
    let generator = CapTrimGenerator::new(prototype());
    let model = DifficultyModel::expert_prior();

    let (mut volume, mut boundary, mut asym, mut reach, mut prox) =
        (vec![], vec![], vec![], vec![], vec![]);
    let (mut difficulty, mut removal) = (vec![], vec![]);
    let mut rejected = 0;

    for seed in 0..300u64 {
        let params = generator.family().sample(seed);
        match generator.generate_item(seed, &params, &model) {
            Ok(item) => {
                let f = item.features;
                volume.push(f.removal_volume);
                boundary.push(f.boundary_ratio);
                asym.push(f.asymmetry);
                reach.push(f.reach_strain);
                prox.push(f.head_proximity);
                difficulty.push(item.predicted_difficulty);
                let initial = item.dto.challenge.initial_hair.voxels.len();
                let target = item.dto.challenge.target_hair.voxels.len();
                removal.push((initial - target) as f64);
            }
            Err(_) => rejected += 1,
        }
    }

    println!("cap-trim family over 300 seeds ({rejected} rejected)\n");
    println!("features:");
    span("removal_volume", &mut volume);
    span("boundary_ratio", &mut boundary);
    span("asymmetry", &mut asym);
    span("reach_strain", &mut reach);
    span("head_proximity", &mut prox);
    println!("\noutputs:");
    span("removed voxels", &mut removal);
    span("predicted b", &mut difficulty);

    println!("\ndegenerate parameter probes:");
    for (label, p) in [
        ("span=0.0", (2.0, 1.0, 0.0, 0.0)),
        ("trim_depth=0", (2.0, 0.0, 0.6, 0.0)),
        ("span=0.05", (2.0, 1.0, 0.05, 0.0)),
    ] {
        let params = ParamVector::from([
            (CapTrimGenerator::CAP_THICKNESS.to_string(), p.0),
            (CapTrimGenerator::TRIM_DEPTH.to_string(), p.1),
            (CapTrimGenerator::REGION_SPAN.to_string(), p.2),
            (CapTrimGenerator::REGION_TURN.to_string(), p.3),
        ]);
        match generator.generate(1, &params) {
            Ok(c) => println!(
                "  {label:<14} initial {:>4}  target {:>4}  removed {:>4}",
                c.initial_hair.voxels.len(),
                c.target_hair.voxels.len(),
                c.initial_hair.voxels.len() - c.target_hair.voxels.len()
            ),
            Err(e) => println!("  {label:<14} rejected: {e}"),
        }
    }
}
