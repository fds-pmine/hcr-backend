//! Authoritative replay, with backpressure and caching.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use hcr_contract::{ChallengeDefinition, ChallengeDefinitionDto, ClientPreview, Program};
use hcr_sim::{ReplayOptions, ReplayOutcome};
use sha2::{Digest, Sha256};
use tokio::sync::Semaphore;

use crate::error::{ServiceError, ServiceResult};

/// Build identifier reported with every score, so a result can be traced to the
/// engine that produced it.
pub const ENGINE_VERSION: &str = concat!("hcr_sim/", env!("CARGO_PKG_VERSION"));

/// Cache key: an outcome depends on nothing but these three.
type CacheKey = (String, u32, String);

/// Runs replays off the async runtime, with a bounded queue and a result cache.
#[derive(Debug)]
pub struct ReplayPool {
    permits: Arc<Semaphore>,
    cache: Mutex<HashMap<CacheKey, Arc<ReplayOutcome>>>,
    cache_capacity: usize,
    options: ReplayOptions,
}

impl ReplayPool {
    /// Build a pool allowing `concurrency` simultaneous replays.
    pub fn new(concurrency: usize, options: ReplayOptions) -> Self {
        Self {
            permits: Arc::new(Semaphore::new(concurrency.max(1))),
            cache: Mutex::new(HashMap::new()),
            cache_capacity: 1024,
            options,
        }
    }

    /// A pool sized for the current machine.
    pub fn with_default_concurrency() -> Self {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get().saturating_sub(1).max(1))
            .unwrap_or(1);
        Self::new(cores, ReplayOptions::default())
    }

    /// Bound on cached outcomes.
    pub fn with_cache_capacity(mut self, capacity: usize) -> Self {
        self.cache_capacity = capacity;
        self
    }

    /// Replay options in force.
    pub fn options(&self) -> ReplayOptions {
        self.options
    }

    /// Score a program against a challenge.
    ///
    /// Replay is CPU-bound — up to 500 commands each sweeping a tool against
    /// thousands of voxels — so it runs on the blocking pool rather than starving
    /// the async runtime. Capacity is bounded: when every permit is taken the
    /// call fails fast with [`ServiceError::RateLimited`] instead of queueing
    /// unboundedly, because a program with maximal repeats against a dense voxel
    /// field is otherwise a denial-of-service primitive.
    pub async fn replay(
        &self,
        dto: &ChallengeDefinitionDto,
        program: &Program,
    ) -> ServiceResult<Arc<ReplayOutcome>> {
        let key = (
            dto.challenge.id.clone(),
            dto.meta.version,
            program_hash(program)?,
        );

        if let Some(cached) = self.lookup(&key) {
            return Ok(cached);
        }

        let permit = Arc::clone(&self.permits)
            .try_acquire_owned()
            .map_err(|_| ServiceError::RateLimited)?;

        let challenge: ChallengeDefinition = dto.challenge.clone();
        let program = program.clone();
        let options = self.options;

        let outcome = tokio::task::spawn_blocking(move || {
            // Held for the duration of the blocking work, released on return.
            let _permit = permit;
            hcr_sim::replay(&challenge, &program, options)
        })
        .await
        .map_err(|_| ServiceError::Internal("replay task failed to complete"))??;

        let outcome = Arc::new(outcome);
        self.store(key, Arc::clone(&outcome));
        Ok(outcome)
    }

    fn lookup(&self, key: &CacheKey) -> Option<Arc<ReplayOutcome>> {
        self.cache.lock().ok()?.get(key).cloned()
    }

    fn store(&self, key: CacheKey, outcome: Arc<ReplayOutcome>) {
        let Ok(mut cache) = self.cache.lock() else {
            return;
        };
        // Deliberately crude: a full clear keeps memory bounded without pulling
        // in an LRU. Replays are deterministic and cheap to recompute, so a cold
        // cache costs latency, never correctness.
        if cache.len() >= self.cache_capacity {
            cache.clear();
        }
        cache.insert(key, outcome);
    }

    /// Number of cached outcomes.
    pub fn cached(&self) -> usize {
        self.cache.lock().map(|c| c.len()).unwrap_or(0)
    }
}

/// Stable hash of a program, used as a cache key.
///
/// Serde emits struct fields in declaration order, so this is stable within a
/// build. It keys a cache, not a signature — a collision would serve a stale
/// score, which is why the challenge id and version are part of the key too.
pub fn program_hash(program: &Program) -> ServiceResult<String> {
    let encoded = serde_json::to_vec(program)
        .map_err(|_| ServiceError::Internal("failed to encode program"))?;
    let digest = Sha256::digest(&encoded);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// Whether the client's own run disagrees with the authoritative one.
///
/// Compares result hashes exactly. That is stricter than the Jaccard tolerance in
/// `docs/backend/02-DETERMINISM.md` §4 — the client sends a hash, not a voxel
/// set, so a partial comparison is not available — but it is the right trade
/// here: this drives an operator alarm, not a user-facing failure, and the
/// conformance suite shows the two engines agreeing exactly in practice.
pub fn diverged(preview: Option<&ClientPreview>, outcome: &ReplayOutcome) -> bool {
    match preview {
        None => false,
        Some(preview) => preview.result_voxels_hash != outcome.result_voxels_hash,
    }
}
