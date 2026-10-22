//! # Off-Chain Price Deviation Alerting System (#199)
//!
//! Monitors for price deviations vs external references and triggers alerts
//! when thresholds are exceeded. Supports multiple notification channels.
//!
//! # Governance Participation & Concentration Guardrails (#662)
//!
//! Provides transparent, auditable indicators for governance health:
//! - **Approval concentration**: share of approvals held by the single most
//!   active signer (basis points of total approvals).
//! - **Quorum participation**: share of eligible signers that participated
//!   (basis points of total signers).
//! - **Inactive signers**: count of eligible signers with zero participation.
//!
//! A scheduled report (`run_governance_report`) recomputes each indicator from
//! auditable signer records and emits an alert when a configured threshold is
//! breached.

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
// Governance participation & concentration guardrails (#662)
// ---------------------------------------------------------------------------

/// Transparent governance health indicators computed from auditable signer
/// records. All ratios are expressed in basis points (1% = 100 bps) so they
/// are integer-exact and easy to compare against configured thresholds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernanceIndicators {
    /// Number of eligible signers considered in this report.
    pub total_signers: u32,
    /// Number of signers that participated (approvals > 0).
    pub active_signers: u32,
    /// Number of eligible signers with zero participation.
    pub inactive_signers: u32,
    /// Total approvals across all signers.
    pub total_approvals: u32,
    /// Approvals held by the single most active signer.
    pub top_signer_approvals: u32,
    /// Approval concentration: top signer's share of total approvals (bps).
    pub approval_concentration_bps: u32,
    /// Quorum participation: active signers' share of total signers (bps).
    pub quorum_participation_bps: u32,
}

/// Configured thresholds for governance guardrail alerts (basis points).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernanceThresholds {
    /// Alert when approval concentration exceeds this value (bps).
    pub max_approval_concentration_bps: u32,
    /// Alert when quorum participation falls below this value (bps).
    pub min_quorum_participation_bps: u32,
    /// Alert when inactive signers exceed this count.
    pub max_inactive_signers: u32,
}

/// Result of a scheduled governance report run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernanceReport {
    pub indicators: GovernanceIndicators,
    /// `true` when approval concentration breached its threshold.
    pub concentration_alert: bool,
    /// `true` when quorum participation fell below its threshold.
    pub quorum_alert: bool,
    /// `true` when inactive signers exceeded their threshold.
    pub inactive_alert: bool,
    /// Ledger sequence at which the report was produced.
    pub ledger: u32,
}

impl GovernanceReport {
    /// `true` if any guardrail threshold was breached.
    pub fn has_alert(&self) -> bool {
        self.concentration_alert || self.quorum_alert || self.inactive_alert
    }
}

/// Computes governance indicators from auditable per-signer approval counts.
///
/// `approvals` is a list of approval counts, one entry per eligible signer.
/// Boundary conditions are handled explicitly:
/// - zero signers: all ratios are 0 and no signer is counted as inactive;
/// - single signer: concentration is 10_000 bps when they have approvals;
/// - full participation: quorum is 10_000 bps and inactive is 0;
/// - all inactive: quorum is 0 bps and inactive equals total signers.
pub fn compute_indicators(approvals: &Vec<u32>) -> GovernanceIndicators {
    let total_signers = approvals.len();
    let mut active_signers: u32 = 0;
    let mut total_approvals: u32 = 0;
    let mut top_signer_approvals: u32 = 0;

    for count in approvals.iter() {
        total_approvals = total_approvals.saturating_add(count);
        if count > 0 {
            active_signers = active_signers.saturating_add(1);
        }
        if count > top_signer_approvals {
            top_signer_approvals = count;
        }
    }

    let inactive_signers = total_signers.saturating_sub(active_signers);

    let approval_concentration_bps = if total_approvals == 0 {
        0
    } else {
        ((top_signer_approvals as u64 * 10_000) / total_approvals as u64) as u32
    };

    let quorum_participation_bps = if total_signers == 0 {
        0
    } else {
        ((active_signers as u64 * 10_000) / total_signers as u64) as u32
    };

    GovernanceIndicators {
        total_signers,
        active_signers,
        inactive_signers,
        total_approvals,
        top_signer_approvals,
        approval_concentration_bps,
        quorum_participation_bps,
    }
}

/// Scheduled governance report: recomputes indicators from auditable signer
/// records and evaluates them against configured thresholds.
///
/// Emits a `GovernanceGuardrailAlertEvent` when any threshold is breached so
/// operators can react. Returns the full report for indexing/auditing.
pub fn run_governance_report(
    env: &Env,
    approvals: &Vec<u32>,
    thresholds: &GovernanceThresholds,
) -> GovernanceReport {
    let indicators = compute_indicators(approvals);

    let concentration_alert =
        indicators.approval_concentration_bps > thresholds.max_approval_concentration_bps;
    let quorum_alert =
        indicators.quorum_participation_bps < thresholds.min_quorum_participation_bps;
    let inactive_alert = indicators.inactive_signers > thresholds.max_inactive_signers;

    let report = GovernanceReport {
        indicators,
        concentration_alert,
        quorum_alert,
        inactive_alert,
        ledger: env.ledger().sequence(),
    };

    if report.has_alert() {
        crate::events::GovernanceGuardrailAlertEvent {
            approval_concentration_bps: report.indicators.approval_concentration_bps,
            quorum_participation_bps: report.indicators.quorum_participation_bps,
            inactive_signers: report.indicators.inactive_signers,
            concentration_alert,
            quorum_alert,
            inactive_alert,
            ledger: report.ledger,
        }
        .publish(env);
    }

    report
}
