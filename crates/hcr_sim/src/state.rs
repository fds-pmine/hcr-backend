//! Joint angle state.

use alloc::collections::BTreeMap;
use alloc::string::{String, ToString};
use hcr_contract::RobotConfig;

/// Current angle of every joint, in degrees.
///
/// Backed by a `BTreeMap` rather than a hash map so iteration order is
/// deterministic — the engine must be a pure function of its inputs
/// (`docs/backend/02-DETERMINISM.md`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct JointAngles {
    angles: BTreeMap<String, f64>,
}

impl JointAngles {
    /// Build the reset state from a challenge's joint definitions.
    pub fn initial(robot_config: &RobotConfig) -> Self {
        Self {
            angles: robot_config
                .joints
                .iter()
                .map(|joint| (joint.id.clone(), joint.initial_angle_deg))
                .collect(),
        }
    }

    /// Current angle of `joint_id`, if defined.
    pub fn get(&self, joint_id: &str) -> Option<f64> {
        self.angles.get(joint_id).copied()
    }

    /// Overwrite the angle of `joint_id`.
    pub fn set(&mut self, joint_id: &str, angle_deg: f64) {
        self.angles.insert(joint_id.to_string(), angle_deg);
    }

    /// Iterate joints in deterministic (sorted) order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, f64)> {
        self.angles.iter().map(|(id, angle)| (id.as_str(), *angle))
    }
}
