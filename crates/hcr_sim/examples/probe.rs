//! Diagnostic: where the arm actually is, and where it is safe.
//!
//! Run with `cargo run -p hcr_sim --example probe`.

use hcr_contract::*;
use hcr_sim::*;

fn joint(id: &str, axis: Axis, min: f64, max: f64, initial: f64, speed: f64) -> JointConfig {
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

fn main() {
    let robot_config = RobotConfig {
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
    };
    let voxel_config = VoxelConfig {
        origin: [1.35, 1.5, 0.0],
        size: 0.16,
        head_center: [1.35, 1.42, 0.0],
        head_scale: [0.68, 0.86, 0.68],
    };

    let initial = JointAngles::initial(&robot_config);
    let pose = compute_robot_pose(&robot_config, &initial).unwrap();
    println!(
        "initial pose: endEffector = [{:.4}, {:.4}, {:.4}]  collision = {:?}",
        pose.end_effector[0],
        pose.end_effector[1],
        pose.end_effector[2],
        find_robot_head_collision(&pose, &voxel_config, &robot_config.geometry).map(|c| c.part)
    );

    println!("\nbaseYaw sweep (other joints at their initial angles):");
    let mut yaw = -60.0_f64;
    while yaw <= 60.0 {
        let mut angles = JointAngles::initial(&robot_config);
        angles.set("baseYaw", yaw);
        let pose = compute_robot_pose(&robot_config, &angles).unwrap();
        let hit = find_robot_head_collision(&pose, &voxel_config, &robot_config.geometry);
        println!(
            "  yaw {yaw:>6.1}  ee = [{:>7.4}, {:>7.4}, {:>7.4}]  {}",
            pose.end_effector[0],
            pose.end_effector[1],
            pose.end_effector[2],
            match hit {
                Some(c) => c.part.label(),
                None => "safe",
            }
        );
        yaw += 5.0;
    }

    println!("\nlattice cell nearest the end effector, per yaw (safe yaws only):");
    let mut yaw = -60.0_f64;
    while yaw <= 60.0 {
        let mut angles = JointAngles::initial(&robot_config);
        angles.set("baseYaw", yaw);
        let pose = compute_robot_pose(&robot_config, &angles).unwrap();
        if find_robot_head_collision(&pose, &voxel_config, &robot_config.geometry).is_none() {
            let ee = pose.end_effector;
            println!(
                "  yaw {yaw:>6.1}  cell = ({}, {}, {})",
                ((ee[0] - voxel_config.origin[0]) / voxel_config.size).round() as i32,
                ((ee[1] - voxel_config.origin[1]) / voxel_config.size).round() as i32,
                ((ee[2] - voxel_config.origin[2]) / voxel_config.size).round() as i32,
            );
        }
        yaw += 5.0;
    }
}
