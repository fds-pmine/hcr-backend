//! A dynamic, exposure-controlled [`QuestionBank`].

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use arona::qbank::{QBankError, QBankStats, QuestionBank};
use arona::selection::SelectionHints;
use arona::selection::selectors::{MaxInfoParams, NearestDiffParams};
use arona::{Ability, Difficulty, Discrimination, GuessingParam, IRTParameters, Question};
use hcr_contract::ItemId;
use rand::distributions::WeightedIndex;
use rand::prelude::*;
use rand::rngs::StdRng;

use crate::blueprint::Blueprint;
use crate::content::{ChallengeContent, OutcomeStore};
use crate::difficulty::DifficultyModel;
use crate::exposure::ExposureController;
use crate::generator::{ChallengeGenerator, GeneratedItem};
use crate::item::{BankItem, CatalogSnapshot};

/// Information multiplier when an item's tags match the learner's declared field.
///
/// Mirrors arona's own soft boost (`arona/src/qbank/static_bank.rs:322-328`) so
/// field-aware selection behaves the same whichever bank is in use.
pub const FIELD_BOOST: f64 = 1.3;

/// Default randomesque pool size.
const DEFAULT_RANDOMESQUE_K: usize = 5;

/// Default parameter vectors tried when generating an item at a target difficulty.
const DEFAULT_GENERATION_ATTEMPTS: u32 = 24;

/// An item that was handed out, in serve order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedItem {
    /// Which item.
    pub id: ItemId,
    /// Which version — pinned, so later recalibration cannot move this response.
    pub version: u32,
}

/// A question bank over a mutable catalog.
///
/// Everything here is deliberately outside arona, because `StaticQBank` provides
/// none of it: the pool is fixed at construction, exposure is uncontrolled,
/// `used_types` is ignored, and selection reaches for `thread_rng()` so sessions
/// cannot be reproduced. `QuestionBank` being object-safe with only three
/// required methods (`arona/src/qbank/traits.rs:210-300`) makes a replacement the
/// intended extension point rather than a fight with the library.
#[derive(Debug)]
pub struct HcrDynamicBank {
    catalog: Arc<CatalogSnapshot>,
    blueprint: Blueprint,
    exposure: ExposureController,
    outcomes: OutcomeStore,
    served: Vec<ServedItem>,
    used: HashSet<ItemId>,
    rng: StdRng,
    randomesque_k: usize,
    measurement_only: bool,
    generator: Option<Arc<dyn ChallengeGenerator>>,
    model: DifficultyModel,
    generation_attempts: u32,
    generated: Vec<GeneratedItem>,
}

impl HcrDynamicBank {
    /// Build a bank over `catalog`, seeded for reproducibility.
    ///
    /// The seed matters: a session can be replayed exactly for debugging or
    /// audit, which arona's own `thread_rng()`-based selection cannot offer.
    pub fn new(catalog: Arc<CatalogSnapshot>, outcomes: OutcomeStore, seed: u64) -> Self {
        Self {
            catalog,
            blueprint: Blueprint::unconstrained(),
            exposure: ExposureController::default(),
            outcomes,
            served: Vec::new(),
            used: HashSet::new(),
            rng: StdRng::seed_from_u64(seed),
            randomesque_k: DEFAULT_RANDOMESQUE_K,
            measurement_only: true,
            generator: None,
            model: DifficultyModel::expert_prior(),
            generation_attempts: DEFAULT_GENERATION_ATTEMPTS,
            generated: Vec::new(),
        }
    }

    /// Attach a generator, so the bank can synthesise an item when the pool has
    /// nothing in the target difficulty band.
    ///
    /// Generation only fires when uncalibrated items are permitted
    /// ([`Self::allow_uncalibrated`]). A freshly generated item is by definition
    /// `Provisional`, so serving one into an adaptive *measurement* session would
    /// contradict the rule that θ must not rest on an uncalibrated item. In a
    /// competitive round it is exactly right — ranking is valid whatever `b`
    /// turns out to be (`docs/backend/07-CALIBRATION.md` §8).
    pub fn with_generator(mut self, generator: Arc<dyn ChallengeGenerator>) -> Self {
        self.generator = Some(generator);
        self
    }

    /// Difficulty model used to place generated items.
    pub fn with_difficulty_model(mut self, model: DifficultyModel) -> Self {
        self.model = model;
        self
    }

    /// Parameter vectors tried per generation request.
    pub fn with_generation_attempts(mut self, attempts: u32) -> Self {
        self.generation_attempts = attempts.max(1);
        self
    }

    /// Take the items generated so far, so the service can persist them with
    /// their provenance. Generated items are only reproducible if the
    /// `(family, version, seed, params)` tuple is stored.
    pub fn take_generated(&mut self) -> Vec<GeneratedItem> {
        std::mem::take(&mut self.generated)
    }

    /// Number of items generated during this session.
    pub fn generated_count(&self) -> usize {
        self.generated.len()
    }

    /// Apply a content blueprint.
    pub fn with_blueprint(mut self, blueprint: Blueprint) -> Self {
        self.blueprint = blueprint;
        self
    }

    /// Apply an exposure policy.
    pub fn with_exposure(mut self, exposure: ExposureController) -> Self {
        self.exposure = exposure;
        self
    }

    /// Size of the randomesque pool. `1` is pure max-information.
    pub fn with_randomesque_k(mut self, k: usize) -> Self {
        self.randomesque_k = k.max(1);
        self
    }

    /// Permit `provisional` items.
    ///
    /// Correct for competitive rounds — ranking is valid whatever `b` is, since
    /// everyone faces the same item — and wrong for measurement, where an
    /// uncalibrated item would contaminate the ability estimate
    /// (`docs/backend/07-CALIBRATION.md` §8).
    pub fn allow_uncalibrated(mut self, allow: bool) -> Self {
        self.measurement_only = !allow;
        self
    }

    /// Items served so far, in order. Index `i` is arona's bank index `i`.
    pub fn served(&self) -> &[ServedItem] {
        &self.served
    }

    /// Resolve an arona bank index back to an item.
    ///
    /// arona addresses items by `Vec` index and `Question` carries no id
    /// (`arona/src/core/question.rs:159-165`), so this map is what lets the
    /// service turn a selection back into something nameable — and it is what the
    /// signed `itemRef` token carries across the wire.
    pub fn item_at(&self, index: usize) -> Option<&ServedItem> {
        self.served.get(index)
    }

    /// The shared outcome store.
    pub fn outcomes(&self) -> &OutcomeStore {
        &self.outcomes
    }

    /// Swap in a new catalog snapshot.
    ///
    /// Already-served items stay served: their versions are pinned in `served`,
    /// so a mid-session catalog change cannot rewrite history.
    pub fn replace_catalog(&mut self, catalog: Arc<CatalogSnapshot>) {
        self.catalog = catalog;
    }

    fn irt_of(item: &BankItem) -> IRTParameters {
        IRTParameters::new(
            Discrimination(item.meta.irt.discrimination),
            Difficulty(item.meta.irt.difficulty),
            GuessingParam(item.meta.irt.guessing),
        )
    }

    /// Weight a candidate, or `None` to exclude it.
    fn weigh(
        &self,
        item: &BankItem,
        theta: Ability,
        target_difficulty: f64,
        max_info: Option<&MaxInfoParams>,
        nearest: Option<&NearestDiffParams>,
        field: Option<&str>,
    ) -> Option<f64> {
        // A pure nearest-difficulty selector asked for a band; honour it.
        if max_info.is_none() {
            if let Some(params) = nearest {
                let distance = (item.meta.irt.difficulty - target_difficulty).abs();
                if distance > params.range {
                    return None;
                }
                return Some(1.0 / (1.0 + distance));
            }
        }

        // Otherwise rank by Fisher information at the current ability estimate.
        // 2PL, not 3PL: guessing is zero for these items, and the extra parameter
        // would only add noise.
        let mut information = Self::irt_of(item).information_2pl(theta);

        if let Some(field) = field {
            if item.matches_field(field) {
                information *= FIELD_BOOST;
            }
        }

        Some(information)
    }

    /// Synthesise an item near `target_difficulty` and add it to the pool.
    ///
    /// Returns its index in the (now extended) catalog, or `None` if no generator
    /// is attached, generation is not permitted, or the search failed.
    fn generate_at(&mut self, target_difficulty: f64) -> Option<usize> {
        // A generated item is Provisional; serving one into a measurement session
        // would rest an ability estimate on an uncalibrated item.
        if self.measurement_only {
            return None;
        }

        let generator = self.generator.clone()?;
        let seed = self.rng.r#gen::<u64>();
        let generated =
            generator.solve_for_difficulty(target_difficulty, &self.model, seed, self.generation_attempts)?;

        let item = BankItem::new(
            generated.dto.challenge.id.clone(),
            generated.dto.meta.clone(),
        );

        // Rebuild the snapshot rather than mutating it: snapshots are immutable
        // by design so a selection can never observe the pool changing.
        let mut items = self.catalog.items().to_vec();
        items.push(item);
        self.catalog = CatalogSnapshot::new(items);
        self.generated.push(generated);

        Some(self.catalog.len() - 1)
    }

    /// Record a selection and build the arona `Question` for it.
    fn serve(&mut self, index: usize) -> Question {
        let catalog = Arc::clone(&self.catalog);
        let item = &catalog.items()[index];

        self.used.insert(item.id.clone());
        self.exposure.record(&item.id);
        self.served.push(ServedItem {
            id: item.id.clone(),
            version: item.meta.version,
        });

        let tags = item
            .meta
            .dimensions
            .iter()
            .map(|d| d.as_str().to_string())
            .collect();

        Question::with_tags(
            Box::new(ChallengeContent::new(
                item.id.clone(),
                item.meta.version,
                item.meta.mastery_threshold,
                self.outcomes.clone(),
            )),
            Self::irt_of(item),
            tags,
        )
    }

    /// Pick from the top-k by weight.
    ///
    /// Taking the single best every time is what burns a bank: everyone near the
    /// same ability would see the same items in the same order.
    fn sample(&mut self, candidates: &[(usize, f64)]) -> usize {
        if candidates.len() == 1 {
            return candidates[0].0;
        }

        let weights: Vec<f64> = candidates.iter().map(|(_, w)| w.max(0.0)).collect();
        match WeightedIndex::new(&weights) {
            Ok(distribution) => candidates[distribution.sample(&mut self.rng)].0,
            // All weights zero or non-finite — fall back to uniform rather than
            // failing a selection over a degenerate information surface.
            Err(_) => candidates[self.rng.gen_range(0..candidates.len())].0,
        }
    }
}

impl QuestionBank for HcrDynamicBank {
    fn select_question(&mut self, hints: &SelectionHints) -> Result<Question, QBankError> {
        let theta = hints
            .params
            .get::<Ability>()
            .copied()
            .unwrap_or(Ability(0.0));
        let max_info = hints.params.get::<MaxInfoParams>();
        let nearest = hints.params.get::<NearestDiffParams>();
        let field = hints.user_field().map(|hint| hint.field.clone());
        let target_difficulty = hints.target_difficulty.map_or(theta.0, |d| d.0);
        let min_discrimination = max_info.map_or(0.0, |p| p.min_discrimination);

        let catalog = Arc::clone(&self.catalog);
        let mut candidates: Vec<(usize, f64)> = Vec::new();

        for (index, item) in catalog.items().iter().enumerate() {
            if self.used.contains(&item.id) {
                continue;
            }
            if !item.meta.calibration.servable() {
                continue;
            }
            if self.measurement_only && !item.meta.calibration.usable_for_measurement() {
                continue;
            }
            if item.meta.irt.discrimination < min_discrimination {
                continue;
            }
            if !self
                .blueprint
                .allows(&item.meta.dimensions, &hints.used_types)
            {
                continue;
            }
            if !self.exposure.permits(&item.id) {
                continue;
            }

            if let Some(weight) = self.weigh(
                item,
                theta,
                target_difficulty,
                max_info,
                nearest,
                field.as_deref(),
            ) {
                candidates.push((index, weight));
            }
        }

        if candidates.is_empty() {
            // Nothing in the pool fits. Synthesise one rather than failing the
            // session — this is what makes the bank dynamic rather than merely
            // mutable.
            if let Some(index) = self.generate_at(target_difficulty) {
                return Ok(self.serve(index));
            }
            return Err(QBankError::NoQuestionAvailable);
        }

        candidates.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal));
        candidates.truncate(self.randomesque_k);

        let chosen = self.sample(&candidates);
        Ok(self.serve(chosen))
    }

    fn stats(&self) -> QBankStats {
        let items = self.catalog.items();
        let servable: Vec<&BankItem> = items
            .iter()
            .filter(|item| item.meta.calibration.servable())
            .collect();

        let available = servable
            .iter()
            .filter(|item| !self.used.contains(&item.id))
            .count();

        let (mut min_b, mut max_b) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut min_a, mut max_a) = (f64::INFINITY, f64::NEG_INFINITY);
        let (mut sum_b, mut sum_a) = (0.0, 0.0);

        for item in &servable {
            let b = item.meta.irt.difficulty;
            let a = item.meta.irt.discrimination;
            min_b = min_b.min(b);
            max_b = max_b.max(b);
            min_a = min_a.min(a);
            max_a = max_a.max(a);
            sum_b += b;
            sum_a += a;
        }

        let count = servable.len();
        if count == 0 {
            // Avoid emitting infinities into a struct other code will read.
            min_b = 0.0;
            max_b = 0.0;
            min_a = 0.0;
            max_a = 0.0;
        }

        QBankStats {
            total_questions: items.len(),
            used_questions: self.used.len(),
            available_questions: available,
            difficulty_range: (Difficulty(min_b), Difficulty(max_b)),
            discrimination_range: (Discrimination(min_a), Discrimination(max_a)),
            avg_difficulty: if count == 0 {
                0.0
            } else {
                sum_b / count as f64
            },
            avg_discrimination: if count == 0 {
                0.0
            } else {
                sum_a / count as f64
            },
        }
    }

    fn reset(&mut self) {
        self.used.clear();
        self.served.clear();
    }

    fn last_selected_index(&self) -> Option<usize> {
        self.served.len().checked_sub(1)
    }
}
