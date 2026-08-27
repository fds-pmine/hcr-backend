//! Versioned challenge storage.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use hcr_contract::{
    ChallengeDefinition, ChallengeDefinitionDto, ChallengeMeta, ChallengeSummary, ProgrammingMode,
};
use hcr_qbank::{BankItem, CatalogSnapshot};

use crate::error::{ServiceError, ServiceResult};

/// An item store keyed by `(challenge_id, version)`.
///
/// Versions are **immutable once written**. Recalibration mints a new version
/// rather than editing an existing one, which is what stops a historical score
/// from changing under a learner who has already been graded
/// (`docs/backend/README.md` decision D8).
#[derive(Debug, Default)]
pub struct CatalogStore {
    inner: RwLock<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// (id, version) -> challenge
    versions: HashMap<(String, u32), Arc<ChallengeDefinitionDto>>,
    /// id -> latest version
    latest: HashMap<String, u32>,
}

impl CatalogStore {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert or replace a version.
    ///
    /// Rewriting an existing version is refused: silently mutating a served
    /// version would move scores that have already been reported.
    pub fn insert(&self, dto: ChallengeDefinitionDto) -> ServiceResult<()> {
        let key = (dto.challenge.id.clone(), dto.meta.version);
        let mut inner = self
            .inner
            .write()
            .map_err(|_| ServiceError::Internal("catalog lock poisoned"))?;

        if inner.versions.contains_key(&key) {
            return Err(ServiceError::Internal(
                "refusing to overwrite an existing challenge version",
            ));
        }

        let latest = inner.latest.entry(key.0.clone()).or_insert(dto.meta.version);
        if dto.meta.version > *latest {
            *latest = dto.meta.version;
        }
        inner.versions.insert(key, Arc::new(dto));
        Ok(())
    }

    /// Convenience: store a challenge with fresh metadata.
    pub fn insert_challenge(
        &self,
        challenge: ChallengeDefinition,
        meta: ChallengeMeta,
    ) -> ServiceResult<()> {
        self.insert(ChallengeDefinitionDto { challenge, meta })
    }

    /// Fetch a specific version, or the latest when `version` is `None`.
    pub fn get(
        &self,
        challenge_id: &str,
        version: Option<u32>,
    ) -> ServiceResult<Arc<ChallengeDefinitionDto>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| ServiceError::Internal("catalog lock poisoned"))?;

        let resolved = match version {
            Some(v) => v,
            None => *inner.latest.get(challenge_id).ok_or_else(|| {
                ServiceError::ChallengeNotFound {
                    challenge_id: challenge_id.to_string(),
                    version: None,
                }
            })?,
        };

        inner
            .versions
            .get(&(challenge_id.to_string(), resolved))
            .cloned()
            .ok_or(ServiceError::ChallengeNotFound {
                challenge_id: challenge_id.to_string(),
                version,
            })
    }

    /// Summaries of the latest version of every challenge.
    ///
    /// # Ordering is part of the contract
    ///
    /// Hand-authored challenges lead, then generated ones, each group by id.
    /// Both properties are load-bearing:
    ///
    /// * **Reproducible.** `HashMap` iteration order is unspecified, so without
    ///   a sort two identical catalogs would list differently.
    /// * **Meaningful.** A client with no other signal opens the *first* entry,
    ///   and a plain id sort made that an accident of the alphabet — generated
    ///   ids begin `cap-trim-…`, so a provisional machine-made item outranked
    ///   the authored one it was generated from. A listing is a menu; the
    ///   authored, calibrated challenges are the ones to meet first.
    ///
    /// A caller that needs a *specific* item must still name it. This ordering
    /// makes "the first one" a defensible default, not a substitute for asking.
    pub fn list(&self) -> ServiceResult<Vec<ChallengeSummary>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| ServiceError::Internal("catalog lock poisoned"))?;

        let mut rows: Vec<(bool, ChallengeSummary)> = inner
            .latest
            .iter()
            .filter_map(|(id, version)| inner.versions.get(&(id.clone(), *version)))
            .map(|dto| {
                (
                    dto.meta.generator.is_some(),
                    ChallengeSummary {
                        id: dto.challenge.id.clone(),
                        name: dto.challenge.name.clone(),
                        description: dto.challenge.description.clone(),
                    },
                )
            })
            .collect();

        rows.sort_by(|(a_generated, a), (b_generated, b)| {
            a_generated.cmp(b_generated).then_with(|| a.id.cmp(&b.id))
        });
        Ok(rows.into_iter().map(|(_, summary)| summary).collect())
    }

    /// Pick an item to run a competitive round on.
    ///
    /// `Provisional` items are deliberately **allowed**: a round ranks players
    /// against each other on an identical item, so the ranking is valid whatever
    /// the item's `b` turns out to be. That is the contract's own position
    /// (`CalibrationState::Provisional`), and it is why the `07-CALIBRATION.md`
    /// §8 objection to uncalibrated items does not carry over — that objection
    /// is about *measuring* an ability, which a round does not do.
    ///
    /// `Retired` items are refused. An item withdrawn as drifted or pathological
    /// should not be the thing that decides who won.
    ///
    /// Beyond that this takes the first entry in [`Self::list`]'s order, so an
    /// unpinned round lands on an authored, human-checked challenge rather than
    /// on whichever generated id happened to sort first.
    pub fn pick_for_match(&self, mode: ProgrammingMode) -> ServiceResult<(String, u32)> {
        let inner = self
            .inner
            .read()
            .map_err(|_| ServiceError::Internal("catalog lock poisoned"))?;

        inner
            .latest
            .iter()
            .filter_map(|(id, version)| inner.versions.get(&(id.clone(), *version)))
            .filter(|dto| dto.meta.calibration.servable())
            // Cutter Grid needs a certified planner profile, so most items
            // cannot be played in it. Serving one anyway would open a round on a
            // challenge nobody in it could attempt.
            .filter(|dto| dto.meta.supports(mode))
            .min_by(|a, b| {
                a.meta
                    .generator
                    .is_some()
                    .cmp(&b.meta.generator.is_some())
                    .then_with(|| a.challenge.id.cmp(&b.challenge.id))
            })
            .map(|dto| (dto.challenge.id.clone(), dto.meta.version))
            .ok_or(ServiceError::BankExhausted)
    }

    /// Build a bank snapshot over the latest version of every challenge.
    pub fn snapshot(&self) -> ServiceResult<Arc<CatalogSnapshot>> {
        self.snapshot_for(ProgrammingMode::Servo)
    }

    /// Build a bank snapshot of the items playable in `mode`.
    ///
    /// Filtering here rather than at selection time is deliberate: arona picks
    /// the item with the most information at the learner's θ, and an item it
    /// cannot legally serve must not be in the pool it is choosing from. Handing
    /// it the full bank and rejecting the choice afterwards would make selection
    /// silently worse — it would keep picking the most informative item, be
    /// refused, and fall back to a less informative one — while looking like it
    /// was working.
    pub fn snapshot_for(&self, mode: ProgrammingMode) -> ServiceResult<Arc<CatalogSnapshot>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| ServiceError::Internal("catalog lock poisoned"))?;

        let mut items: Vec<BankItem> = inner
            .latest
            .iter()
            .filter_map(|(id, version)| inner.versions.get(&(id.clone(), *version)))
            .filter(|dto| dto.meta.supports(mode))
            .map(|dto| BankItem::new(dto.challenge.id.clone(), dto.meta.clone()))
            .collect();

        items.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(CatalogSnapshot::new(items))
    }

    /// Number of stored versions across all challenges.
    pub fn len(&self) -> usize {
        self.inner.read().map(|i| i.versions.len()).unwrap_or(0)
    }

    /// Whether the store holds nothing.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
