//! Forward kinematics for the five-joint arm.
//!
//! A faithful port of `src/features/robot/kinematics.ts`. Operation order is
//! preserved exactly so results track the TypeScript engine as closely as
//! floating point allows — note that `sin`/`cos` are not required by IEEE-754 to
//! be correctly rounded, so agreement is to within a few ULP rather than exact
//! (`docs/backend/02-DETERMINISM.md` §4).
//!
//! The joint names below are hard-coded, matching the TypeScript: the arm's
//! topology is fixed even though joint *limits* are configurable.

use hcr_contract::{RobotConfig, Vec3};

use crate::error::SimError;
use crate::state::JointAngles;

/// World positions of every notable point on the arm.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RobotPose {
    /// Base mount point.
    pub base: Vec3,
    /// Shoulder pivot.
    pub shoulder: Vec3,
    /// Shoulder roll pivot (coincident with `shoulder`).
    pub shoulder_roll: Vec3,
    /// Elbow pivot.
    pub elbow: Vec3,
    /// Wrist pivot.
    pub wrist: Vec3,
    /// Tool mount (coincident with `wrist`).
    pub tool_base: Vec3,
    /// Tip of the tool — the point swept against hair voxels.
    pub end_effector: Vec3,
}

/// A row-major 3×3 rotation matrix.
type Matrix3 = [f64; 9];

/// Compute the pose implied by a set of joint angles.
///
/// # Errors
/// Returns [`SimError::MissingJoint`] if a required joint angle is absent or
/// non-finite, mirroring the TypeScript `readAngle` guard.
pub fn compute_robot_pose(
    robot_config: &RobotConfig,
    joint_angles: &JointAngles,
) -> Result<RobotPose, SimError> {
    let geometry = &robot_config.geometry;

    let base_yaw = degrees_to_radians(read_angle(joint_angles, "baseYaw")?);
    let shoulder_roll_angle = degrees_to_radians(read_angle(joint_angles, "shoulderRoll")?);
    let shoulder_angle = degrees_to_radians(read_angle(joint_angles, "shoulder")?);
    let elbow_angle = degrees_to_radians(read_angle(joint_angles, "elbow")?);
    let wrist_angle = degrees_to_radians(read_angle(joint_angles, "wrist")?);

    let base = geometry.base_position;
    let shoulder: Vec3 = [
        base[0],
        base[1] + geometry.shoulder_height,
        base[2],
    ];
    let shoulder_roll = shoulder;

    let base_rotation = rotation_y(base_yaw);
    let roll_rotation = multiply_matrices(base_rotation, rotation_x(shoulder_roll_angle));
    let shoulder_rotation = multiply_matrices(roll_rotation, rotation_z(shoulder_angle));
    let elbow = add_transformed_link(shoulder, geometry.upper_arm_length, shoulder_rotation);

    let elbow_rotation = multiply_matrices(shoulder_rotation, rotation_z(elbow_angle));
    let wrist = add_transformed_link(elbow, geometry.forearm_length, elbow_rotation);

    let wrist_rotation = multiply_matrices(elbow_rotation, rotation_z(wrist_angle));
    let tool_base = wrist;
    let end_effector = add_transformed_link(tool_base, geometry.tool_length, wrist_rotation);

    Ok(RobotPose {
        base,
        shoulder,
        shoulder_roll,
        elbow,
        wrist,
        tool_base,
        end_effector,
    })
}

fn add_transformed_link(start: Vec3, length: f64, rotation: Matrix3) -> Vec3 {
    let direction = transform_direction(rotation, [length, 0.0, 0.0]);
    [
        start[0] + direction[0],
        start[1] + direction[1],
        start[2] + direction[2],
    ]
}

fn transform_direction(m: Matrix3, v: Vec3) -> Vec3 {
    [
        m[0] * v[0] + m[1] * v[1] + m[2] * v[2],
        m[3] * v[0] + m[4] * v[1] + m[5] * v[2],
        m[6] * v[0] + m[7] * v[1] + m[8] * v[2],
    ]
}

fn multiply_matrices(l: Matrix3, r: Matrix3) -> Matrix3 {
    [
        l[0] * r[0] + l[1] * r[3] + l[2] * r[6],
        l[0] * r[1] + l[1] * r[4] + l[2] * r[7],
        l[0] * r[2] + l[1] * r[5] + l[2] * r[8],
        l[3] * r[0] + l[4] * r[3] + l[5] * r[6],
        l[3] * r[1] + l[4] * r[4] + l[5] * r[7],
        l[3] * r[2] + l[4] * r[5] + l[5] * r[8],
        l[6] * r[0] + l[7] * r[3] + l[8] * r[6],
        l[6] * r[1] + l[7] * r[4] + l[8] * r[7],
        l[6] * r[2] + l[7] * r[5] + l[8] * r[8],
    ]
}

fn rotation_x(angle: f64) -> Matrix3 {
    let (sine, cosine) = sin_cos(angle);
    [1.0, 0.0, 0.0, 0.0, cosine, -sine, 0.0, sine, cosine]
}

fn rotation_y(angle: f64) -> Matrix3 {
    let (sine, cosine) = sin_cos(angle);
    [cosine, 0.0, sine, 0.0, 1.0, 0.0, -sine, 0.0, cosine]
}

fn rotation_z(angle: f64) -> Matrix3 {
    let (sine, cosine) = sin_cos(angle);
    [cosine, -sine, 0.0, sine, cosine, 0.0, 0.0, 0.0, 1.0]
}

/// `libm` is used in **every** build, std or not.
///
/// `f64::sin` is std-only, so no_std needs `libm` regardless — but using it
/// unconditionally buys something better: the server and the firmware then run
/// bit-identical trigonometry instead of two implementations that agree only to
/// within a ULP. That removes an entire class of divergence between the two Rust
/// executors, at no cost.
fn sin_cos(angle: f64) -> (f64, f64) {
    (libm::sin(angle), libm::cos(angle))
}

fn read_angle(joint_angles: &JointAngles, joint_id: &str) -> Result<f64, SimError> {
    match joint_angles.get(joint_id) {
        Some(angle) if angle.is_finite() => Ok(angle),
        _ => Err(SimError::MissingJoint {
            joint_id: joint_id.into(),
        }),
    }
}

fn degrees_to_radians(degrees: f64) -> f64 {
    (degrees * core::f64::consts::PI) / 180.0
}
