//! # Off-Chain Price Deviation Alerting System (#199)
//!
//! Monitors for price deviations vs external references and triggers alerts
//! when thresholds are exceeded. Supports multiple notification channels.
//!
//! Also provides source submission clock-skew monitoring (#647): source
//! timestamps are compared against both the ledger timestamp and the observed
//! submission time, with per-source configurable thresholds.

use soroban_sdk::{Address, Env};

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

/// Per-source clock-skew thresholds, in seconds.
///
/// `ledger_tolerance_secs` bounds the difference between the source
/// timestamp and the ledger timestamp. `submission_tolerance_secs` bounds the
/// difference between the source timestamp and the observed submission time.
#[derive(Clone, Copy)]
pub struct SkewThresholds {
    pub ledger_tolerance_secs: u64,
    pub submission_tolerance_secs: u64,
}

/// Result of a clock-skew evaluation for a single source.
#[derive(Clone, Copy)]
pub struct SkewReport {
    /// Absolute skew between source timestamp and ledger timestamp (secs).
    pub ledger_skew_secs: u64,
    /// Absolute skew between source timestamp and observed submission time (secs).
    pub submission_skew_secs: u64,
    /// Whether the ledger skew exceeded the configured threshold.
    pub ledger_exceeded: bool,
    /// Whether the submission skew exceeded the configured threshold.
    pub submission_exceeded: bool,
}

impl SkewReport {
    /// Returns `true` if any skew dimension exceeded its threshold.
    pub fn exceeded(&self) -> bool {
        self.ledger_exceeded || self.submission_exceeded
    }
}

/// Stores the per-source clock-skew thresholds.
pub fn set_skew_thresholds(env: &Env, source: Address, thresholds: SkewThresholds) {
    let key = DataKey::SourceSkewThresholds(source.clone());
    env.storage().persistent().set(&key, &thresholds);
    env.storage()
        .persistent()
        .extend_ttl(&key, 300000, LEDGER_BUMP);
}

/// Returns the configured per-source clock-skew thresholds, if any.
pub fn get_skew_thresholds(env: &Env, source: &Address) -> Option<SkewThresholds> {
    let key = DataKey::SourceSkewThresholds(source.clone());
    env.storage().persistent().get(&key)
}

/// Measures source timestamp skew against the ledger timestamp and the
/// observed submission time, and emits a source-scoped alert when either
/// exceeds the per-source threshold.
///
/// This is additive: it never suppresses or short-circuits other source
/// health signals. Callers should continue to run their other checks
/// regardless of the returned report.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `source` - Source whose submission is being evaluated.
/// * `source_timestamp` - Timestamp reported by the source (secs).
/// * `observed_submission_time` - Timestamp observed when the submission was received (secs).
///
/// # Returns
/// The computed [`SkewReport`]. When no thresholds are configured for the
/// source, no alert is emitted and both `*_exceeded` flags are `false`.
pub fn check_and_alert_clock_skew(
    env: &Env,
    source: Address,
    source_timestamp: u64,
    observed_submission_time: u64,
) -> SkewReport {
    let ledger_timestamp = env.ledger().timestamp();

    let ledger_skew_secs = abs_diff_u64(source_timestamp, ledger_timestamp);
    let submission_skew_secs = abs_diff_u64(source_timestamp, observed_submission_time);

    let mut report = SkewReport {
        ledger_skew_secs,
        submission_skew_secs,
        ledger_exceeded: false,
        submission_exceeded: false,
    };

    if let Some(thresholds) = get_skew_thresholds(env, &source) {
        report.ledger_exceeded = ledger_skew_secs > thresholds.ledger_tolerance_secs;
        report.submission_exceeded =
            submission_skew_secs > thresholds.submission_tolerance_secs;

        if report.exceeded() {
            record_clock_skew_alert(env, &source, &report, ledger_timestamp);
        }
    }

    report
}

/// Records a source-scoped clock-skew alert for monitoring/indexing.
fn record_clock_skew_alert(env: &Env, source: &Address, report: &SkewReport, ledger: u64) {
    let key = DataKey::SourceSkewAlertLast(source.clone());
    env.storage().persistent().set(&key, &ledger);
    env.storage()
        .persistent()
        .extend_ttl(&key, 300000, LEDGER_BUMP);

    crate::events::SourceClockSkewAlertEvent {
        source: source.clone(),
        ledger_skew_secs: report.ledger_skew_secs,
        submission_skew_secs: report.submission_skew_secs,
        ledger_exceeded: report.ledger_exceeded,
        submission_exceeded: report.submission_exceeded,
        ledger,
    }
    .publish(env);
}

/// Returns the ledger timestamp of the last clock-skew alert for a source, if any.
pub fn get_last_clock_skew_alert(env: &Env, source: &Address) -> Option<u64> {
    let key = DataKey::SourceSkewAlertLast(source.clone());
    env.storage().persistent().get(&key)
}

/// Absolute difference between two `u64` values without underflow.
fn abs_diff_u64(a: u64, b: u64) -> u64 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}
