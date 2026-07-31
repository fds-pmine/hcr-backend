//! Bridging replay outcomes into arona.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use arona::{QuestionContent, RenderFormat, Score};
use hcr_contract::ItemId;

use crate::mastery::remap_for_arona;

/// Where replayed scores wait to be picked up by arona.
///
/// # Why this indirection exists
///
/// `QuestionContent::score(&self, answer) -> Score`
/// (`arona/src/content/traits.rs:134`) is **synchronous and infallible**. But an
/// HCR response is a Program IR that has to be replayed server-side, which is
/// expensive and asynchronous. So the flow inverts: the session actor replays
/// first, records the outcome here, and only then calls
/// `Session::submit_response(submission_id)`. By the time arona asks for a score,
/// the answer is already sitting in this map.
///
/// Cheap to clone; all clones share one store.
#[derive(Debug, Clone, Default)]
pub struct OutcomeStore {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    /// submission id -> raw normalized score in [0,1]
    scores: Mutex<HashMap<String, f64>>,
    /// Counts lookups that found nothing — see [`OutcomeStore::misses`].
    misses: AtomicU64,
}

impl OutcomeStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a replayed outcome. `raw_score` is the normalized final score in
    /// `[0,1]` (i.e. `finalScore / 100`).
    pub fn record(&self, submission_id: impl Into<String>, raw_score: f64) {
        self.inner
            .scores
            .lock()
            .expect("outcome store poisoned")
            .insert(submission_id.into(), raw_score.clamp(0.0, 1.0));
    }

    /// Look up a recorded outcome.
    pub fn get(&self, submission_id: &str) -> Option<f64> {
        self.inner
            .scores
            .lock()
            .expect("outcome store poisoned")
            .get(submission_id)
            .copied()
    }

    /// Forget a recorded outcome.
    pub fn remove(&self, submission_id: &str) -> Option<f64> {
        self.inner
            .scores
            .lock()
            .expect("outcome store poisoned")
            .remove(submission_id)
    }

    /// How many score lookups found nothing.
    ///
    /// `score()` cannot fail — it returns a `Score`, not a `Result` — so a
    /// missing outcome would otherwise be silently scored zero and quietly
    /// corrupt an ability estimate. This counter makes that visible; a healthy
    /// system keeps it at zero, and it is worth alerting on.
    pub fn misses(&self) -> u64 {
        self.inner.misses.load(Ordering::Relaxed)
    }

    fn record_miss(&self) {
        self.inner.misses.fetch_add(1, Ordering::Relaxed);
    }
}

/// arona-facing content for one HCR challenge.
///
/// Deliberately a lightweight handle, not the challenge itself: `Question` is not
/// `Clone` and `select_question` returns it by value
/// (`arona/src/qbank/traits.rs:240`), so a fresh one is constructed per
/// selection. Copying a whole voxel challenge each time would be wasteful.
#[derive(Debug, Clone)]
pub struct ChallengeContent {
    item_id: ItemId,
    version: u32,
    mastery_threshold: f64,
    outcomes: OutcomeStore,
}

impl ChallengeContent {
    /// Build content for an item.
    pub fn new(
        item_id: impl Into<ItemId>,
        version: u32,
        mastery_threshold: f64,
        outcomes: OutcomeStore,
    ) -> Self {
        Self {
            item_id: item_id.into(),
            version,
            mastery_threshold,
            outcomes,
        }
    }

    /// The item this content stands for.
    pub fn item_id(&self) -> &str {
        &self.item_id
    }

    /// Version served.
    pub fn version(&self) -> u32 {
        self.version
    }
}

impl QuestionContent for ChallengeContent {
    /// A handle, not display text. The learner sees the challenge rendered by the
    /// frontend; arona only needs something stable to log.
    fn render(&self, _format: RenderFormat) -> String {
        format!("{}@{}", self.item_id, self.version)
    }

    /// `answer` is the submission id; the score comes from the recorded replay.
    fn score(&self, answer: &str) -> Score {
        match self.outcomes.get(answer) {
            Some(raw) => Score::new(remap_for_arona(raw, self.mastery_threshold)),
            None => {
                self.outcomes.record_miss();
                // Zero is the safe default: it cannot inflate an estimate. The
                // miss counter is how this gets noticed.
                Score::new(0.0)
            }
        }
    }

    /// Zero. A robot-programming task cannot be guessed, which is why these items
    /// are modelled as 2PL rather than 3PL.
    fn guessing_probability(&self) -> f64 {
        0.0
    }

    fn generate_feedback(&self, _answer: &str, score: Score) -> String {
        if score.is_correct() {
            format!("Mastered {}.", self.item_id)
        } else {
            format!("Not yet mastered: {}.", self.item_id)
        }
    }

    fn clone_box(&self) -> Box<dyn QuestionContent> {
        Box::new(self.clone())
    }
}
