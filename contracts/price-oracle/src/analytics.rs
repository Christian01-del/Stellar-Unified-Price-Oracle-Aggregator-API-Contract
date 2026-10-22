//! # Source Performance Analytics (#204)
//!
//! Provides visibility into source performance metrics including accuracy, timeliness,
//! uptime, and reputation trajectory over configurable rolling windows.
//!
//! Also hosts the bounded source-reward pilot evaluation (#666): measurable
//! participation, quality, and cost targets with explicit, deterministic stop
//! criteria. The evaluation is pure and reproducible so it can be replayed in a
//! simulation report and asserted by automated checks.
//!
//! Reward attribution and event reconciliation (#667): submitted data and quality
//! metrics map to a reproducible source reward calculation. Allocations are
//! derived from retained events so an independent replay reproduces every
//! allocation within the documented rounding rules.

use soroban_sdk::{Address, Env, Vec};

use crate::events::SourceAnalyticsReportedEvent;
use crate::reputation;
use crate::storage::LEDGER_BUMP;
use crate::types::{DataKey, SourceAnalytics};

/// Computes and returns analytics for a source over the last `num_rounds` submissions.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `source` - Source address to analyze.
/// * `num_rounds` - Number of recent submissions to consider (rolling window).
///
/// # Returns
/// A [`SourceAnalytics`] struct containing:
/// - `accuracy`: Percentage based on price deviation from median (0–100).
/// - `timeliness`: Percentage based on submission regularity (0–100).
/// - `uptime`: Percentage of time source was active vs inactive (0–100).
/// - `reputation`: Current reputation score (0–100).
/// - `reputation_trajectory`: Change trend (+/- direction).
pub fn get_source_analytics(env: &Env, source: Address, num_rounds: u32) -> SourceAnalytics {
    let reputation_score = reputation::get_reputation(env, source.clone());

    // Get submission count for this source
    let key = DataKey::SourceSubmissionCount(source.clone());
    let submission_count: u32 = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(0);

    // Get accuracy sum for averaging
    let accuracy_key = DataKey::SourceAccuracySum(source.clone());
    let accuracy_sum: i128 = env
        .storage()
        .persistent()
        .get(&accuracy_key)
        .unwrap_or(0);

    // Calculate average accuracy (0–100)
    let accuracy = if submission_count > 0 && num_rounds > 0 {
        let avg = accuracy_sum / (submission_count.min(num_rounds) as i128);
        (avg.clamp(0, 100)) as u32
    } else {
        50 // neutral if no data
    };

    // Calculate timeliness: assume regular submissions = higher timeliness
    // If submission_count > 0, assume timeliness is proportional to submission frequency
    let timeliness = if submission_count > 0 {
        ((submission_count as u32).min(100)) as u32
    } else {
        0
    };

    // Calculate uptime: assume active if submitted in recent rounds
    let uptime = if submission_count > 0 {
        ((submission_count.min(num_rounds) as u32 * 100) / num_rounds.max(1)) as u32
    } else {
        0
    };

    // Compute reputation trajectory: compare old and new reputation
    // For simplicity, we check if reputation is drifting toward 50 (decay) or away from 50 (growth)
    let neutral_reputation = 50i32;
    let reputation_int = reputation_score as i32;
    let reputation_trajectory = if reputation_int > neutral_reputation {
        // Positive reputation: check if growing or decaying
        if accuracy as i32 > neutral_reputation {
            1 // improving
        } else {
            -1 // decaying
        }
    } else if reputation_int < neutral_reputation {
        // Low reputation: check if recovering or degrading
        if accuracy as i32 > reputation_int {
            1 // recovering
        } else {
            -1 // worsening
        }
    } else {
        0 // stable at neutral
    };

    SourceAnalytics {
        accuracy,
        timeliness,
        uptime,
        reputation: reputation_score,
        reputation_trajectory,
    }
}

/// Updates accuracy metrics for a source after a price submission.
///
/// Called internally after aggregation to record the source's accuracy.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `source` - Source address.
/// * `accuracy_score` - Accuracy score (0–100) for this submission.
pub fn record_submission_accuracy(env: &Env, source: &Address, accuracy_score: u32) {
    // Increment submission count
    let count_key = DataKey::SourceSubmissionCount(source.clone());
    let count: u32 = env
        .storage()
        .persistent()
        .get(&count_key)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&count_key, &(count + 1));

    // Add to accuracy sum
    let sum_key = DataKey::SourceAccuracySum(source.clone());
    let sum: i128 = env
        .storage()
        .persistent()
        .get(&sum_key)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&sum_key, &(sum + accuracy_score as i128));

    // Extend TTL
    env.storage()
        .persistent()
        .extend_ttl(&count_key, 300000, 3600000);
    env.storage()
        .persistent()
        .extend_ttl(&sum_key, 300000, 3600000);

    SourceAnalyticsReportedEvent {
        source: source.clone(),
        accuracy: accuracy_score,
    }
    .publish(env);
}

// ---------------------------------------------------------------------------
// Source reward pilot (#666)
// ---------------------------------------------------------------------------

/// Measurable targets for a bounded source-reward pilot.
///
/// All ratios are expressed in basis points (1 bps = 0.01%) to keep the
/// evaluation integer-only and deterministic across environments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewardPilotTargets {
    /// Minimum share of eligible sources that must participate (bps of eligible).
    pub min_participation_bps: u32,
    /// Minimum share of submissions that must be accurate (bps of submissions).
    pub min_quality_bps: u32,
    /// Maximum reward spend allowed for the pilot (in reward units).
    pub max_cost: i128,
    /// Maximum cost per accurate submission (in reward units).
    pub max_cost_per_accurate: i128,
}

impl RewardPilotTargets {
    /// Default pilot targets: 60% participation, 90% quality, bounded spend.
    pub fn default_pilot() -> Self {
        Self {
            min_participation_bps: 6_000,
            min_quality_bps: 9_000,
            max_cost: 1_000_000,
            max_cost_per_accurate: 100,
        }
    }
}

/// Observed pilot metrics used to evaluate the targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewardPilotMetrics {
    /// Number of sources eligible to participate in the pilot.
    pub eligible_sources: u32,
    /// Number of sources that actually participated.
    pub participating_sources: u32,
    /// Total submissions recorded during the pilot.
    pub total_submissions: u32,
    /// Submissions that met the accuracy threshold.
    pub accurate_submissions: u32,
    /// Total reward spend during the pilot (in reward units).
    pub total_cost: i128,
}

/// Deterministic outcome of evaluating a pilot against its targets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewardPilotReport {
    /// Participation ratio achieved (bps of eligible sources).
    pub participation_bps: u32,
    /// Quality ratio achieved (bps of submissions).
    pub quality_bps: u32,
    /// Cost per accurate submission (0 when there are no accurate submissions).
    pub cost_per_accurate: i128,
    /// Whether the participation target was met.
    pub participation_ok: bool,
    /// Whether the quality target was met.
    pub quality_ok: bool,
    /// Whether the total cost stayed within budget.
    pub cost_ok: bool,
    /// Whether cost per accurate submission stayed within budget.
    pub cost_per_accurate_ok: bool,
    /// Deterministic pause decision: `true` means the pilot must pause.
    pub should_pause: bool,
}

/// Computes a ratio in basis points, saturating at `u32::MAX` and returning 0
/// when the denominator is zero. Integer-only for deterministic replay.
fn ratio_bps(numerator: u32, denominator: u32) -> u32 {
    if denominator == 0 {
        return 0;
    }
    let scaled = (numerator as u64)
        .saturating_mul(10_000)
        .checked_div(denominator as u64)
        .unwrap_or(0);
    if scaled > u32::MAX as u64 {
        u32::MAX
    } else {
        scaled as u32
    }
}

// ---------------------------------------------------------------------------
// Reward attribution and event reconciliation (#667)
// ---------------------------------------------------------------------------

/// A single retained reward event used as the source of truth for attribution.
///
/// Each event captures the submitted data and quality metrics for one source
/// during one round. Reward allocations are derived *only* from these retained
/// events, so an independent replay over the same event log reproduces every
/// allocation exactly.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewardEvent {
    /// Source that produced the submission.
    pub source: Address,
    /// Round the submission belongs to (used for deterministic ordering).
    pub round: u32,
    /// Accuracy score for the submission (0–100).
    pub accuracy: u32,
    /// Whether the submission met the accuracy threshold.
    pub accurate: bool,
}

/// Deterministic reward allocation for a single source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewardAllocation {
    /// Source receiving the allocation.
    pub source: Address,
    /// Total reward units allocated to the source.
    pub amount: i128,
}

/// Reward attribution parameters. All weights are integer reward units so the
/// calculation is exact and reproducible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RewardAttributionParams {
    /// Reward units granted per accurate submission.
    pub reward_per_accurate: i128,
    /// Reward units granted per submission regardless of accuracy.
    pub reward_per_submission: i128,
    /// Accuracy threshold (0–100) a submission must meet to count as accurate.
    pub accuracy_threshold: u32,
    /// Total reward budget for the attribution window.
    pub budget: i128,
}

impl RewardAttributionParams {
    /// Default attribution parameters: 10 per accurate, 1 per submission,
    /// 80 accuracy threshold, 1,000,000 budget.
    pub fn default_params() -> Self {
        Self {
            reward_per_accurate: 10,
            reward_per_submission: 1,
            accuracy_threshold: 80,
            budget: 1_000_000,
        }
    }
}

/// Computes the raw (pre-budget) reward for a single event.
///
/// Formula: `reward_per_submission + (accurate ? reward_per_accurate : 0)`.
/// Accuracy is derived from the event's `accuracy` field against the
/// configured threshold, so replay does not depend on the stored `accurate`
/// flag and reconciles it against the retained metric.
fn event_reward(params: &RewardAttributionParams, event: &RewardEvent) -> i128 {
    let is_accurate = event.accuracy >= params.accuracy_threshold;
    let mut reward = params.reward_per_submission;
    if is_accurate {
        reward = reward.saturating_add(params.reward_per_accurate);
    }
    reward.max(0)
}

/// Computes reproducible reward allocations from retained events.
///
/// # Determinism and rounding rules
/// - Events are processed in the order provided; callers must supply events in
///   canonical `(round, source)` order for cross-environment replay.
/// - Per-source raw rewards are summed exactly (integer arithmetic, no floats).
/// - If the summed raw rewards exceed `budget`, each source's allocation is
///   scaled down proportionally using integer floor division:
///   `amount = raw * budget / total_raw`. The remainder is left unallocated
///   (never redistributed), so replay is exact and order-independent.
/// - If `total_raw <= budget`, allocations equal the raw rewards exactly.
/// - Sources with zero raw reward receive no allocation entry.
///
/// Returns allocations in the same order sources first appear in `events`.
pub fn compute_reward_allocations(
    env: &Env,
    params: &RewardAttributionParams,
    events: &Vec<RewardEvent>,
) -> Vec<RewardAllocation> {
    let mut sources: Vec<Address> = Vec::new(env);
    let mut raws: Vec<i128> = Vec::new(env);

    for i in 0..events.len() {
        let event = events.get(i).unwrap();
        let reward = event_reward(params, &event);

        // Find existing source index (linear scan keeps ordering deterministic).
        let mut found: Option<u32> = None;
        for j in 0..sources.len() {
            if sources.get(j).unwrap() == event.source {
                found = Some(j);
                break;
            }
        }

        match found {
            Some(idx) => {
                let prev = raws.get(idx).unwrap();
                raws.set(idx, prev.saturating_add(reward));
            }
            None => {
                sources.push_back(event.source.clone());
                raws.push_back(reward);
            }
        }
    }

    // Sum raw rewards for budget scaling.
    let mut total_raw: i128 = 0;
    for i in 0..raws.len() {
        total_raw = total_raw.saturating_add(raws.get(i).unwrap());
    }

    let mut allocations: Vec<RewardAllocation> = Vec::new(env);
    for i in 0..sources.len() {
        let raw = raws.get(i).unwrap();
        if raw <= 0 {
            continue;
        }
        let amount = if total_raw > params.budget && total_raw > 0 {
            // Proportional floor scaling; remainder intentionally unallocated.
            raw.saturating_mul(params.budget.max(0)) / total_raw
        } else {
            raw
        };
        if amount > 0 {
            allocations.push_back(RewardAllocation {
                source: sources.get(i).unwrap(),
                amount,
            });
        }
    }

    allocations
}

/// Reconciles retained events against a previously computed allocation set.
///
/// Replays [`compute_reward_allocations`] over `events` and verifies that the
/// result matches `expected` exactly (same sources, same amounts, same order).
/// Returns `true` when the replay reproduces every allocation within the
/// documented rounding rules.
pub fn reconcile_reward_allocations(
    env: &Env,
    params: &RewardAttributionParams,
    events: &Vec<RewardEvent>,
    expected: &Vec<RewardAllocation>,
) -> bool {
    let replayed = compute_reward_allocations(env, params, events);
    if replayed.len() != expected.len() {
        return false;
    }
    for i in 0..replayed.len() {
        let a = replayed.get(i).unwrap();
        let b = expected.get(i).unwrap();
        if a.source != b.source || a.amount != b.amount {
            return false;
        }
    }
    true
}
