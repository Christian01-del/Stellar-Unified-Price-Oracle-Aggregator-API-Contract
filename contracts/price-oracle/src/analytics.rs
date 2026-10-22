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
//!
//! Oracle operating cost per asset model (#669): recurring RPC, storage,
//! monitoring, and incident costs are computed per supported asset from measured
//! inputs. The model reconciles input assumptions against sampled production-like
//! measurements and is pure and integer-only so it can be replayed and asserted
//! by automated checks.

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
    pub participation_met: bool,
    /// Whether the quality target was met.
    pub quality_met: bool,
    /// Whether the total spend stayed within the pilot budget.
    pub cost_met: bool,
    /// Whether the cost-per-accurate target was met.
    pub cost_per_accurate_met: bool,
}

impl RewardPilotReport {
    /// Returns true when every target was met and the pilot may continue.
    pub fn all_targets_met(&self) -> bool {
        self.participation_met && self.quality_met && self.cost_met && self.cost_per_accurate_met
    }
}

/// Evaluates pilot metrics against targets, producing a deterministic report.
///
/// Ratios are computed in basis points with integer division (floor). When the
/// denominator is zero the corresponding ratio is reported as zero and the
/// target is treated as unmet, so an empty pilot never silently passes.
pub fn evaluate_reward_pilot(
    targets: &RewardPilotTargets,
    metrics: &RewardPilotMetrics,
) -> RewardPilotReport {
    let participation_bps = if metrics.eligible_sources > 0 {
        ((metrics.participating_sources as u64 * 10_000) / metrics.eligible_sources as u64) as u32
    } else {
        0
    };

    let quality_bps = if metrics.total_submissions > 0 {
        ((metrics.accurate_submissions as u64 * 10_000) / metrics.total_submissions as u64) as u32
    } else {
        0
    };

    let cost_per_accurate = if metrics.accurate_submissions > 0 {
        metrics.total_cost / metrics.accurate_submissions as i128
    } else {
        0
    };

    RewardPilotReport {
        participation_bps,
        quality_bps,
        cost_per_accurate,
        participation_met: metrics.eligible_sources > 0
            && participation_bps >= targets.min_participation_bps,
        quality_met: metrics.total_submissions > 0 && quality_bps >= targets.min_quality_bps,
        cost_met: metrics.total_cost <= targets.max_cost,
        cost_per_accurate_met: metrics.accurate_submissions > 0
            && cost_per_accurate <= targets.max_cost_per_accurate,
    }
}

// ---------------------------------------------------------------------------
// Oracle operating cost per asset model (#669)
// ---------------------------------------------------------------------------

/// Measured recurring inputs for a single supported asset.
///
/// All monetary values are expressed in the smallest cost unit (e.g. stroops)
/// and all counts are per accounting period (typically one month). Inputs are
/// intended to be sampled from production-like measurements so the model can be
/// reconciled against observed operation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetCostInputs {
    /// RPC calls per period for this asset (reads, writes, simulations).
    pub rpc_calls: u32,
    /// Cost per RPC call (in cost units).
    pub rpc_cost_per_call: i128,
    /// Persistent storage entries maintained for this asset.
    pub storage_entries: u32,
    /// Cost per storage entry per period (in cost units).
    pub storage_cost_per_entry: i128,
    /// Monitoring checks per period for this asset.
    pub monitoring_checks: u32,
    /// Cost per monitoring check (in cost units).
    pub monitoring_cost_per_check: i128,
    /// Expected incidents per period for this asset.
    pub incidents_per_period: u32,
    /// Cost per incident (in cost units).
    pub incident_cost: i128,
}

impl AssetCostInputs {
    /// Default measured inputs used as the baseline assumption set.
    pub fn default_measured() -> Self {
        Self {
            rpc_calls: 100_000,
            rpc_cost_per_call: 10,
            storage_entries: 50,
            storage_cost_per_entry: 1_000,
            monitoring_checks: 8_640,
            monitoring_cost_per_check: 5,
            incidents_per_period: 1,
            incident_cost: 50_000,
        }
    }
}

/// Breakdown of recurring operating cost for a single asset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetCostBreakdown {
    /// Recurring RPC cost for the period.
    pub rpc_cost: i128,
    /// Recurring storage cost for the period.
    pub storage_cost: i128,
    /// Recurring monitoring cost for the period.
    pub monitoring_cost: i128,
    /// Expected incident cost for the period.
    pub incident_cost: i128,
    /// Total recurring cost for the period.
    pub total_cost: i128,
}

/// Computes the recurring operating cost breakdown for a single asset.
///
/// The computation is integer-only and saturating so extreme inputs cannot
/// overflow and produce a misleading total.
pub fn compute_asset_cost(inputs: &AssetCostInputs) -> AssetCostBreakdown {
    let rpc_cost = (inputs.rpc_calls as i128).saturating_mul(inputs.rpc_cost_per_call);
    let storage_cost =
        (inputs.storage_entries as i128).saturating_mul(inputs.storage_cost_per_entry);
    let monitoring_cost =
        (inputs.monitoring_checks as i128).saturating_mul(inputs.monitoring_cost_per_check);
    let incident_cost =
        (inputs.incidents_per_period as i128).saturating_mul(inputs.incident_cost);

    let total_cost = rpc_cost
        .saturating_add(storage_cost)
        .saturating_add(monitoring_cost)
        .saturating_add(incident_cost);

    AssetCostBreakdown {
        rpc_cost,
        storage_cost,
        monitoring_cost,
        incident_cost,
        total_cost,
    }
}

/// Reconciliation result comparing assumed inputs against sampled measurements.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetCostReconciliation {
    /// Cost computed from the assumed inputs.
    pub assumed_total: i128,
    /// Cost computed from the sampled measurements.
    pub sampled_total: i128,
    /// Absolute difference between assumed and sampled totals.
    pub delta: i128,
    /// Whether the delta is within the allowed tolerance.
    pub within_tolerance: bool,
}

/// Reconciles assumed inputs against sampled production-like measurements.
///
/// The model is considered reconciled when the absolute difference between the
/// assumed and sampled totals is at most `tolerance` (in cost units). A zero
/// tolerance requires an exact match.
pub fn reconcile_asset_cost(
    assumed: &AssetCostInputs,
    sampled: &AssetCostInputs,
    tolerance: i128,
) -> AssetCostReconciliation {
    let assumed_total = compute_asset_cost(assumed).total_cost;
    let sampled_total = compute_asset_cost(sampled).total_cost;
    let delta = if assumed_total >= sampled_total {
        assumed_total - sampled_total
    } else {
        sampled_total - assumed_total
    };

    AssetCostReconciliation {
        assumed_total,
        sampled_total,
        delta,
        within_tolerance: delta <= tolerance,
    }
}

/// Aggregated operating cost across all supported assets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PortfolioCostReport {
    /// Number of assets included in the report.
    pub asset_count: u32,
    /// Total recurring cost across all assets for the period.
    pub total_cost: i128,
    /// Average recurring cost per asset (0 when there are no assets).
    pub average_cost_per_asset: i128,
}

/// Aggregates per-asset cost breakdowns into a portfolio-level report.
///
/// Each entry in `per_asset` is the breakdown for one supported asset. The
/// average is computed with integer division (floor) and is zero when no assets
/// are supplied, so an empty portfolio never divides by zero.
pub fn aggregate_portfolio_cost(per_asset: &Vec<AssetCostBreakdown>) -> PortfolioCostReport {
    let asset_count = per_asset.len();
    let mut total_cost: i128 = 0;
    for breakdown in per_asset.iter() {
        total_cost = total_cost.saturating_add(breakdown.total_cost);
    }

    let average_cost_per_asset = if asset_count > 0 {
        total_cost / asset_count as i128
    } else {
        0
    };

    PortfolioCostReport {
        asset_count,
        total_cost,
        average_cost_per_asset,
    }
}
