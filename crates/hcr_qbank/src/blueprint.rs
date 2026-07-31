//! Content balancing across skill dimensions.

use std::collections::{BTreeMap, HashMap};

use hcr_contract::SkillDimension;

/// Target mix of skill dimensions across a session.
///
/// arona's `SelectionHints` carries a `used_types: HashMap<String, u32>` intended
/// for exactly this, and `StaticQBank` never reads it
/// (`arona/src/selection/hints.rs:111-117`). Our bank honours it, which turns an
/// inert field into constrained CAT at almost no cost.
#[derive(Debug, Clone, Default)]
pub struct Blueprint {
    targets: BTreeMap<SkillDimension, f64>,
    tolerance: f64,
}

impl Blueprint {
    /// No constraint: every item is eligible.
    pub fn unconstrained() -> Self {
        Self::default()
    }

    /// Equal share for every dimension.
    pub fn uniform() -> Self {
        let share = 1.0 / SkillDimension::ALL.len() as f64;
        Self::new(SkillDimension::ALL.iter().map(|d| (*d, share)).collect())
    }

    /// Explicit target proportions. Values need not sum to 1; they are compared
    /// against observed proportions independently.
    pub fn new(targets: BTreeMap<SkillDimension, f64>) -> Self {
        Self {
            targets,
            tolerance: 0.05,
        }
    }

    /// Allowed overshoot before a dimension is considered saturated.
    pub fn with_tolerance(mut self, tolerance: f64) -> Self {
        self.tolerance = tolerance;
        self
    }

    /// Whether any constraint is active.
    pub fn is_unconstrained(&self) -> bool {
        self.targets.is_empty()
    }

    /// Whether an item covering `dimensions` may still be served.
    ///
    /// An item qualifies if **at least one** of its dimensions is under quota.
    /// Requiring all of them would deadlock: a multi-dimension item would become
    /// unservable as soon as any single dimension filled up.
    pub fn allows(&self, dimensions: &[SkillDimension], used_types: &HashMap<String, u32>) -> bool {
        if self.targets.is_empty() || dimensions.is_empty() {
            return true;
        }

        let total: u32 = used_types.values().sum();
        if total == 0 {
            return true;
        }

        dimensions.iter().any(|dimension| {
            let Some(target) = self.targets.get(dimension) else {
                // Undeclared dimensions are unconstrained.
                return true;
            };
            let served = used_types.get(dimension.as_str()).copied().unwrap_or(0);
            let proportion = f64::from(served) / f64::from(total);
            proportion <= target + self.tolerance
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn used(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), *v))
            .collect()
    }

    #[test]
    fn unconstrained_blueprint_allows_everything() {
        let blueprint = Blueprint::unconstrained();
        assert!(blueprint.allows(&[SkillDimension::Precision], &used(&[("precision", 99)])));
    }

    #[test]
    fn a_saturated_dimension_is_excluded() {
        let blueprint = Blueprint::uniform().with_tolerance(0.0);
        // 8 of 10 served are precision, far above the 0.2 uniform target.
        let counts = used(&[("precision", 8), ("safety", 2)]);
        assert!(!blueprint.allows(&[SkillDimension::Precision], &counts));
        assert!(blueprint.allows(&[SkillDimension::Safety], &counts));
    }

    #[test]
    fn multi_dimension_items_qualify_on_any_under_quota_dimension() {
        let blueprint = Blueprint::uniform().with_tolerance(0.0);
        let counts = used(&[("precision", 8), ("safety", 2)]);
        // Precision is saturated but kinematics has had nothing, so the item runs.
        assert!(blueprint.allows(
            &[SkillDimension::Precision, SkillDimension::Kinematics],
            &counts
        ));
    }

    #[test]
    fn nothing_is_blocked_before_anything_has_been_served() {
        let blueprint = Blueprint::uniform().with_tolerance(0.0);
        assert!(blueprint.allows(&[SkillDimension::Precision], &HashMap::new()));
    }
}
