//! Versioned challenge storage.

use std::collections::HashMap;
use std::sync::{Arc, RwLock};

use hcr_contract::{
    ChallengeDefinition, ChallengeDefinitionDto, ChallengeMeta, ChallengeSummary,
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

    /// Summaries of the latest version of every challenge, in stable id order.
    pub fn list(&self) -> ServiceResult<Vec<ChallengeSummary>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| ServiceError::Internal("catalog lock poisoned"))?;

        let mut summaries: Vec<ChallengeSummary> = inner
            .latest
            .iter()
            .filter_map(|(id, version)| inner.versions.get(&(id.clone(), *version)))
            .map(|dto| ChallengeSummary {
                id: dto.challenge.id.clone(),
                name: dto.challenge.name.clone(),
                description: dto.challenge.description.clone(),
            })
            .collect();

        // HashMap order is not specified; sort so listings are reproducible.
        summaries.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(summaries)
    }

    /// Build a bank snapshot over the latest version of every challenge.
    pub fn snapshot(&self) -> ServiceResult<Arc<CatalogSnapshot>> {
        let inner = self
            .inner
            .read()
            .map_err(|_| ServiceError::Internal("catalog lock poisoned"))?;

        let mut items: Vec<BankItem> = inner
            .latest
            .iter()
            .filter_map(|(id, version)| inner.versions.get(&(id.clone(), *version)))
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
