//! Turning a continuous HCR score into something arona can consume.

/// Map a raw normalized score so that `s > tau` becomes `remapped > 0.5`.
///
/// # Why this exists
///
/// HCR produces continuous scores, but every arona estimator collapses a response
/// through `Score::is_correct()`, a hard `> 0.5` test
/// (`arona/src/core/score.rs:143`). Handing it a raw normalized score would
/// silently define "mastery" as exactly 50/100 — and since `finalScore` blends
/// completion (0.6), efficiency (0.25) and time (0.15), 50 is not a defensible
/// bar for any particular item.
///
/// This remap moves the decision boundary to a per-item threshold `tau` while
/// preserving order, so no ranking information is lost and the estimator's
/// dichotomization lands where the item author intended.
///
/// It is a **workaround, not a solution**. The principled fix is a polytomous
/// model (GPCM or a graded response model), which arona does not implement. The
/// raw score is persisted separately so that upgrade stays open
/// (`docs/backend/03-DYNAMIC-QBANK.md` §2).
///
/// # Properties
///
/// Monotonic and continuous, with `0 → 0`, `tau → 0.5`, `1 → 1`.
///
/// # Panics
/// Debug builds assert `0 < tau < 1`; release builds clamp instead, because a
/// bad threshold must never take down a live session.
pub fn remap_for_arona(raw_score: f64, tau: f64) -> f64 {
    debug_assert!(
        tau > 0.0 && tau < 1.0,
        "mastery threshold must be strictly between 0 and 1, got {tau}"
    );

    // Guard the release path: a degenerate threshold would divide by zero.
    let tau = tau.clamp(f64::EPSILON, 1.0 - f64::EPSILON);
    let raw = raw_score.clamp(0.0, 1.0);

    let mapped = if raw <= tau {
        0.5 * (raw / tau)
    } else {
        0.5 + 0.5 * (raw - tau) / (1.0 - tau)
    };

    // arona's `Score::new` asserts its input is in [0,1] and panics otherwise,
    // so clamp rather than trust the arithmetic.
    mapped.clamp(0.0, 1.0)
}

/// Inverse of [`remap_for_arona`], for reporting a stored arona score back in raw
/// terms.
pub fn raw_from_remapped(remapped: f64, tau: f64) -> f64 {
    let tau = tau.clamp(f64::EPSILON, 1.0 - f64::EPSILON);
    let mapped = remapped.clamp(0.0, 1.0);

    let raw = if mapped <= 0.5 {
        (mapped / 0.5) * tau
    } else {
        tau + ((mapped - 0.5) / 0.5) * (1.0 - tau)
    };

    raw.clamp(0.0, 1.0)
}

/// Whether a raw score counts as mastery at this threshold.
///
/// Agrees with what arona will conclude from the remapped score.
pub fn is_mastered(raw_score: f64, tau: f64) -> bool {
    remap_for_arona(raw_score, tau) > 0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_maps_to_the_decision_boundary() {
        for tau in [0.1, 0.3, 0.5, 0.7, 0.9] {
            assert!((remap_for_arona(tau, tau) - 0.5).abs() < 1e-12);
            assert_eq!(remap_for_arona(0.0, tau), 0.0);
            assert_eq!(remap_for_arona(1.0, tau), 1.0);
        }
    }

    #[test]
    fn mastery_agrees_with_aronas_dichotomization() {
        let tau = 0.8;
        // Just under the bar fails, just over it passes — which a raw score fed
        // straight to arona would get wrong, since 0.79 > 0.5.
        assert!(!is_mastered(0.79, tau));
        assert!(is_mastered(0.81, tau));
        assert!(!is_mastered(0.5, tau));
    }

    #[test]
    fn remap_is_monotonic() {
        let tau = 0.65;
        let mut previous = -1.0;
        for step in 0..=1000 {
            let raw = f64::from(step) / 1000.0;
            let mapped = remap_for_arona(raw, tau);
            assert!(mapped >= previous, "not monotonic at raw={raw}");
            previous = mapped;
        }
    }

    #[test]
    fn round_trips_through_the_inverse() {
        let tau = 0.42;
        for step in 0..=100 {
            let raw = f64::from(step) / 100.0;
            let back = raw_from_remapped(remap_for_arona(raw, tau), tau);
            assert!((back - raw).abs() < 1e-12, "raw={raw} came back as {back}");
        }
    }

    #[test]
    fn output_is_always_a_legal_arona_score() {
        // `Score::new` panics outside [0,1], so nothing may escape the range —
        // including from absurd inputs.
        for raw in [-5.0, -0.0, 0.5, 1.0, 7.0, f64::INFINITY] {
            let mapped = remap_for_arona(raw, 0.5);
            assert!((0.0..=1.0).contains(&mapped), "raw={raw} produced {mapped}");
        }
    }
}
