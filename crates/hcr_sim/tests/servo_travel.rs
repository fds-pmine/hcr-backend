//! Simulated joint travel against real servo travel.
//!
//! Every joint angle on the wire is **servo degrees** — what the arm is actually
//! commanded to. So a challenge may configure any range it likes and the
//! simulator will happily fly it, right up until the same program is sent to the
//! hardware, where the gateway clamps it (`hcr-fw/hcr-gateway/src/robot/`
//! `controller.rs`) and the arm silently stops agreeing with the screen.
//!
//! That failure is nasty because it is invisible in every test that does not
//! involve a physical arm: the simulation is self-consistent, the score is
//! self-consistent, and only the metal disagrees. This file makes the firmware's
//! limits a build-time fact so widening a joint past them fails here instead.
//!
//! Cutter Grid raises the stakes. Its planner searches for poses near the joint
//! limits — that is where the reach is — so a range that overstates the hardware
//! produces certified trajectories the arm cannot fly.

use hcr_contract::{ChallengeDefinition, ServoAxisId};

const VECTORS: &str = include_str!("fixtures/vectors.json");

/// `AXES` in `hcr-fw/hcr-gateway/src/robot/axis_config.rs`, converted from the
/// firmware's tenths of a degree.
///
/// Transcribed rather than shared: the firmware is a separate repository built
/// for a different target, and a build-time dependency on it would be a much
/// larger commitment than this table is worth. The cost is that this has to be
/// updated by hand when the hardware changes — which is why the source is named
/// here and the values are spelled out rather than computed.
struct ServoTravel {
    axis: ServoAxisId,
    minimum_deg: f64,
    maximum_deg: f64,
    home_deg: f64,
}

const FIRMWARE_AXES: [ServoTravel; 5] = [
    ServoTravel { axis: ServoAxisId::X, minimum_deg: 0.0, maximum_deg: 180.0, home_deg: 90.0 },
    ServoTravel { axis: ServoAxisId::Y, minimum_deg: 0.0, maximum_deg: 180.0, home_deg: 90.0 },
    ServoTravel { axis: ServoAxisId::Z, minimum_deg: 0.0, maximum_deg: 180.0, home_deg: 90.0 },
    ServoTravel { axis: ServoAxisId::B, minimum_deg: 0.0, maximum_deg: 180.0, home_deg: 90.0 },
    // The cutter. Deliberately unmapped in the simulator — SPEC v0.3 keeps
    // scissor actuation out of the first version — so no joint should claim it.
    ServoTravel { axis: ServoAxisId::E, minimum_deg: 45.0, maximum_deg: 100.0, home_deg: 90.0 },
];

fn shipped_challenge() -> ChallengeDefinition {
    let vectors: serde_json::Value = serde_json::from_str(VECTORS).expect("vectors parse");
    serde_json::from_value(vectors["challenge"].clone()).expect("challenge parses")
}

fn travel_for(axis: ServoAxisId) -> &'static ServoTravel {
    FIRMWARE_AXES
        .iter()
        .find(|candidate| candidate.axis == axis)
        .expect("every servo axis is in the firmware table")
}

/// No joint may ask for more travel than its servo has.
#[test]
fn every_mapped_joint_fits_inside_its_servo() {
    let challenge = shipped_challenge();

    for joint in &challenge.robot_config.joints {
        let Some(servo) = &joint.servo else {
            // Simulation-only joints (`shoulderRoll`) have no servo to exceed.
            continue;
        };
        let travel = travel_for(servo.axis);

        assert!(
            joint.min_angle_deg >= travel.minimum_deg,
            "{} bottoms out at {}° but servo {:?} stops at {}°",
            joint.id,
            joint.min_angle_deg,
            servo.axis,
            travel.minimum_deg,
        );
        assert!(
            joint.max_angle_deg <= travel.maximum_deg,
            "{} tops out at {}° but servo {:?} stops at {}°",
            joint.id,
            joint.max_angle_deg,
            servo.axis,
            travel.maximum_deg,
        );
        assert!(
            joint.initial_angle_deg >= joint.min_angle_deg
                && joint.initial_angle_deg <= joint.max_angle_deg,
            "{} rests at {}°, outside its own {}..{}°",
            joint.id,
            joint.initial_angle_deg,
            joint.min_angle_deg,
            joint.max_angle_deg,
        );
    }
}

/// Two joints on one servo would fight each other on the real arm.
#[test]
fn no_two_joints_share_a_servo() {
    let challenge = shipped_challenge();
    let mut claimed: Vec<ServoAxisId> = Vec::new();

    for joint in &challenge.robot_config.joints {
        if let Some(servo) = &joint.servo {
            assert!(
                !claimed.contains(&servo.axis),
                "{} claims servo {:?}, which is already driven",
                joint.id,
                servo.axis,
            );
            claimed.push(servo.axis);
        }
    }
}

/// The cutter axis stays unclaimed while scissors are out of scope.
///
/// If this ever fails it is probably intentional — someone added the cutter — and
/// the fix is to delete this test along with the prohibition it encodes, not to
/// work around it.
#[test]
fn the_cutter_axis_is_not_driven_by_a_joint() {
    let challenge = shipped_challenge();

    for joint in &challenge.robot_config.joints {
        if let Some(servo) = &joint.servo {
            assert_ne!(
                servo.axis,
                ServoAxisId::E,
                "{} drives the cutter servo, which the first version does not simulate",
                joint.id,
            );
        }
    }
}

/// Electron relies on one firmware-wide Home value before entering a
/// challenge-specific pose. Cover E explicitly too: it has no simulated joint,
/// so the mapping-centre test below cannot otherwise detect its Home drifting.
#[test]
fn every_firmware_axis_homes_at_ninety_degrees() {
    for travel in &FIRMWARE_AXES {
        assert_eq!(
            travel.home_deg, 90.0,
            "servo {:?} homes at {}° instead of the Electron contract's 90°",
            travel.axis, travel.home_deg,
        );
    }
}

/// Every servo homes at 90°, and every mapping centres there.
///
/// The mapping's `centerDeg` is the servo angle that `offsetDeg` corresponds to.
/// If it drifted from the firmware's `home_tenths`, a reset would leave the model
/// and the arm in different poses — and Cutter Grid's entry trajectory starts
/// from exactly that pose.
#[test]
fn servo_mappings_centre_on_the_firmware_home_position() {
    let challenge = shipped_challenge();

    for joint in &challenge.robot_config.joints {
        if let Some(servo) = &joint.servo {
            let travel = travel_for(servo.axis);
            assert_eq!(
                servo.center_deg, travel.home_deg,
                "{} centres on {}° but servo {:?} homes at {}°",
                joint.id, servo.center_deg, servo.axis, travel.home_deg,
            );
        }
    }
}
