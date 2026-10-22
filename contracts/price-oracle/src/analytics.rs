//! # Source Performance Analytics (#204)
//!
//! Provides visibility into source performance metrics including accuracy, timeliness,
//! uptime, and reputation trajectory over configurable rolling windows.
//!
//! Also hosts the bounded source-reward pilot evaluation (#666): measurable
//! participation, quality, and cost targets with explicit, deterministic stop
//! criteria. The evaluation is pure and reproducible so it can be replayed in a
//! simulation report and asserted by automated checks.

use soroban_sdk::{Address, Env};

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
    let scaled = (numerator as u64).saturating_mul(10_000) / (denominator as u64);
    if scaled > u32::MAX as u64 {
        u32::MAX
    } else {
        scaled as u32
    }
}

/// Evaluates pilot metrics against targets and produces a deterministic report.
///
/// The pause decision is a pure function of the inputs: the pilot pauses if any
/// target is missed. This makes the outcome reproducible for simulation reports
/// and automated checks.
pub fn evaluate_reward_pilot(
    targets: &RewardPilotTargets,
    metrics: &RewardPilotMetrics,
) -> RewardPilotReport {
    let participation_bps = ratio_bps(metrics.participating_sources, metrics.eligible_sources);
    let quality_bps = ratio_bps(metrics.accurate_submissions, metrics.total_submissions);

    let cost_per_accurate = if metrics.accurate_submissions > 0 {
        metrics.total_cost / (metrics.accurate_submissions as i128)
    } else {
        0
    };

    let participation_ok = participation_bps >= targets.min_participation_bps;
    let quality_ok = quality_bps >= targets.min_quality_bps;
    let cost_ok = metrics.total_cost <= targets.max_cost;
    // No accurate submissions means no rewardable output: treat as a cost miss.
    let cost_per_accurate_ok = metrics.accurate_submissions > 0
        && cost_per_accurate <= targets.max_cost_per_accurate;

    let should_pause = !(participation_ok && quality_ok && cost_ok && cost_per_accurate_ok);

    RewardPilotReport {
        participation_bps,
        quality_bps,
        cost_per_accurate,
        participation_ok,
        quality_ok,
        cost_ok,
        cost_per_accurate_ok,
        should_pause,
    }
}
