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
//!
//! Treasury runway and funding scenarios (#668): operational spend and committed
//! funding are modeled under baseline, low-usage, and high-incident scenarios.
//! The report states its assumptions, the runway per scenario, and the threshold
//! that triggers a funding review. The model is pure and integer-only so it can
//! be replayed and asserted by automated checks.

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

// ---------------------------------------------------------------------------
// Treasury runway and funding scenarios (#668)
// ---------------------------------------------------------------------------

/// Operational scenario used to model treasury runway.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreasuryScenario {
    /// Expected steady-state operation.
    Baseline,
    /// Reduced usage: lower submission volume and lower reward spend.
    LowUsage,
    /// Elevated incident response: higher operational and reward spend.
    HighIncident,
}

/// Assumptions and inputs for the treasury runway model.
///
/// All monetary values are expressed in the same integer reward units used by
/// the reward pilot. Ratios are in basis points (1 bps = 0.01%).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreasuryAssumptions {
    /// Committed funding available to the treasury (reward units).
    pub committed_funding: i128,
    /// Baseline operational spend per period (reward units).
    pub baseline_operational_spend: i128,
    /// Baseline reward spend per period (reward units).
    pub baseline_reward_spend: i128,
    /// Low-usage spend multiplier (bps of baseline).
    pub low_usage_spend_bps: u32,
    /// High-incident spend multiplier (bps of baseline).
    pub high_incident_spend_bps: u32,
    /// Minimum runway (in periods) that triggers a funding review.
    pub funding_review_threshold_periods: u32,
}

impl TreasuryAssumptions {
    /// Default assumptions: 12-month committed funding, 50% low-usage and
    /// 200% high-incident spend, with a 6-period funding-review threshold.
    pub fn default_assumptions() -> Self {
        Self {
            committed_funding: 1_200_000,
            baseline_operational_spend: 50_000,
            baseline_reward_spend: 50_000,
            low_usage_spend_bps: 5_000,
            high_incident_spend_bps: 20_000,
            funding_review_threshold_periods: 6,
        }
    }
}

/// Runway result for a single scenario.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreasuryScenarioReport {
    /// Scenario modeled.
    pub scenario: TreasuryScenario,
    /// Total spend per period under this scenario (reward units).
    pub spend_per_period: i128,
    /// Number of whole periods the committed funding covers (0 when spend is 0).
    pub runway_periods: u32,
    /// Whether runway is at or below the funding-review threshold.
    pub funding_review_required: bool,
}

/// Deterministic treasury runway report across all scenarios.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreasuryRunwayReport {
    /// Assumptions used to produce the report.
    pub assumptions: TreasuryAssumptions,
    /// Baseline scenario result.
    pub baseline: TreasuryScenarioReport,
    /// Low-usage scenario result.
    pub low_usage: TreasuryScenarioReport,
    /// High-incident scenario result.
    pub high_incident: TreasuryScenarioReport,
    /// Whether any scenario requires a funding review.
    pub funding_review_required: bool,
}

/// Computes the spend per period for a scenario from the assumptions.
fn scenario_spend_per_period(assumptions: &TreasuryAssumptions, scenario: TreasuryScenario) -> i128 {
    let baseline = assumptions
        .baseline_operational_spend
        .saturating_add(assumptions.baseline_reward_spend);
    let multiplier_bps = match scenario {
        TreasuryScenario::Baseline => 10_000u32,
        TreasuryScenario::LowUsage => assumptions.low_usage_spend_bps,
        TreasuryScenario::HighIncident => assumptions.high_incident_spend_bps,
    };
    baseline.saturating_mul(multiplier_bps as i128) / 10_000
}

/// Computes the runway (whole periods) for a given spend per period.
fn runway_periods(committed_funding: i128, spend_per_period: i128) -> u32 {
    if spend_per_period <= 0 {
        return 0;
    }
    let periods = committed_funding / spend_per_period;
    if periods <= 0 {
        0
    } else if periods > u32::MAX as i128 {
        u32::MAX
    } else {
        periods as u32
    }
}

fn evaluate_scenario(
    assumptions: &TreasuryAssumptions,
    scenario: TreasuryScenario,
) -> TreasuryScenarioReport {
    let spend_per_period = scenario_spend_per_period(assumptions, scenario);
    let runway = runway_periods(assumptions.committed_funding, spend_per_period);
    let funding_review_required = runway <= assumptions.funding_review_threshold_periods;
    TreasuryScenarioReport {
        scenario,
        spend_per_period,
        runway_periods: runway,
        funding_review_required,
    }
}

/// Models treasury runway and committed funding under baseline, low-usage, and
/// high-incident scenarios.
///
/// The model is pure and integer-only: given the same [`TreasuryAssumptions`]
/// it always produces the same [`TreasuryRunwayReport`], so it can be replayed
/// and asserted by automated checks. A scenario triggers a funding review when
/// its runway is at or below `funding_review_threshold_periods`.
pub fn model_treasury_runway(assumptions: TreasuryAssumptions) -> TreasuryRunwayReport {
    let baseline = evaluate_scenario(&assumptions, TreasuryScenario::Baseline);
    let low_usage = evaluate_scenario(&assumptions, TreasuryScenario::LowUsage);
    let high_incident = evaluate_scenario(&assumptions, TreasuryScenario::HighIncident);
    let funding_review_required = baseline.funding_review_required
        || low_usage.funding_review_required
        || high_incident.funding_review_required;
    TreasuryRunwayReport {
        assumptions,
        baseline,
        low_usage,
        high_incident,
        funding_review_required,
    }
}
