//! The item pool.

use std::collections::HashMap;
use std::sync::Arc;

use hcr_contract::{ChallengeMeta, ItemId, SkillDimension};

/// One selectable item: an HCR challenge plus its psychometric metadata.
///
/// The challenge *content* is not held here. The bank only needs identity and
/// parameters to choose; the service loads the challenge itself from the catalog
/// store once an item has been picked.
#[derive(Debug, Clone)]
pub struct BankItem {
    /// Stable item identity.
    pub id: ItemId,
    /// Parameters, calibration state and dimensions.
    pub meta: ChallengeMeta,
}

impl BankItem {
    /// Build an item.
    pub fn new(id: impl Into<ItemId>, meta: ChallengeMeta) -> Self {
        Self {
            id: id.into(),
            meta,
        }
    }

    /// Whether the item exercises `dimension`.
    pub fn has_dimension(&self, dimension: SkillDimension) -> bool {
        self.meta.dimensions.contains(&dimension)
    }

    /// Whether any of the item's dimensions matches this tag.
    pub fn matches_field(&self, field: &str) -> bool {
        self.meta
            .dimensions
            .iter()
            .any(|d| d.as_str().eq_ignore_ascii_case(field))
    }
}

/// An immutable view of the pool.
///
/// Held behind an `Arc` and swapped atomically when the catalog changes, so a
/// selection can never observe the pool mutating underneath it. This is what
/// makes the bank *dynamic* without making it racy — `StaticQBank` is fixed at
/// construction and has no add/remove at all
/// (`arona/src/qbank/static_bank.rs:191`).
#[derive(Debug, Clone, Default)]
pub struct CatalogSnapshot {
    items: Vec<BankItem>,
    by_id: HashMap<ItemId, usize>,
}

impl CatalogSnapshot {
    /// Build a snapshot. Later duplicates of an id win the index entry.
    pub fn new(items: Vec<BankItem>) -> Arc<Self> {
        let by_id = items
            .iter()
            .enumerate()
            .map(|(index, item)| (item.id.clone(), index))
            .collect();
        Arc::new(Self { items, by_id })
    }

    /// All items, in catalog order.
    pub fn items(&self) -> &[BankItem] {
        &self.items
    }

    /// Number of items.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether the pool is empty.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Look an item up by identity.
    pub fn get(&self, id: &str) -> Option<&BankItem> {
        self.by_id.get(id).map(|index| &self.items[*index])
    }
}
