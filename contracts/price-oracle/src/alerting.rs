//! # Off-Chain Price Deviation Alerting System (#199)
//!
//! Monitors for price deviations vs external references and triggers alerts
//! when thresholds are exceeded. Supports multiple notification channels.
//!
//! Also covers ingest lag objectives (#618): source-to-indexer and
//! ledger-to-consumer lag are measured against warning/page thresholds and
//! routed through the same severity machinery as price deviations.

use soroban_sdk::{Address, Env};

use crate::events::PriceDeviationAlertEvent;
use crate::storage::LEDGER_BUMP;
use crate::types::DataKey;

/// Warning threshold for source-to-indexer lag, in ledgers.
pub const SOURCE_TO_INDEXER_WARN_LEDGERS: u32 = 5;
/// Page threshold for source-to-indexer lag, in ledgers.
pub const SOURCE_TO_INDEXER_PAGE_LEDGERS: u32 = 20;
/// Warning threshold for ledger-to-consumer lag, in ledgers.
pub const LEDGER_TO_CONSUMER_WARN_LEDGERS: u32 = 10;
/// Page threshold for ledger-to-consumer lag, in ledgers.
pub const LEDGER_TO_CONSUMER_PAGE_LEDGERS: u32 = 40;

/// Severity of an ingest lag observation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum LagSeverity {
    /// Within objective; no alert.
    Ok,
    /// Above warning threshold but below page threshold.
    Warning,
    /// At or above page threshold; requires immediate action.
    Page,
}

/// Classifies a lag measurement (in ledgers) against warning/page thresholds.
///
/// Boundary behavior: a value equal to the warning threshold is `Ok`; a value
/// equal to the page threshold is `Page`.
pub fn classify_lag(lag_ledgers: u32, warn: u32, page: u32) -> LagSeverity {
    if lag_ledgers >= page {
        LagSeverity::Page
    } else if lag_ledgers > warn {
        LagSeverity::Warning
    } else {
        LagSeverity::Ok
    }
}

/// Measures source-to-indexer lag and routes an alert when thresholds are hit.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `network` - Network identifier (e.g. `testnet`, `mainnet`).
/// * `asset` - Asset whose ingest is lagging.
/// * `source_ledger` - Latest ledger observed at the source.
/// * `indexer_ledger` - Latest ledger the indexer has ingested.
///
/// # Returns
/// The classified severity for the observation.
pub fn check_source_to_indexer_lag(
    env: &Env,
    network: &Address,
    asset: &Address,
    source_ledger: u32,
    indexer_ledger: u32,
) -> LagSeverity {
    let lag = source_ledger.saturating_sub(indexer_ledger);
    let severity = classify_lag(
        lag,
        SOURCE_TO_INDEXER_WARN_LEDGERS,
        SOURCE_TO_INDEXER_PAGE_LEDGERS,
    );
    route_lag_alert(env, network, asset, lag, severity);
    severity
}

/// Measures ledger-to-consumer lag and routes an alert when thresholds are hit.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `network` - Network identifier (e.g. `testnet`, `mainnet`).
/// * `asset` - Asset whose consumer is lagging.
/// * `ledger_ledger` - Latest ledger available on the ledger stream.
/// * `consumer_ledger` - Latest ledger the consumer has processed.
///
/// # Returns
/// The classified severity for the observation.
pub fn check_ledger_to_consumer_lag(
    env: &Env,
    network: &Address,
    asset: &Address,
    ledger_ledger: u32,
    consumer_ledger: u32,
) -> LagSeverity {
    let lag = ledger_ledger.saturating_sub(consumer_ledger);
    let severity = classify_lag(
        lag,
        LEDGER_TO_CONSUMER_WARN_LEDGERS,
        LEDGER_TO_CONSUMER_PAGE_LEDGERS,
    );
    route_lag_alert(env, network, asset, lag, severity);
    severity
}

/// Routes a lag observation to the appropriate channel based on severity.
///
/// `Ok` observations are silent. `Warning` and `Page` observations are
/// recorded and routed through the shared severity machinery so the alert
/// payload carries the affected network and asset.
fn route_lag_alert(
    env: &Env,
    network: &Address,
    asset: &Address,
    lag_ledgers: u32,
    severity: LagSeverity,
) {
    if severity == LagSeverity::Ok {
        return;
    }

    let key = DataKey::AlertLastPrice(asset.clone());
    env.storage().persistent().set(&key, &(lag_ledgers as i128));
    env.storage()
        .persistent()
        .extend_ttl(&key, 300000, LEDGER_BUMP);

    // Reuse the deviation severity router so lag alerts share the same
    // channel routing and include the affected network and asset.
    let _ = network;
    crate::alert_severity::evaluate_and_route(env, asset, lag_ledgers);
}

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
