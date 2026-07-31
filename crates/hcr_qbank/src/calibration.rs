//! Item calibration: refitting difficulty from observed responses.
//!
//! arona has no calibration machinery at all, so this is entirely ours. The model
//! is 2PL with a mode term:
//!
//! ```text
//! P(correct | θ, item i, mode m) = σ( aᵢ · (θ − bᵢ − δₘ) )      δ_solo ≡ 0
//! ```
//!
//! # Why three stages rather than one joint fit
//!
//! For an item observed **only** under match conditions, `bᵢ` and `δ` are
//! perfectly confounded: raising `bᵢ` by `c` and lowering `δ` by `c` leaves the
//! likelihood unchanged. No quantity of data resolves that. Identification comes
//! from **linking items** deliberately served in both modes, whose difficulty is
//! pinned by solo responses — so any residual on their match responses is
//! attributable to the mode itself.
//!
//! | Stage | Data | Estimates | Held fixed |
//! | --- | --- | --- | --- |
//! | A | linking items, solo only | `bᵢ` | δ ≡ 0 by definition |
//! | B | linking items, match only | **δ** | `bᵢ` from stage A |
//! | C | every other item | `bᵢ` | δ from stage B |
//!
//! # The circularity that match data avoids
//!
//! Calibrating item `i` from a θ that was itself estimated using item `i` is
//! double-dipping. Match responses never update θ
//! (`docs/backend/07-CALIBRATION.md` §2), so for match-observed items the ability
//! estimate comes from independent solo sessions and the circularity is exactly
//! zero — a second reason the two modes complement each other.

use std::collections::BTreeSet;

use hcr_contract::{CalibrationState, ItemId, ItemParameters};

use crate::difficulty::{DIFFICULTY_MAX, DIFFICULTY_MIN};

/// Which condition a response was collected under.
///
/// Corresponds to the `source` tag on a stored response record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Untimed adaptive session. The reference condition, `δ = 0`.
    Solo,
    /// Competitive round under a wall-clock deadline.
    Match,
}

/// One scored response, reduced to what calibration needs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Observation {
    /// Ability of the respondent when they answered.
    pub theta: f64,
    /// Whether they mastered the item, after the threshold remap.
    pub correct: bool,
    /// Condition.
    pub mode: Mode,
}

impl Observation {
    /// Build an observation.
    pub fn new(theta: f64, correct: bool, mode: Mode) -> Self {
        Self {
            theta,
            correct,
            mode,
        }
    }

    fn delta(&self, mode_offset: f64) -> f64 {
        match self.mode {
            Mode::Solo => 0.0,
            Mode::Match => mode_offset,
        }
    }

    /// Model probability of a correct response.
    pub fn probability(&self, a: f64, b: f64, mode_offset: f64) -> f64 {
        logistic(a * (self.theta - b - self.delta(mode_offset)))
    }

    fn score(&self) -> f64 {
        if self.correct { 1.0 } else { 0.0 }
    }
}

fn logistic(z: f64) -> f64 {
    // Guard against overflow at the tails; exp(709) is near f64::MAX.
    if z >= 0.0 {
        1.0 / (1.0 + (-z).exp())
    } else {
        let e = z.exp();
        e / (1.0 + e)
    }
}

/// Numerical settings for the fitters.
#[derive(Debug, Clone, Copy)]
pub struct CalibrationSettings {
    /// Maximum Newton / Fisher-scoring iterations.
    pub max_iterations: u32,
    /// Convergence threshold on the parameter step.
    pub tolerance: f64,
    /// Largest single step, in logits.
    ///
    /// Without this, one unlucky batch can throw an item across the scale before
    /// the next iteration pulls it back.
    pub max_step: f64,
    /// Smallest usable Fisher information; below this the fit is unidentified.
    pub min_information: f64,
    /// `|Δb|` beyond which the item's parameters count as materially changed and
    /// a new version must be minted.
    pub version_bump_threshold: f64,
}

impl Default for CalibrationSettings {
    fn default() -> Self {
        Self {
            max_iterations: 50,
            tolerance: 1e-6,
            max_step: 0.5,
            min_information: 1e-9,
            version_bump_threshold: 0.1,
        }
    }
}

/// Outcome of fitting one item.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FitResult {
    /// Refitted difficulty.
    pub difficulty: f64,
    /// Discrimination, refitted only by [`refit_item`].
    pub discrimination: f64,
    /// Standard error of the difficulty estimate.
    pub se_difficulty: f64,
    /// Responses used.
    pub observations: usize,
    /// Iterations run.
    pub iterations: u32,
    /// Whether the step size fell below tolerance.
    pub converged: bool,
    /// Every response was identical, so the likelihood is monotonic and the
    /// maximum lies at ±∞.
    ///
    /// The estimate is pinned to the scale boundary and its standard error blows
    /// up; the item needs responses on both sides before it means anything.
    pub separated: bool,
}

impl FitResult {
    /// Whether this fit is precise enough to act on.
    pub fn is_usable(&self) -> bool {
        self.converged && !self.separated && self.se_difficulty.is_finite()
    }
}

/// Refit difficulty with discrimination held fixed.
///
/// The stationary condition is `Σ P = Σ x` — pick the difficulty at which the
/// model predicts exactly as many successes as were observed. The log-likelihood
/// is concave in `b`, so the Newton step
///
/// ```text
/// b ← b + Σ(Pᵣ − xᵣ) / ( a · Σ Pᵣ(1−Pᵣ) )
/// ```
///
/// converges from any sensible start.
pub fn refit_difficulty(
    observations: &[Observation],
    discrimination: f64,
    initial_difficulty: f64,
    mode_offset: f64,
    settings: &CalibrationSettings,
) -> FitResult {
    let mut result = FitResult {
        difficulty: initial_difficulty,
        discrimination,
        se_difficulty: f64::INFINITY,
        observations: observations.len(),
        iterations: 0,
        converged: false,
        separated: false,
    };

    if observations.is_empty() || discrimination <= 0.0 {
        return result;
    }

    let corrects = observations.iter().filter(|o| o.correct).count();
    result.separated = corrects == 0 || corrects == observations.len();

    let mut b = initial_difficulty;

    for iteration in 1..=settings.max_iterations {
        result.iterations = iteration;

        let mut residual = 0.0; // Σ(P − x)
        let mut weight = 0.0; // Σ P(1−P)
        for observation in observations {
            let p = observation.probability(discrimination, b, mode_offset);
            residual += p - observation.score();
            weight += p * (1.0 - p);
        }

        if weight < settings.min_information {
            break;
        }

        let step = (residual / (discrimination * weight))
            .clamp(-settings.max_step, settings.max_step);
        b = (b + step).clamp(DIFFICULTY_MIN, DIFFICULTY_MAX);

        if step.abs() < settings.tolerance {
            result.converged = true;
            break;
        }
    }

    // Recompute information at the final estimate for the standard error.
    let weight: f64 = observations
        .iter()
        .map(|o| {
            let p = o.probability(discrimination, b, mode_offset);
            p * (1.0 - p)
        })
        .sum();

    result.difficulty = b;
    result.se_difficulty = if weight > settings.min_information {
        1.0 / (discrimination * discrimination * weight).sqrt()
    } else {
        f64::INFINITY
    };

    result
}

/// Refit discrimination and difficulty jointly, by Fisher scoring.
///
/// Uses the **expected** information rather than the observed Hessian: the
/// `(x − P)` terms vanish in expectation, which makes the update matrix positive
/// definite and the iteration far better behaved on small samples.
///
/// Discrimination is estimated on `ln a` so it cannot go negative.
///
/// The information matrix is singular exactly when the respondents have no
/// spread in θ — which is precisely the case where discrimination is
/// unidentifiable — so that is detected and reported rather than papered over.
pub fn refit_item(
    observations: &[Observation],
    initial_discrimination: f64,
    initial_difficulty: f64,
    mode_offset: f64,
    settings: &CalibrationSettings,
) -> FitResult {
    let mut result = FitResult {
        difficulty: initial_difficulty,
        discrimination: initial_discrimination,
        se_difficulty: f64::INFINITY,
        observations: observations.len(),
        iterations: 0,
        converged: false,
        separated: false,
    };

    if observations.is_empty() || initial_discrimination <= 0.0 {
        return result;
    }

    let corrects = observations.iter().filter(|o| o.correct).count();
    result.separated = corrects == 0 || corrects == observations.len();

    let mut alpha = initial_discrimination.ln();
    let mut b = initial_difficulty;

    for iteration in 1..=settings.max_iterations {
        result.iterations = iteration;
        let a = alpha.exp();

        // A = Σ w u², B = Σ w u, C = Σ w, with w = P(1−P) and u = θ − b − δ.
        let (mut sum_a, mut sum_b, mut sum_c) = (0.0, 0.0, 0.0);
        // S = Σ(x − P), T = Σ(x − P)u
        let (mut s, mut t) = (0.0, 0.0);

        for observation in observations {
            let u = observation.theta - b - observation.delta(mode_offset);
            let p = observation.probability(a, b, mode_offset);
            let w = p * (1.0 - p);
            let residual = observation.score() - p;

            sum_a += w * u * u;
            sum_b += w * u;
            sum_c += w;
            s += residual;
            t += residual * u;
        }

        let determinant = sum_a * sum_c - sum_b * sum_b;
        if determinant < settings.min_information || sum_c < settings.min_information {
            // No θ spread: discrimination cannot be identified from this sample.
            break;
        }

        let step_alpha = ((sum_c * t - sum_b * s) / (a * determinant))
            .clamp(-settings.max_step, settings.max_step);
        let step_b = ((sum_b * t - sum_a * s) / (a * determinant))
            .clamp(-settings.max_step, settings.max_step);

        alpha += step_alpha;
        b = (b + step_b).clamp(DIFFICULTY_MIN, DIFFICULTY_MAX);
        // Keep discrimination in a sane band; runaway values are a symptom of a
        // bad sample, not a genuinely hyper-discriminating item.
        alpha = alpha.clamp(0.05f64.ln(), 3.0f64.ln());

        if step_alpha.abs() < settings.tolerance && step_b.abs() < settings.tolerance {
            result.converged = true;
            break;
        }
    }

    let a = alpha.exp();
    let (mut sum_a, mut sum_b, mut sum_c) = (0.0, 0.0, 0.0);
    for observation in observations {
        let u = observation.theta - b - observation.delta(mode_offset);
        let p = observation.probability(a, b, mode_offset);
        let w = p * (1.0 - p);
        sum_a += w * u * u;
        sum_b += w * u;
        sum_c += w;
    }
    let determinant = sum_a * sum_c - sum_b * sum_b;

    result.discrimination = a;
    result.difficulty = b;
    result.se_difficulty = if determinant > settings.min_information {
        (sum_a / (a * a * determinant)).sqrt()
    } else {
        f64::INFINITY
    };

    result
}

/// An item whose difficulty is already pinned, contributing to the δ estimate.
#[derive(Debug, Clone)]
pub struct LinkingItem<'a> {
    /// Discrimination, held fixed.
    pub discrimination: f64,
    /// Difficulty from stage A (solo responses only).
    pub difficulty: f64,
    /// All responses; only the match ones are used.
    pub observations: &'a [Observation],
}

/// Outcome of estimating the mode offset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModeOffsetFit {
    /// Estimated δ for match conditions.
    pub offset: f64,
    /// Standard error.
    pub se: f64,
    /// Match responses used.
    pub observations: usize,
    /// Linking items that contributed at least one match response.
    pub linking_items: usize,
    /// Whether the iteration converged.
    pub converged: bool,
}

impl ModeOffsetFit {
    /// A δ of exactly zero, for when there is nothing to estimate from.
    pub fn none() -> Self {
        Self {
            offset: 0.0,
            se: f64::INFINITY,
            observations: 0,
            linking_items: 0,
            converged: false,
        }
    }
}

/// Estimate δ from linking items' match responses (stage B).
///
/// Same shape as the difficulty fit with the roles swapped — δ is shared across
/// items rather than shared across responses to one item:
///
/// ```text
/// δ ← δ + Σ aᵢ(Pᵣ − xᵣ) / Σ aᵢ² Pᵣ(1−Pᵣ)
/// ```
///
/// **This is only meaningful for items whose difficulty was fixed by solo data.**
/// Passing items observed only in match mode would produce a number, and that
/// number would be arbitrary: δ and their `b` are not separately identified.
pub fn estimate_mode_offset(
    linking: &[LinkingItem<'_>],
    initial_offset: f64,
    settings: &CalibrationSettings,
) -> ModeOffsetFit {
    let mut used = 0usize;
    let mut contributing = 0usize;
    for item in linking {
        let count = item
            .observations
            .iter()
            .filter(|o| o.mode == Mode::Match)
            .count();
        if count > 0 {
            contributing += 1;
            used += count;
        }
    }

    let mut fit = ModeOffsetFit {
        offset: initial_offset,
        se: f64::INFINITY,
        observations: used,
        linking_items: contributing,
        converged: false,
    };

    if used == 0 {
        return fit;
    }

    let mut delta = initial_offset;

    for _ in 0..settings.max_iterations {
        let mut numerator = 0.0; // Σ a(P − x)
        let mut denominator = 0.0; // Σ a² P(1−P)

        for item in linking {
            for observation in item
                .observations
                .iter()
                .filter(|o| o.mode == Mode::Match)
            {
                let p = observation.probability(item.discrimination, item.difficulty, delta);
                numerator += item.discrimination * (p - observation.score());
                denominator += item.discrimination * item.discrimination * p * (1.0 - p);
            }
        }

        if denominator < settings.min_information {
            break;
        }

        let step = (numerator / denominator).clamp(-settings.max_step, settings.max_step);
        delta += step;

        if step.abs() < settings.tolerance {
            fit.converged = true;
            break;
        }
    }

    let mut information = 0.0;
    for item in linking {
        for observation in item
            .observations
            .iter()
            .filter(|o| o.mode == Mode::Match)
        {
            let p = observation.probability(item.discrimination, item.difficulty, delta);
            information += item.discrimination * item.discrimination * p * (1.0 - p);
        }
    }

    fit.offset = delta;
    fit.se = if information > settings.min_information {
        1.0 / information.sqrt()
    } else {
        f64::INFINITY
    };
    fit
}

/// Outfit-style mean squared standardized residual.
///
/// Expected value is 1 for data that fits the model. Values well above 1 mean the
/// item is behaving erratically — often a sign the content changed underneath it,
/// or that it is measuring something other than the intended trait.
pub fn outfit(
    observations: &[Observation],
    discrimination: f64,
    difficulty: f64,
    mode_offset: f64,
) -> f64 {
    let mut total = 0.0;
    let mut counted = 0usize;

    for observation in observations {
        let p = observation.probability(discrimination, difficulty, mode_offset);
        let variance = p * (1.0 - p);
        // Near-certain responses carry almost no information and would otherwise
        // dominate the statistic through division by a vanishing variance.
        if variance < 1e-6 {
            continue;
        }
        let z = (observation.score() - p) / variance.sqrt();
        total += z * z;
        counted += 1;
    }

    if counted == 0 { 1.0 } else { total / counted as f64 }
}

/// Split-half drift check.
///
/// Refits the first and second half of a chronologically ordered response stream
/// and reports how far apart the two estimates are, in pooled standard errors.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DriftReport {
    /// Difficulty fitted from the earlier half.
    pub early_difficulty: f64,
    /// Difficulty fitted from the later half.
    pub late_difficulty: f64,
    /// `|b_late − b_early|` in pooled standard errors.
    pub z: f64,
}

impl DriftReport {
    /// Whether the two halves disagree beyond the conventional two-sigma bar.
    pub fn is_drifting(&self) -> bool {
        self.z > 2.0
    }
}

/// Detect parameter drift over time. Returns `None` when either half is too thin
/// or too degenerate to fit.
pub fn detect_drift(
    observations: &[Observation],
    discrimination: f64,
    difficulty: f64,
    mode_offset: f64,
    settings: &CalibrationSettings,
) -> Option<DriftReport> {
    if observations.len() < 4 {
        return None;
    }
    let midpoint = observations.len() / 2;
    let (early, late) = observations.split_at(midpoint);

    let early_fit = refit_difficulty(early, discrimination, difficulty, mode_offset, settings);
    let late_fit = refit_difficulty(late, discrimination, difficulty, mode_offset, settings);

    if !early_fit.is_usable() || !late_fit.is_usable() {
        return None;
    }

    let pooled = (early_fit.se_difficulty.powi(2) + late_fit.se_difficulty.powi(2)).sqrt();
    if pooled <= 0.0 || !pooled.is_finite() {
        return None;
    }

    Some(DriftReport {
        early_difficulty: early_fit.difficulty,
        late_difficulty: late_fit.difficulty,
        z: (late_fit.difficulty - early_fit.difficulty).abs() / pooled,
    })
}

/// Thresholds governing an item's progression through the calibration lifecycle.
#[derive(Debug, Clone, Copy)]
pub struct PromotionPolicy {
    /// Responses needed to leave `Provisional`.
    pub online_min_observations: usize,
    /// Standard error needed to leave `Provisional`.
    pub online_max_se: f64,
    /// Responses needed to reach `Calibrated`.
    pub calibrated_min_observations: usize,
    /// Standard error needed to reach `Calibrated`.
    pub calibrated_max_se: f64,
    /// Discrimination at or below which an item is withdrawn.
    pub retire_below_discrimination: f64,
    /// Outfit above which an item is withdrawn.
    pub retire_above_outfit: f64,
}

impl Default for PromotionPolicy {
    fn default() -> Self {
        Self {
            online_min_observations: 30,
            online_max_se: 0.50,
            calibrated_min_observations: 200,
            calibrated_max_se: 0.25,
            retire_below_discrimination: 0.2,
            retire_above_outfit: 2.0,
        }
    }
}

impl PromotionPolicy {
    /// Decide an item's next lifecycle state.
    ///
    /// Retirement is checked first and is one-way: an item that has demonstrably
    /// stopped measuring should not be rescued by a favourable sample size.
    pub fn next_state(
        &self,
        current: CalibrationState,
        fit: &FitResult,
        outfit: f64,
    ) -> CalibrationState {
        if current == CalibrationState::Retired {
            return CalibrationState::Retired;
        }

        // A separated or unconverged fit tells us nothing; hold position.
        if !fit.is_usable() {
            return current;
        }

        if fit.discrimination <= self.retire_below_discrimination || outfit > self.retire_above_outfit
        {
            return CalibrationState::Retired;
        }

        if fit.observations >= self.calibrated_min_observations
            && fit.se_difficulty <= self.calibrated_max_se
        {
            CalibrationState::Calibrated
        } else if fit.observations >= self.online_min_observations
            && fit.se_difficulty <= self.online_max_se
        {
            CalibrationState::Online
        } else {
            current
        }
    }
}

/// Responses gathered for one item.
#[derive(Debug, Clone)]
pub struct ItemObservations {
    /// Which item.
    pub item_id: ItemId,
    /// Current parameters.
    pub params: ItemParameters,
    /// Current lifecycle state.
    pub state: CalibrationState,
    /// Responses, chronologically ordered.
    pub observations: Vec<Observation>,
}

impl ItemObservations {
    /// Responses collected under one condition.
    pub fn by_mode(&self, mode: Mode) -> Vec<Observation> {
        self.observations
            .iter()
            .copied()
            .filter(|o| o.mode == mode)
            .collect()
    }
}

/// What the pipeline concluded about one item.
#[derive(Debug, Clone)]
pub struct ItemFit {
    /// Which item.
    pub item_id: ItemId,
    /// Parameters before the refit.
    pub before: ItemParameters,
    /// Parameters after.
    pub after: ItemParameters,
    /// Fit diagnostics.
    pub fit: FitResult,
    /// Mean squared standardized residual.
    pub outfit: f64,
    /// Lifecycle state before.
    pub previous_state: CalibrationState,
    /// Lifecycle state after.
    pub next_state: CalibrationState,
    /// Whether `|Δb|` crossed the materiality threshold, requiring a new
    /// `challengeVersion` so historical scores cannot move.
    pub needs_version_bump: bool,
    /// Drift check, when enough responses existed to run one.
    pub drift: Option<DriftReport>,
}

/// Everything one pipeline run produced.
#[derive(Debug, Clone)]
pub struct PipelineReport {
    /// Estimated mode offset.
    pub mode_offset: ModeOffsetFit,
    /// Per-item outcomes.
    pub items: Vec<ItemFit>,
}

impl PipelineReport {
    /// Items whose parameters moved enough to require a new version.
    pub fn version_bumps(&self) -> impl Iterator<Item = &ItemFit> {
        self.items.iter().filter(|item| item.needs_version_bump)
    }

    /// Items the run retired.
    pub fn retired(&self) -> impl Iterator<Item = &ItemFit> {
        self.items
            .iter()
            .filter(|item| item.next_state == CalibrationState::Retired)
    }
}

/// The three-stage calibration run.
#[derive(Debug, Clone, Copy, Default)]
pub struct CalibrationPipeline {
    /// Numerical settings.
    pub settings: CalibrationSettings,
    /// Lifecycle thresholds.
    pub policy: PromotionPolicy,
    /// Refit discrimination as well as difficulty.
    ///
    /// Off by default: discrimination needs far more data than difficulty, and a
    /// badly estimated `a` corrupts every subsequent difficulty fit that uses it.
    pub fit_discrimination: bool,
}

impl CalibrationPipeline {
    /// Run stages A, B and C.
    ///
    /// `linking` are items deliberately served in **both** modes; they are the
    /// only source of identification for δ. `others` are everything else.
    ///
    /// Pure: nothing is mutated and nothing is persisted. The caller inspects the
    /// report and decides what to apply — which matters because applying a
    /// difficulty change may require minting a new item version, and that is a
    /// storage decision rather than a statistical one.
    pub fn run(
        &self,
        linking: &[ItemObservations],
        others: &[ItemObservations],
    ) -> PipelineReport {
        let mut items = Vec::with_capacity(linking.len() + others.len());

        // ---- Stage A: pin linking-item difficulty from solo responses only ----
        let mut solo_fits = Vec::with_capacity(linking.len());
        for item in linking.iter() {
            let solo = item.by_mode(Mode::Solo);
            let fit = self.fit_one(&solo, &item.params, 0.0);
            solo_fits.push(fit);
        }

        // ---- Stage B: estimate δ from those items' match responses ----
        let linking_views: Vec<LinkingItem<'_>> = linking
            .iter()
            .zip(&solo_fits)
            .map(|(item, fit)| LinkingItem {
                discrimination: fit.discrimination,
                difficulty: fit.difficulty,
                observations: &item.observations,
            })
            .collect();

        let mode_offset =
            estimate_mode_offset(&linking_views, 0.0, &self.settings);
        // An unidentified or non-converged δ must not be applied: it would shift
        // every match-observed item by an arbitrary amount.
        let delta = if mode_offset.converged {
            mode_offset.offset
        } else {
            0.0
        };

        // Record the linking items using their stage-A fits.
        for (item, fit) in linking.iter().zip(solo_fits) {
            items.push(self.finish(item, fit, delta));
        }

        // ---- Stage C: everything else, with δ now fixed ----
        for item in others.iter() {
            let fit = self.fit_one(&item.observations, &item.params, delta);
            items.push(self.finish(item, fit, delta));
        }

        PipelineReport { mode_offset, items }
    }

    fn fit_one(
        &self,
        observations: &[Observation],
        params: &ItemParameters,
        delta: f64,
    ) -> FitResult {
        if self.fit_discrimination {
            refit_item(
                observations,
                params.discrimination,
                params.difficulty,
                delta,
                &self.settings,
            )
        } else {
            refit_difficulty(
                observations,
                params.discrimination,
                params.difficulty,
                delta,
                &self.settings,
            )
        }
    }

    fn finish(&self, item: &ItemObservations, fit: FitResult, delta: f64) -> ItemFit {
        let before = item.params;
        let misfit = outfit(&item.observations, fit.discrimination, fit.difficulty, delta);
        let next_state = self.policy.next_state(item.state, &fit, misfit);
        let drift = detect_drift(
            &item.observations,
            fit.discrimination,
            fit.difficulty,
            delta,
            &self.settings,
        );

        let after = if fit.is_usable() {
            ItemParameters {
                discrimination: fit.discrimination,
                difficulty: fit.difficulty,
                guessing: before.guessing,
            }
        } else {
            // Nothing learned; leave the item exactly as it was.
            before
        };

        let needs_version_bump =
            (after.difficulty - before.difficulty).abs() > self.settings.version_bump_threshold;

        ItemFit {
            item_id: item.item_id.clone(),
            before,
            after,
            fit,
            outfit: misfit,
            previous_state: item.state,
            next_state,
            needs_version_bump,
            drift,
        }
    }
}

/// Items appearing in both slices, which would be fitted twice.
///
/// A linking item must not also be passed as an ordinary item: stage A fits it
/// from solo data alone, and a second stage-C fit would overwrite that with a
/// δ-contaminated estimate, quietly destroying the identification the linking set
/// exists to provide.
pub fn overlapping_ids(linking: &[ItemObservations], others: &[ItemObservations]) -> Vec<ItemId> {
    let linking_ids: BTreeSet<&str> = linking.iter().map(|i| i.item_id.as_str()).collect();
    others
        .iter()
        .filter(|item| linking_ids.contains(item.item_id.as_str()))
        .map(|item| item.item_id.clone())
        .collect()
}
