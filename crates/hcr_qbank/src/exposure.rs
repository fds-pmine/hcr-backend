//! Item exposure control.
//!
//! arona has none beyond within-session no-repeat
//! (`arona/src/qbank/static_bank.rs:146`), so this layer is ours. Without it,
//! always serving the single most informative item would hand the same handful
//! of challenges to everyone near a given ability, burning the bank and making
//! sessions predictable.

use std::collections::HashMap;

use hcr_contract::ItemId;

/// Caps how often any one item may be served.
///
/// This is the coarse rate cap. The primary mechanism is randomesque top-k
/// sampling in the bank itself; a full Sympson–Hetter scheme with per-item
/// administration probabilities would sit here for high-stakes use
/// (`docs/backend/03-DYNAMIC-QBANK.md` §7).
#[derive(Debug, Clone)]
pub struct ExposureController {
    max_rate: f64,
    /// Below this many observations the rate estimate is meaningless, so the cap
    /// is not enforced — otherwise the first item served would immediately sit at
    /// 100% and lock itself out.
    warmup: u64,
    total: u64,
    counts: HashMap<ItemId, u64>,
}

impl Default for ExposureController {
    fn default() -> Self {
        Self::new(0.2)
    }
}

impl ExposureController {
    /// Cap each item at `max_rate` of all administrations.
    pub fn new(max_rate: f64) -> Self {
        Self {
            max_rate: max_rate.clamp(0.0, 1.0),
            warmup: 20,
            total: 0,
            counts: HashMap::new(),
        }
    }

    /// Disable the cap entirely.
    pub fn unlimited() -> Self {
        Self {
            max_rate: 1.0,
            warmup: 0,
            total: 0,
            counts: HashMap::new(),
        }
    }

    /// Observations required before the cap applies.
    pub fn with_warmup(mut self, warmup: u64) -> Self {
        self.warmup = warmup;
        self
    }

    /// Whether the item may be served now.
    pub fn permits(&self, item_id: &str) -> bool {
        if self.total < self.warmup || self.max_rate >= 1.0 {
            return true;
        }
        self.rate(item_id) <= self.max_rate
    }

    /// Observed administration rate for an item.
    pub fn rate(&self, item_id: &str) -> f64 {
        if self.total == 0 {
            return 0.0;
        }
        let count = self.counts.get(item_id).copied().unwrap_or(0);
        count as f64 / self.total as f64
    }

    /// Record that an item was served.
    pub fn record(&mut self, item_id: &str) {
        self.total += 1;
        *self.counts.entry(item_id.to_string()).or_insert(0) += 1;
    }

    /// Total administrations observed.
    pub fn total(&self) -> u64 {
        self.total
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn warmup_lets_everything_through() {
        let mut controller = ExposureController::new(0.1);
        for _ in 0..5 {
            controller.record("hot");
        }
        assert!(controller.permits("hot"), "cap must not bite during warmup");
    }

    #[test]
    fn an_overexposed_item_is_withheld() {
        let mut controller = ExposureController::new(0.2).with_warmup(10);
        for index in 0..10 {
            controller.record(if index < 5 { "hot" } else { "cold" });
        }
        // 5/10 = 0.5, well over the 0.2 cap.
        assert!(!controller.permits("hot"));
        assert!(controller.permits("unseen"));
    }

    #[test]
    fn unlimited_never_withholds() {
        let mut controller = ExposureController::unlimited();
        for _ in 0..100 {
            controller.record("hot");
        }
        assert!(controller.permits("hot"));
    }
}
