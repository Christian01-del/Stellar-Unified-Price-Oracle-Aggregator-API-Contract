//! # Off-Chain Price Deviation Alerting System (#199)
//!
//! Monitors for price deviations vs external references and triggers alerts
//! when thresholds are exceeded. Supports multiple notification channels.
//!
//! Also hosts the per-asset operating cost model (#669): recurring RPC,
//! storage, monitoring, and incident costs are derived from measured inputs
//! and reconciled against sampled production-like measurements.

use soroban_sdk::{Address, Env, Vec};

use crate::events::PriceDeviationAlertEvent;
use crate::storage::LEDGER_BUMP;
use crate::types::DataKey;

/// Checks price deviation against a reference and triggers alert if threshold exceeded.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `asset` - Asset being monitored.
/// * `our_price` - Our aggregated price.
/// * `reference_price` - Price from external reference (Coinbase, Binance, etc).
/// * `deviation_threshold_bps` - Threshold in basis points (1% = 100 bps).
///
/// # Returns
/// `true` if deviation exceeded threshold and alert triggered, `false` otherwise.
pub fn check_and_alert_deviation(
    env: &Env,
    asset: Address,
    our_price: i128,
    reference_price: i128,
    deviation_threshold_bps: u32,
) -> bool {
    if our_price <= 0 || reference_price <= 0 {
        return false;
    }

    // Calculate deviation in basis points
    let abs_diff = (our_price - reference_price).abs();
    let deviation_bps = (abs_diff * 10_000) / reference_price;

    if deviation_bps as u32 > deviation_threshold_bps {
        // Trigger alert
        record_deviation_alert(
            env,
            &asset,
            our_price,
            reference_price,
            deviation_bps as u32,
        );
        // Classify the deviation by severity and route it to the appropriate channel.
        crate::alert_severity::evaluate_and_route(env, &asset, deviation_bps as u32);
        true
    } else {
        false
    }
}

/// Records a price deviation alert for monitoring/indexing.
///
/// Called internally when deviation exceeds threshold.
fn record_deviation_alert(
    env: &Env,
    asset: &Address,
    our_price: i128,
    reference_price: i128,
    deviation_bps: u32,
) {
    // Track last alert for this asset
    let key = DataKey::AlertLastPrice(asset.clone());
    env.storage().persistent().set(&key, &our_price);
    env.storage()
        .persistent()
        .extend_ttl(&key, 300000, LEDGER_BUMP);

    PriceDeviationAlertEvent {
        asset: asset.clone(),
        our_price,
        reference_price,
        deviation_bps,
        ledger: env.ledger().sequence(),
    }
    .publish(env);
}

/// Registers an external reference oracle for price comparison.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `asset` - Asset to monitor.
/// * `reference_oracle` - External oracle contract address.
pub fn register_reference_oracle(env: &Env, asset: Address, reference_oracle: Address) {
    let key = DataKey::ReferenceOracle(reference_oracle.clone());
    // Store the mapping from our asset to reference oracle
    let mut mapping = soroban_sdk::Map::new(env);
    mapping.set(asset.clone(), reference_oracle.clone());

    env.storage().persistent().set(&key, &mapping);
    env.storage()
        .persistent()
        .extend_ttl(&key, 300000, LEDGER_BUMP);
}

/// Returns the deviation check result for an asset.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `asset` - Asset to check.
///
/// # Returns
/// Last recorded price if deviation alert was triggered, 0 otherwise.
pub fn get_last_alert_price(env: &Env, asset: &Address) -> i128 {
    let key = DataKey::AlertLastPrice(asset.clone());
    env.storage().persistent().get(&key).unwrap_or(0i128)
}

// ---------------------------------------------------------------------------
// Per-asset operating cost model (#669)
// ---------------------------------------------------------------------------

/// Measured recurring inputs for a single supported asset, expressed in
/// stroops (1 XLM = 10_000_000 stroops) so the model stays integer-exact.
///
/// All fields are *measured* values sampled from production-like runs; the
/// model never invents them. `sample_count` records how many samples back the
/// inputs so reconciliation can reject under-sampled assumptions.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetCostInputs {
    /// RPC calls per month for this asset (price reads, submissions).
    pub rpc_calls_per_month: u32,
    /// Measured stroop cost per RPC call.
    pub rpc_cost_per_call: i128,
    /// Persistent storage entries kept live for this asset.
    pub storage_entries: u32,
    /// Measured stroop cost to (re)extend one storage entry per month.
    pub storage_cost_per_entry: i128,
    /// Monitoring checks per month (deviation checks, heartbeats).
    pub monitoring_checks_per_month: u32,
    /// Measured stroop cost per monitoring check.
    pub monitoring_cost_per_check: i128,
    /// Expected incidents per month for this asset.
    pub incidents_per_month: u32,
    /// Measured stroop cost to resolve one incident.
    pub incident_cost: i128,
    /// Number of production-like samples backing these inputs.
    pub sample_count: u32,
}

/// Breakdown of the recurring monthly cost for one asset, in stroops.
pub struct AssetCostBreakdown {
    pub rpc_cost: i128,
    pub storage_cost: i128,
    pub monitoring_cost: i128,
    pub incident_cost: i128,
    pub total_cost: i128,
}

/// Minimum number of production-like samples required before a cost model is
/// considered reconciled. Below this the model is treated as an assumption.
pub const MIN_COST_SAMPLES: u32 = 3;

/// Computes the recurring monthly operating cost for one asset from measured
/// inputs. Returns `None` when inputs are invalid (negative costs) so callers
/// can surface a failure path instead of silently under-reporting.
pub fn compute_asset_cost(inputs: &AssetCostInputs) -> Option<AssetCostBreakdown> {
    if inputs.rpc_cost_per_call < 0
        || inputs.storage_cost_per_entry < 0
        || inputs.monitoring_cost_per_check < 0
        || inputs.incident_cost < 0
    {
        return None;
    }

    let rpc_cost = (inputs.rpc_calls_per_month as i128) * inputs.rpc_cost_per_call;
    let storage_cost = (inputs.storage_entries as i128) * inputs.storage_cost_per_entry;
    let monitoring_cost =
        (inputs.monitoring_checks_per_month as i128) * inputs.monitoring_cost_per_check;
    let incident_cost = (inputs.incidents_per_month as i128) * inputs.incident_cost;
    let total_cost = rpc_cost + storage_cost + monitoring_cost + incident_cost;

    Some(AssetCostBreakdown {
        rpc_cost,
        storage_cost,
        monitoring_cost,
        incident_cost,
        total_cost,
    })
}

/// Reconciles the model's assumed inputs against sampled production-like
/// measurements. Returns `true` when the model is backed by at least
/// `MIN_COST_SAMPLES` samples and the sampled total is within
/// `tolerance_bps` of the modelled total.
///
/// This is the repeatable check that keeps assumptions honest: a model that
/// drifts from measurements fails reconciliation rather than being trusted.
pub fn reconcile_asset_cost(
    inputs: &AssetCostInputs,
    sampled_total_cost: i128,
    tolerance_bps: u32,
) -> bool {
    if inputs.sample_count < MIN_COST_SAMPLES || sampled_total_cost < 0 {
        return false;
    }

    let modelled = match compute_asset_cost(inputs) {
        Some(b) => b.total_cost,
        None => return false,
    };

    if modelled == 0 {
        return sampled_total_cost == 0;
    }

    let diff = (modelled - sampled_total_cost).abs();
    let drift_bps = (diff * 10_000) / modelled;
    drift_bps <= tolerance_bps as i128
}

/// Aggregates the recurring monthly cost across all supported assets.
///
/// Invalid inputs are skipped so a single bad asset cannot mask the fleet
/// total; callers should reconcile each asset individually first.
pub fn total_fleet_cost(assets: &Vec<AssetCostInputs>) -> i128 {
    let mut total = 0i128;
    for i in 0..assets.len() {
        if let Some(inputs) = assets.get(i) {
            if let Some(breakdown) = compute_asset_cost(&inputs) {
                total += breakdown.total_cost;
            }
        }
    }
    total
}
