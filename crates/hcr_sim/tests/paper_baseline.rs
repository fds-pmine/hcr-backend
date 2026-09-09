//! Conformance against the robotic-haircutting literature baseline.
//!
//! The criteria encoded here are derived from the published robotic-haircutting
//! corpus, not from this engine's own behaviour — the whole point is to measure
//! the engine against an external standard. Each test carries the criterion id
//! it discharges; the criteria themselves, with sources and DOIs, live in
//! [`docs/09-PAPER-BASELINE.md`](../../../docs/09-PAPER-BASELINE.md).
//!
//! # Two tiers
//!
//! **Tier A** — criteria the system already meets. These run in the ordinary
//! suite and must stay green: they are the claims a paper about this system
//! would be entitled to make.
//!
//! **Tier B** — criteria the literature sets and this system does *not* meet
//! yet. They are `#[ignore]`d so the normal suite stays green, and each panics
//! with the specific capability that is missing. `cargo test -p hcr_sim
//! --features planner -- --ignored` therefore prints the outstanding gap list
//! as a checklist. A Tier B test graduates to Tier A by being rewritten against
//! the API that closed it — never by being deleted.
//!
//! Tier A deliberately excludes criteria already covered elsewhere, so this file
//! adds rather than duplicates. Those are mapped in the doc's coverage table:
//! stop-at-last-safe-pose and tick-size independence are in `engine.rs`,
//! cross-language agreement is in `conformance.rs`, and reference-plan fidelity
//! is in `cutter_grid_v4_conformance.rs`.

/// Records a literature criterion this engine does not implement yet.
///
/// Used only by Tier B. The message names the missing capability rather than
/// the failing assertion, because there is no assertion to make until the
/// capability exists.
macro_rules! gap {
    ($criterion:literal, $missing:expr) => {
        panic!(
            "{} is not implemented.\n  missing capability: {}\n  see: docs/09-PAPER-BASELINE.md",
            $criterion, $missing
        )
    };
}

// ---------------------------------------------------------------------------
// Tier A — criteria this engine meets
//
// All of these read the certified V4 plan, so they need the server-side
// planner. Firmware/no_std builds do not link it and skip this module.
// ---------------------------------------------------------------------------

#[cfg(feature = "planner")]
mod tier_a {
    use hcr_contract::{ChallengeDefinition, CutterGridProfileV4};
    use hcr_sim::{compile_cutter_grid_program_v4, plan_cutter_grid_v4};
    use serde::Deserialize;

    const PROFILE_JSON: &str = include_str!(
        "../../../../HCR_Simulator_Frontend/tests/fixtures/cutter-grid-profile-v4.json"
    );
    const CHALLENGE_JSON: &str = include_str!("fixtures/vectors.json");

    #[derive(Debug, Deserialize)]
    struct ChallengeFixture {
        challenge: ChallengeDefinition,
    }

    fn challenge() -> ChallengeDefinition {
        serde_json::from_str::<ChallengeFixture>(CHALLENGE_JSON)
            .expect("frontend challenge fixture parses")
            .challenge
    }

    fn profile() -> CutterGridProfileV4 {
        serde_json::from_str(PROFILE_JSON).expect("frontend V4 Profile fixture parses")
    }

    /// Plan the certified reference program — the one motion this system claims
    /// is safe — and return its diagnostics.
    fn reference_plan() -> hcr_contract::CutterTrajectoryPlanV4 {
        let challenge = challenge();
        let profile = profile();
        let compiled = compile_cutter_grid_program_v4(&profile.reference_program)
            .expect("certified reference Program compiles");
        plan_cutter_grid_v4(&challenge, &compiled, &profile)
            .expect("certified reference Program is plannable")
    }

    /// **PB-SAFE-01** — no motion is emitted unless a positive head clearance
    /// has been *proven* over the whole primitive, not sampled at endpoints.
    ///
    /// The three-layer safety framework's control layer requires the separation
    /// between tool and head to be an invariant of the plan rather than a
    /// property checked at execution time. The existing conformance test pins
    /// this number against the frontend; it never asserts it is safe. A planner
    /// change that certified a clearance of zero would keep that test green.
    #[test]
    fn pb_safe_01_certified_plans_prove_positive_head_clearance() {
        let clearance = reference_plan().diagnostics.minimum_head_clearance;
        assert!(
            clearance > 0.0,
            "certified plan must keep the arm strictly clear of the head, \
             got minimum clearance {clearance}"
        );
    }

    /// **PB-SAFE-02** — dynamic limits are *satisfied*, not merely reported.
    ///
    /// Jerk limiting is the safety property the surveys of this field never
    /// specify. Asserting the ratios stay inside their hard limits is what makes
    /// the certificate a guarantee: a ratio above 1.0 means the plan exceeded a
    /// limit it recorded, which the reproduction test would happily accept.
    #[test]
    fn pb_safe_02_dynamic_limits_are_satisfied_not_merely_recorded() {
        let diagnostics = reference_plan().diagnostics;

        for (label, ratio) in [
            ("velocity", diagnostics.maximum_velocity_ratio),
            ("acceleration", diagnostics.maximum_acceleration_ratio),
            ("jerk", diagnostics.maximum_jerk_ratio),
        ] {
            assert!(
                ratio <= 1.0,
                "{label} ratio {ratio} exceeds its hard limit; the certificate \
                 records a violation instead of preventing one"
            );
        }

        // Clamping may only ever slow the motion down. A realized scale above
        // the requested one would mean the limiter accelerated the arm.
        assert!(
            diagnostics.actual_speed_scale <= diagnostics.requested_speed_scale,
            "speed clamping raised the speed scale from {} to {}",
            diagnostics.requested_speed_scale,
            diagnostics.actual_speed_scale
        );
    }

    /// **PB-PATH-01** — every certified pose keeps a margin against the joint
    /// limits, so the plan does not depend on reaching a mechanical hard stop.
    #[test]
    fn pb_path_01_certified_plans_keep_a_joint_limit_margin() {
        let margin = reference_plan().diagnostics.minimum_joint_limit_margin;
        assert!(
            margin > 0.0,
            "certified plan must not drive a joint onto its limit, \
             got normalized margin {margin}"
        );
    }

    /// **PB-PATH-02** — the tool removes only the material it was asked to
    /// remove, and approach motion removes none.
    ///
    /// This is the material-removal discipline the CNC framing of haircutting
    /// demands: in machining terms, collateral cuts are gouges, and rapids must
    /// not touch the workpiece. It is the closest thing this system has to a
    /// correctness criterion for the cut itself.
    #[test]
    fn pb_path_02_the_tool_removes_only_intended_material() {
        let certification = profile().certification;

        assert!(
            certification.entry_zero_contact,
            "system positioning must reach the origin without cutting hair"
        );
        assert!(
            certification.reference_extra_cut_voxels.is_empty(),
            "reference program cut {} voxels it was not asked to cut: {:?}",
            certification.reference_extra_cut_voxels.len(),
            certification.reference_extra_cut_voxels
        );
        assert!(
            certification.passed,
            "the shipped profile must carry a passing certification"
        );
    }
}

// ---------------------------------------------------------------------------
// Tier B — criteria the literature sets that this system does not meet
//
// Each names one missing capability. None of them are failures of the current
// design; they are the distance between a deterministic teaching simulator and
// a machine the literature would call human-safe.
// ---------------------------------------------------------------------------

/// **PB-HRI-01** — supervised autonomy: execution is continuously compared
/// against a trajectory the client consented to, and a deviation past threshold
/// escalates warn → pause → hand over to a human.
///
/// This system generates and certifies a trajectory, but nothing observes the
/// executed one. The certificate is a pre-flight proof; supervised autonomy
/// additionally requires an in-flight monitor, because the proof assumes a head
/// pose that the person in the chair is under no obligation to maintain. The
/// shared-control primaries are Dass et al. (PATO: Policy Assisted TeleOperation,
/// RSS 2023, arXiv:2212.04708) and Liu et al. (Robot Learning on the Job, Int. J.
/// Robotics Research 2025, 44(10-11), 1727-1742, doi:10.1177/02783649241273901).
#[test]
#[ignore = "PB-HRI-01 gap — no deviation monitor exists"]
fn pb_hri_01_execution_is_monitored_against_the_consented_trajectory() {
    gap!(
        "PB-HRI-01 (supervised-autonomy deviation monitoring)",
        "a monitor that subscribes to planned and executed trajectories, \
         computes deviation, and publishes warn/pause/estop with hysteresis"
    );
}

/// **PB-SAFE-03** — an emergency stop preempts queued motion within a bounded
/// time, rather than being processed in command order.
///
/// The wire contract already states the rule — e-stop travels on its own topic
/// and firmware must handle it ahead of ordinary commands
/// (`docs/01-CONTRACT.md` §6.5) — but no executor in this workspace implements
/// it. `ProgramExecutor` runs a command list to completion or to a fault; there
/// is no channel by which a stop can arrive mid-program.
#[test]
#[ignore = "PB-SAFE-03 gap — no e-stop path in any executor"]
fn pb_safe_03_estop_preempts_queued_motion() {
    gap!(
        "PB-SAFE-03 (e-stop preemption)",
        "an interrupt input on the executor that halts within a bounded number \
         of ticks, ahead of the pending command queue"
    );
}

/// **PB-SAFE-04** — loss of communication de-energizes or holds position.
///
/// The contract requires hold-position-on-disconnect and the embedded notes
/// name a watchdog as mandatory (`docs/05-EMBEDDED.md` §7), but the last will
/// only informs the *server* that the device vanished. Nothing in this
/// workspace acts on that, and the simulator has no notion of a link at all.
#[test]
#[ignore = "PB-SAFE-04 gap — LWT informs the server, nothing de-energizes"]
fn pb_safe_04_communication_loss_holds_position() {
    gap!(
        "PB-SAFE-04 (comms-loss watchdog)",
        "a device-side watchdog that holds position and refuses motion until \
         re-registration, independent of the broker"
    );
}

/// **PB-SAFE-05** — contact force against the scalp is bounded.
///
/// The threshold comes from ISO/TS 15066 Annex A (per-body-region contact
/// limits) and Behrens et al., Front. Robot. AI 2022, 8:667818,
/// doi:10.3389/frobt.2021.667818. Bilyea, French & Abdullah (Proc. IMechE
/// Part H 2023, 237(7), 829-840, doi:10.1177/09544119231179568) supplies
/// forces measured during hair brushing specifically.
///
/// The literature disagrees about the number. Huang, Li & Roning (J. Field
/// Robotics 2026) report ~65 N transient and ~90 N/cm^2 for the head; ISO/TS
/// 15066 Annex A gives 130 N for skull-and-forehead, twice that. Read the
/// standard before hard-coding either.
///
/// This engine models geometry only: collision is a boolean over swept
/// primitives, with no compliance, no force and no torque. A clearance of
/// zero and a press of ten newtons are the same event here.
#[test]
#[ignore = "PB-SAFE-05 gap — the model is purely geometric"]
fn pb_safe_05_scalp_contact_force_is_bounded() {
    gap!(
        "PB-SAFE-05 (bounded contact force)",
        "a contact model carrying force or torque, and a limit derived from \
         published grooming contact-force measurements"
    );
}

/// **PB-PERC-01** — the head may move during execution.
///
/// `VoxelConfig` fixes `head_center` and `head_scale` for the whole replay, and
/// `docs/05-EMBEDDED.md` §7 states the assumption outright: a perfectly known
/// head pose, zero servo error, no human movement. The CNC framing of
/// haircutting is explicit that the workpiece moving is the one thing that
/// separates it from machining, and it is named as an open problem.
#[test]
#[ignore = "PB-PERC-01 gap — head pose is static for the whole replay"]
fn pb_perc_01_head_pose_may_change_during_execution() {
    gap!(
        "PB-PERC-01 (moving workpiece)",
        "a time-varying head pose in the replay loop, plus replanning or \
         compensation when it moves past a threshold"
    );
}

/// **PB-MODEL-01** — hair is anchored at the root.
///
/// Voxels are independent lattice cells: removing one says nothing about the
/// strand it belonged to. Root-anchored manipulation — hold the root, style the
/// free segment — is the operation that makes real hair tractable, and it has
/// no representation in a model with no strand identity. The primary result is
/// Kim, Kanazawa, Hasegawa, Kawaharazuka & Okada, IEEE/SICE SII 2025, 544–549
/// (doi:10.1109/SII59315.2025.10871088), which pairs it with path planning.
#[test]
#[ignore = "PB-MODEL-01 gap — voxels carry no strand identity"]
fn pb_model_01_hair_is_anchored_at_the_root() {
    gap!(
        "PB-MODEL-01 (root-anchored hair)",
        "a hair representation with root attachment and strand identity, so a \
         cut shortens a strand rather than clearing an independent cell"
    );
}

/// **PB-MODEL-02** — the cut target is a scalar length field over the scalp.
///
/// Scoring here compares voxel sets: a hairstyle is the set of cells that should
/// remain. The end-to-end pipelines in this literature instead carry a target
/// length distribution over the scalp surface, which is what lets one target
/// generalize across head shapes instead of being tied to one lattice.
#[test]
#[ignore = "PB-MODEL-02 gap — targets are voxel sets, not length fields"]
fn pb_model_02_cut_target_is_a_scalp_length_field() {
    gap!(
        "PB-MODEL-02 (scalp length field)",
        "a target expressed as length-over-scalp-surface, and scoring that \
         measures realized length against it"
    );
}

/// **PB-PATH-03** — coverage is planned as decomposed passes over a surface.
///
/// The CNC framing calls for boustrophedon coverage decomposition with
/// multi-pass optimization. This planner takes an explicit grid program from the
/// learner and finds a certified path between the cells it names; it never
/// decomposes a region into passes, because in the teaching product choosing
/// the traversal *is* the exercise. Closing this is a planner alongside the
/// existing one, not a replacement for it.
#[test]
#[ignore = "PB-PATH-03 gap — traversal is authored, never decomposed"]
fn pb_path_03_coverage_is_decomposed_into_passes() {
    gap!(
        "PB-PATH-03 (coverage decomposition)",
        "a boustrophedon coverage planner over the scalp with multi-pass depth \
         scheduling, emitting programs the existing certifier can accept"
    );
}

/// **PB-ARCH-01** — the device topic grammar is typed, not prose.
///
/// `docs/01-CONTRACT.md` §5 specifies the topic tree and §6.5 the device
/// payloads, including the rule that separates e-stop from commands. No Rust
/// type encodes any of it: there is no `hcr/v1/...` string anywhere under
/// `crates/`, and no `dev.estop.req` type. Until the grammar is a type, the rule
/// that keeps a stop off the command queue is a sentence in a document that
/// nothing enforces.
#[test]
#[ignore = "PB-ARCH-01 gap — MQTT topics and device payloads exist only in docs"]
fn pb_arch_01_device_topic_grammar_is_typed() {
    gap!(
        "PB-ARCH-01 (typed transport contract)",
        "topic constructors and device payload types in hcr_contract, so the \
         separate-topic e-stop rule is checkable by the compiler"
    );
}
