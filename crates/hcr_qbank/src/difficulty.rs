//! Predicting an item's difficulty before anyone has attempted it.

use hcr_contract::ChallengeDefinition;

use crate::features::ChallengeFeatures;

/// arona validates `Difficulty` to this range (`arona/src/primitive_wrapper`),
/// so predictions are clamped into it.
pub const DIFFICULTY_MIN: f64 = -3.0;
/// Upper end of the usable difficulty scale.
pub const DIFFICULTY_MAX: f64 = 3.0;

/// A linear difficulty model, LLTM-style: `b̂ = β₀ + Σ βₖ · fₖ`.
///
/// # Status of the coefficients
///
/// The defaults are **expert priors, not fitted values**. They encode plausible
/// orderings — a target that hugs the scalp is harder than one that does not —
/// and give generated items a sane starting point, nothing more. Every item they
/// produce enters the bank as `Provisional` and is expected to be recalibrated
/// from response data; the intended path is to regress observed `b` on these
/// features and replace the priors
/// (`docs/backend/03-DYNAMIC-QBANK.md` §6).
///
/// Treating these numbers as measurements would be a mistake.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DifficultyModel {
    /// β₀.
    pub intercept: f64,
    /// Coefficient on `ln(1 + removal count)`.
    pub removal_volume: f64,
    /// Coefficient on the target's boundary fraction.
    pub boundary_ratio: f64,
    /// Coefficient on left/right asymmetry.
    pub asymmetry: f64,
    /// Coefficient on reach demand.
    pub reach_strain: f64,
    /// Coefficient on proximity to the head.
    pub head_proximity: f64,
}

impl Default for DifficultyModel {
    fn default() -> Self {
        Self::expert_prior()
    }
}

impl DifficultyModel {
    /// Hand-set coefficients used until enough response data exists to fit real ones.
    pub fn expert_prior() -> Self {
        Self {
            // Calibrated against the measured feature distribution of the
            // reference `cap-trim` family (see `examples/family_probe.rs`), whose
            // median item contributes ≈ 5.1 across the five terms. An earlier
            // guess of -2.0 put more than half the family at the +3 ceiling,
            // making every generated item look equally hard and defeating
            // difficulty targeting entirely.
            intercept: -5.0,
            // ln-scaled: a 200-voxel job contributes ~5.3 × 0.30 ≈ 1.6 logits
            // more than a 5-voxel one.
            removal_volume: 0.30,
            boundary_ratio: 1.10,
            asymmetry: 0.90,
            reach_strain: 1.40,
            head_proximity: 1.70,
        }
    }

    /// Predict difficulty from an extracted feature vector.
    pub fn predict_features(&self, features: &ChallengeFeatures) -> f64 {
        let raw = self.intercept
            + self.removal_volume * features.removal_volume
            + self.boundary_ratio * features.boundary_ratio
            + self.asymmetry * features.asymmetry
            + self.reach_strain * features.reach_strain
            + self.head_proximity * features.head_proximity;

        // arona rejects difficulties outside [-3, 3]; an unclamped prediction
        // would produce an item the bank could never validate.
        raw.clamp(DIFFICULTY_MIN, DIFFICULTY_MAX)
    }

    /// Extract features from a challenge and predict its difficulty.
    pub fn predict(&self, challenge: &ChallengeDefinition) -> f64 {
        self.predict_features(&ChallengeFeatures::extract(challenge))
    }

    /// Coefficients in the same order as [`ChallengeFeatures::as_array`].
    pub fn weights(&self) -> [f64; 5] {
        [
            self.removal_volume,
            self.boundary_ratio,
            self.asymmetry,
            self.reach_strain,
            self.head_proximity,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features(
        removal_volume: f64,
        boundary_ratio: f64,
        asymmetry: f64,
        reach_strain: f64,
        head_proximity: f64,
    ) -> ChallengeFeatures {
        ChallengeFeatures {
            removal_volume,
            boundary_ratio,
            asymmetry,
            reach_strain,
            head_proximity,
        }
    }

    #[test]
    fn every_feature_increases_predicted_difficulty() {
        let model = DifficultyModel::expert_prior();

        // Start from the measured medians of the reference family
        // (`examples/family_probe.rs`) so the comparison happens in the model's
        // operating range. An all-zero baseline sits below the clamp floor,
        // where every variant would flatten to -3 and the test would prove
        // nothing.
        let baseline = features(5.60, 0.94, 0.57, 0.65, 0.57);
        let base = model.predict_features(&baseline);
        assert!(
            base > DIFFICULTY_MIN && base < DIFFICULTY_MAX,
            "baseline {base} must not sit against a clamp"
        );

        let variants = [
            ("removal_volume", features(6.60, 0.94, 0.57, 0.65, 0.57)),
            ("boundary_ratio", features(5.60, 0.99, 0.57, 0.65, 0.57)),
            ("asymmetry", features(5.60, 0.94, 0.77, 0.65, 0.57)),
            ("reach_strain", features(5.60, 0.94, 0.57, 0.85, 0.57)),
            ("head_proximity", features(5.60, 0.94, 0.57, 0.65, 0.72)),
        ];
        for (name, variant) in variants {
            let raised = model.predict_features(&variant);
            assert!(
                raised > base,
                "raising {name} should make the item harder ({raised} vs {base})"
            );
        }
    }

    #[test]
    fn predictions_stay_inside_aronas_valid_range() {
        let model = DifficultyModel::expert_prior();

        // Absurd inputs in both directions must still yield a usable difficulty,
        // because arona validates Difficulty to [-3, 3].
        let huge = features(1_000.0, 1.0, 1.0, 1.0, 1.0);
        let tiny = features(-1_000.0, 0.0, 0.0, 0.0, 0.0);

        assert_eq!(model.predict_features(&huge), DIFFICULTY_MAX);
        assert_eq!(model.predict_features(&tiny), DIFFICULTY_MIN);
    }

    #[test]
    fn a_plausible_easy_item_lands_below_a_plausible_hard_one() {
        let model = DifficultyModel::expert_prior();
        // Small, blunt, symmetric, far from the head.
        let easy = features((1.0f64 + 8.0).ln(), 0.35, 0.05, 0.10, 0.10);
        // Large, filigreed, asymmetric, hugging the scalp at full reach.
        let hard = features((1.0f64 + 180.0).ln(), 0.85, 0.60, 0.75, 0.80);

        let (easy_b, hard_b) = (
            model.predict_features(&easy),
            model.predict_features(&hard),
        );
        assert!(easy_b < hard_b, "easy={easy_b}, hard={hard_b}");
        assert!(
            easy_b < 0.0 && hard_b > 0.0,
            "the prior should straddle zero for these two: easy={easy_b}, hard={hard_b}"
        );
    }
}
