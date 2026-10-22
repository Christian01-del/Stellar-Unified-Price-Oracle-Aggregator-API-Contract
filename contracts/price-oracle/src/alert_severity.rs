//! # Severity-Aware Alerting
//!
//! Extends the existing price-deviation alerting paths ([`crate::alerts`] on-chain
//! subscriptions and [`crate::alerting`] off-chain reference-price checks) with a
//! severity taxonomy so operators can triage anomalies instead of treating every
//! deviation identically.
//!
//! ## Severity taxonomy
//!
//! Movement is classified against configurable basis-point thresholds:
//!
//! | Severity    | Condition                              | Channel   |
//! |-------------|-----------------------------------------|-----------|
//! | `Info`      | `movement_bps < warning_bps`             | —         |
//! | `Warning`   | `warning_bps <= movement_bps < critical_bps`   | Dashboard |
//! | `Critical`  | `critical_bps <= movement_bps < emergency_bps` | Page      |
//! | `Emergency` | `movement_bps >= emergency_bps`          | Page      |
//!
//! Thresholds default globally and can be overridden per asset. Every classification
//! emits a [`SeverityAlertEvent`], which off-chain relayers subscribe to and forward
//! to the routed channel (a dashboard feed for `Warning`, a paging integration for
//! `Critical`/`Emergency`) — see `docs/monitoring/README.md`.
//!
//! `Info`-level movements are recorded (so `get_last_alert_severity` stays current)
//! but are not routed anywhere; they exist purely so operators can distinguish "no
//! alert has ever fired" from "the last alert was below the warning line".
//!
//! ## Ingest lag objective
//!
//! The same severity taxonomy is reused to calibrate ingest-lag alerting. Two lag
//! metrics are measured in ledgers (the natural clock of the indexer):
//!
//! * **source-to-indexer lag** — how far the indexer trails the source ledger
//!   stream (e.g. Horizon / RPC ingestion).
//! * **ledger-to-consumer lag** — how far downstream consumers trail the latest
//!   indexed ledger.
//!
//! Both are classified against [`LagThresholds`] with the same `Warning`/`Critical`
//! routing table, so a replayed lag incident produces the expected severity and the
//! emitted [`LagAlertEvent`] carries the affected network and asset.

use soroban_sdk::{panic_with_error, Address, Env, String};

use crate::events::{LagAlertEvent, SeverityAlertEvent, SeverityThresholdsSetEvent};
use crate::storage::{get_admin, LEDGER_BUMP, LEDGER_THRESHOLD};
use crate::types::{AlertChannel, AlertSeverity, DataKey, ErrorCode, SeverityThresholds};

/// Default global severity thresholds, in basis points of price movement.
pub const DEFAULT_WARNING_BPS: u32 = 300; // 3%
pub const DEFAULT_CRITICAL_BPS: u32 = 1_000; // 10%
pub const DEFAULT_EMERGENCY_BPS: u32 = 2_500; // 25%

/// Default ingest-lag thresholds, in ledgers.
///
/// Warning fires when lag reaches ~30 ledgers (~2.5 min at 5s/ledger); page
/// (Critical) fires at ~120 ledgers (~10 min). These are the actionable lines
/// operators agreed on for the source-to-indexer and ledger-to-consumer paths.
pub const DEFAULT_LAG_WARNING_LEDGERS: u32 = 30;
pub const DEFAULT_LAG_CRITICAL_LEDGERS: u32 = 120;

fn default_thresholds() -> SeverityThresholds {
    SeverityThresholds {
        warning_bps: DEFAULT_WARNING_BPS,
        critical_bps: DEFAULT_CRITICAL_BPS,
        emergency_bps: DEFAULT_EMERGENCY_BPS,
    }
}

fn validate_thresholds(env: &Env, thresholds: &SeverityThresholds) {
    if thresholds.warning_bps == 0
        || thresholds.warning_bps >= thresholds.critical_bps
        || thresholds.critical_bps >= thresholds.emergency_bps
    {
        panic_with_error!(env, ErrorCode::InvalidSeverityThresholds);
    }
}

// ─── Configuration ─────────────────────────────────────────────────────────────

/// Sets the global default severity thresholds (basis points). Admin-only.
///
/// # Panics
/// * [`ErrorCode::NotAuthorized`] — caller is not the admin.
/// * [`ErrorCode::InvalidSeverityThresholds`] — thresholds are not strictly
///   increasing, or `warning_bps == 0`.
pub fn set_severity_thresholds(env: &Env, warning_bps: u32, critical_bps: u32, emergency_bps: u32) {
    let admin = get_admin(env);
    admin.require_auth();

    let thresholds = SeverityThresholds {
        warning_bps,
        critical_bps,
        emergency_bps,
    };
    validate_thresholds(env, &thresholds);

    let key = DataKey::CfgSeverityThresholds;
    env.storage().persistent().set(&key, &thresholds);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);

    SeverityThresholdsSetEvent {
        is_asset_override: false,
        warning_bps,
        critical_bps,
        emergency_bps,
    }
    .publish(env);
}

/// Returns the global default severity thresholds.
pub fn get_severity_thresholds(env: &Env) -> SeverityThresholds {
    env.storage()
        .persistent()
        .get(&DataKey::CfgSeverityThresholds)
        .unwrap_or_else(default_thresholds)
}

/// Sets a per-asset severity threshold override. Admin-only.
///
/// # Panics
/// * [`ErrorCode::NotAuthorized`] — caller is not the admin.
/// * [`ErrorCode::InvalidSeverityThresholds`] — thresholds are not strictly
///   increasing, or `warning_bps == 0`.
pub fn set_asset_severity_thresholds(
    env: &Env,
    asset: Address,
    warning_bps: u32,
    critical_bps: u32,
    emergency_bps: u32,
) {
    let admin = get_admin(env);
    admin.require_auth();

    let thresholds = SeverityThresholds {
        warning_bps,
        critical_bps,
        emergency_bps,
    };
    validate_thresholds(env, &thresholds);

    let key = DataKey::AssetSeverityThresholds(asset);
    env.storage().persistent().set(&key, &thresholds);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);

    SeverityThresholdsSetEvent {
        is_asset_override: true,
        warning_bps,
        critical_bps,
        emergency_bps,
    }
    .publish(env);
}

/// Returns the effective severity thresholds for `asset`: the per-asset override
/// if one is configured, otherwise the global default.
pub fn get_asset_severity_thresholds(env: &Env, asset: Address) -> SeverityThresholds {
    let key = DataKey::AssetSeverityThresholds(asset);
    if let Some(t) = env.storage().persistent().get(&key) {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
        t
    } else {
        get_severity_thresholds(env)
    }
}

// ─── Classification & routing ───────────────────────────────────────────────────

/// Classifies a basis-point price movement against a set of thresholds.
pub fn classify(movement_bps: u32, thresholds: &SeverityThresholds) -> AlertSeverity {
    if movement_bps >= thresholds.emergency_bps {
        AlertSeverity::Emergency
    } else if movement_bps >= thresholds.critical_bps {
        AlertSeverity::Critical
    } else if movement_bps >= thresholds.warning_bps {
        AlertSeverity::Warning
    } else {
        AlertSeverity::Info
    }
}

/// Maps a severity level to the channel it should be routed to.
///
/// `Warning` goes to the dashboard; `Critical` and `Emergency` page an on-call
/// operator. `Info` is not routed anywhere by the caller (see `evaluate_and_route`).
pub fn route_for(severity: AlertSeverity) -> AlertChannel {
    match severity {
        AlertSeverity::Info | AlertSeverity::Warning => AlertChannel::Dashboard,
        AlertSeverity::Critical | AlertSeverity::Emergency => AlertChannel::Page,
    }
}

/// Classifies `movement_bps` for `asset`, records it, and emits a
/// [`SeverityAlertEvent`] when the severity is `Warning` or above.
///
/// Called internally from [`crate::alerts::dispatch_alerts`] and
/// [`crate::alerting::check_and_alert_deviation`] after each computes a movement
/// in basis points, so both the on-chain subscription path and the off-chain
/// reference-price path share one severity taxonomy and routing table.
///
/// Returns the classified `(severity, channel)` pair so callers can, e.g., only
/// invoke expensive callback dispatch for `Critical`/`Emergency` movements.
pub fn evaluate_and_route(
    env: &Env,
    asset: &Address,
    movement_bps: u32,
) -> (AlertSeverity, AlertChannel) {
    let thresholds = get_asset_severity_thresholds(env, asset.clone());
    let severity = classify(movement_bps, &thresholds);
    let channel = route_for(severity);

    let key = DataKey::LastAlertSeverity(asset.clone());
    env.storage().persistent().set(&key, &severity);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);

    if severity != AlertSeverity::Info {
        SeverityAlertEvent {
            asset: asset.clone(),
            severity: severity as u32,
            channel: channel as u32,
            movement_bps,
            ledger: env.ledger().sequence(),
        }
        .publish(env);
    }

    (severity, channel)
}

/// Returns the most recently classified severity for `asset`, or `None` if no
/// movement has ever been evaluated for it.
pub fn get_last_alert_severity(env: &Env, asset: Address) -> Option<AlertSeverity> {
    env.storage()
        .persistent()
        .get(&DataKey::LastAlertSeverity(asset))
}

// ─── Ingest lag calibration ─────────────────────────────────────────────────────

/// Warning/page thresholds for a single ingest-lag metric, in ledgers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LagThresholds {
    pub warning_ledgers: u32,
    pub critical_ledgers: u32,
}

impl LagThresholds {
    pub fn default_objective() -> Self {
        LagThresholds {
            warning_ledgers: DEFAULT_LAG_WARNING_LEDGERS,
            critical_ledgers: DEFAULT_LAG_CRITICAL_LEDGERS,
        }
    }
}

/// Classifies an ingest lag (in ledgers) against [`LagThresholds`].
///
/// `Warning` at/above `warning_ledgers`, `Critical` (page) at/above
/// `critical_ledgers`, otherwise `Info`.
pub fn classify_lag(lag_ledgers: u32, thresholds: &LagThresholds) -> AlertSeverity {
    if lag_ledgers >= thresholds.critical_ledgers {
        AlertSeverity::Critical
    } else if lag_ledgers >= thresholds.warning_ledgers {
        AlertSeverity::Warning
    } else {
        AlertSeverity::Info
    }
}

/// Evaluates both ingest-lag metrics for `asset` on `network` and emits a
/// [`LagAlertEvent`] for each metric whose severity is `Warning` or above.
///
/// `source_to_indexer_lag` and `ledger_to_consumer_lag` are measured in ledgers.
/// The emitted event carries the affected `network` and `asset` so a replayed lag
/// incident routes to the expected severity with full context.
///
/// Returns the `(source_to_indexer, ledger_to_consumer)` severities so callers
/// can assert on the classification (e.g. in replay tests).
pub fn evaluate_ingest_lag(
    env: &Env,
    network: &String,
    asset: &Address,
    source_to_indexer_lag: u32,
    ledger_to_consumer_lag: u32,
) -> (AlertSeverity, AlertSeverity) {
    let thresholds = LagThresholds::default_objective();

    let source_severity = classify_lag(source_to_indexer_lag, &thresholds);
    let consumer_severity = classify_lag(ledger_to_consumer_lag, &thresholds);

    if source_severity != AlertSeverity::Info {
        LagAlertEvent {
            network: network.clone(),
            asset: asset.clone(),
            metric: 0, // source-to-indexer
            lag_ledgers: source_to_indexer_lag,
            severity: source_severity as u32,
            channel: route_for(source_severity) as u32,
            ledger: env.ledger().sequence(),
        }
        .publish(env);
    }

    if consumer_severity != AlertSeverity::Info {
        LagAlertEvent {
            network: network.clone(),
            asset: asset.clone(),
            metric: 1, // ledger-to-consumer
            lag_ledgers: ledger_to_consumer_lag,
            severity: consumer_severity as u32,
            channel: route_for(consumer_severity) as u32,
            ledger: env.ledger().sequence(),
        }
        .publish(env);
    }

    (source_severity, consumer_severity)
}
