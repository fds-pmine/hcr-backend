//! Assembling an arona [`Session`] with settings suited to HCR.

use arona::estimation::EAPEstimator;
use arona::selection::selectors::{MaxInformationSelector, WarmupSelector};
use arona::termination::HybridTerminator;
use arona::{Ability, Session};

use crate::bank::HcrDynamicBank;

/// Tunables for an adaptive session.
///
/// The defaults deliberately diverge from arona's own example
/// (`arona/examples/interactive_demo.rs:57-166`), which is written for
/// multiple-choice items answered in seconds. An HCR item is a multi-minute
/// programming task, so both the warmup and the length budget have to be far
/// smaller.
#[derive(Debug, Clone, Copy)]
pub struct SessionConfig {
    /// Items before the session may stop early on precision.
    pub min_items: usize,
    /// Hard ceiling on items.
    pub max_items: usize,
    /// Standard error at which measurement is precise enough.
    pub target_se: f64,
    /// Random items served before adaptive selection engages.
    pub warmup_items: usize,
    /// Prior mean for the ability estimate.
    pub prior_mean: f64,
    /// Prior standard deviation.
    pub prior_sd: f64,
    /// How far below maximum information a candidate may fall.
    pub tolerance: f64,
    /// Items less discriminating than this are skipped.
    pub min_discrimination: f64,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            min_items: 4,
            max_items: 12,
            target_se: 0.40,
            // Two, not arona's five: a five-item random warmup would eat most of
            // a session when each item takes minutes.
            warmup_items: 2,
            prior_mean: 0.0,
            prior_sd: 1.0,
            tolerance: 1.0,
            min_discrimination: 0.4,
        }
    }
}

/// Build a session over `bank`.
///
/// Three choices are load-bearing:
///
/// * **EAP, not MLE.** Maximum likelihood is undefined until the learner has both
///   a pass and a fail, and an all-correct opening is common when items are
///   chosen well. EAP is stable from the first response via its prior.
/// * **Hybrid termination, never SE-threshold alone.** With a finite bank, chasing
///   a target standard error can exhaust the pool and fail with
///   `BankExhausted`; the item-count ceiling bounds that.
/// * **A warmup selector.** Early adaptive selection off a near-flat prior mostly
///   samples noise, so the first couple of items are drawn at random.
pub fn build_session(
    bank: HcrDynamicBank,
    config: SessionConfig,
    initial_theta: f64,
) -> Session {
    Session::new(
        Box::new(WarmupSelector::new(
            MaxInformationSelector {
                tolerance: config.tolerance,
                min_discrimination: config.min_discrimination,
            },
            config.warmup_items,
        )),
        Box::new(bank),
        Box::new(EAPEstimator::new(config.prior_mean, config.prior_sd)),
        Box::new(HybridTerminator::new(
            config.min_items,
            config.max_items,
            config.target_se,
        )),
        Ability(initial_theta),
    )
}
