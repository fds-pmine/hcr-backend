//! Head-safety constraint.
//!
//! Ports `src/features/robot/headCollision.ts`. This is a deterministic geometric
//! constraint for the simulation. It is **not** a safety system for physical
//! hardware — see `docs/backend/05-EMBEDDED.md` §7.

use hcr_contract::{RobotGeometryConfig, Vec3, VoxelConfig};

use crate::kinematics::RobotPose;

/// Which arm primitive touched the head.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RobotCollisionPart {
    /// Base column.
    Base,
    /// Shoulder pivot sphere.
    ShoulderJoint,
    /// Shoulder-to-elbow link.
    UpperArm,
    /// Elbow pivot sphere.
    ElbowJoint,
    /// Elbow-to-wrist link.
    Forearm,
    /// Wrist pivot sphere.
    WristJoint,
    /// Wrist-to-tip shaft.
    ToolShaft,
    /// Tool tip sphere.
    EndEffector,
}

impl RobotCollisionPart {
    /// Human-readable label, matching the TypeScript engine's strings exactly so
    /// log messages and conformance vectors line up.
    pub fn label(self) -> &'static str {
        match self {
            RobotCollisionPart::Base => "Base",
            RobotCollisionPart::ShoulderJoint => "Shoulder Joint",
            RobotCollisionPart::UpperArm => "Upper Arm Link",
            RobotCollisionPart::ElbowJoint => "Elbow Joint",
            RobotCollisionPart::Forearm => "Forearm Link",
            RobotCollisionPart::WristJoint => "Wrist Joint",
            RobotCollisionPart::ToolShaft => "Tool Shaft",
            RobotCollisionPart::EndEffector => "End Effector",
        }
    }
}

/// A detected contact between the arm and the head volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadCollision {
    /// The offending primitive.
    pub part: RobotCollisionPart,
}

/// First arm primitive (in fixed order) that would enter the expanded head.
///
/// Order is significant: the TypeScript uses `Array.find`, so the reported part
/// is the first match, not the deepest one.
pub fn find_robot_head_collision(
    pose: &RobotPose,
    voxel_config: &VoxelConfig,
    geometry: &RobotGeometryConfig,
) -> Option<HeadCollision> {
    let collision = &geometry.collision;
    let primitives: [(RobotCollisionPart, Vec3, Vec3, f64); 8] = [
        (
            RobotCollisionPart::Base,
            pose.base,
            pose.shoulder,
            collision.joint_radius,
        ),
        (
            RobotCollisionPart::ShoulderJoint,
            pose.shoulder,
            pose.shoulder,
            collision.joint_radius,
        ),
        (
            RobotCollisionPart::UpperArm,
            pose.shoulder,
            pose.elbow,
            collision.link_radius,
        ),
        (
            RobotCollisionPart::ElbowJoint,
            pose.elbow,
            pose.elbow,
            collision.joint_radius,
        ),
        (
            RobotCollisionPart::Forearm,
            pose.elbow,
            pose.wrist,
            collision.link_radius,
        ),
        (
            RobotCollisionPart::WristJoint,
            pose.wrist,
            pose.wrist,
            collision.joint_radius,
        ),
        (
            RobotCollisionPart::ToolShaft,
            pose.tool_base,
            pose.end_effector,
            collision.tool_shaft_radius,
        ),
        (
            RobotCollisionPart::EndEffector,
            pose.end_effector,
            pose.end_effector,
            geometry.tool_radius,
        ),
    ];

    primitives
        .into_iter()
        .find(|(_, start, end, radius)| {
            segment_intersects_expanded_ellipsoid(
                *start,
                *end,
                voxel_config.head_center,
                voxel_config.head_scale,
                radius + collision.head_clearance,
            )
        })
        .map(|(part, _, _, _)| HeadCollision { part })
}

/// Whether a capsule of radius `expansion` around segment `start..end` overlaps
/// the head ellipsoid.
///
/// Works by scaling space so the expanded ellipsoid becomes a unit sphere, then
/// finding the closest point on the segment to the origin.
pub fn segment_intersects_expanded_ellipsoid(
    start: Vec3,
    end: Vec3,
    center: Vec3,
    scale: Vec3,
    expansion: f64,
) -> bool {
    let axes: Vec3 = [
        scale[0] + expansion,
        scale[1] + expansion,
        scale[2] + expansion,
    ];
    let normalized_start = normalize_point(start, center, axes);
    let normalized_end = normalize_point(end, center, axes);
    let direction: Vec3 = [
        normalized_end[0] - normalized_start[0],
        normalized_end[1] - normalized_start[1],
        normalized_end[2] - normalized_start[2],
    ];

    let length_squared = dot(direction, direction);
    let closest_t = if length_squared == 0.0 {
        0.0
    } else {
        (-dot(normalized_start, direction) / length_squared).clamp(0.0, 1.0)
    };

    let closest: Vec3 = [
        normalized_start[0] + direction[0] * closest_t,
        normalized_start[1] + direction[1] * closest_t,
        normalized_start[2] + direction[2] * closest_t,
    ];

    dot(closest, closest) <= 1.0
}

fn normalize_point(point: Vec3, center: Vec3, axes: Vec3) -> Vec3 {
    [
        (point[0] - center[0]) / axes[0],
        (point[1] - center[1]) / axes[1],
        (point[2] - center[2]) / axes[2],
    ]
}

fn dot(left: Vec3, right: Vec3) -> f64 {
    left[0] * right[0] + left[1] * right[1] + left[2] * right[2]
}
