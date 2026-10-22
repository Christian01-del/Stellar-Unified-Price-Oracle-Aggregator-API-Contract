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
//! ## Incident severity & decision authority matrix
//!
//! Each alert severity maps to an incident severity (`SEV1`–`SEV4`) with a fixed
//! response role, pause authority, communications requirement, and escalation
//! deadline. The matrix is the single source of truth used by the tabletop check
//! (`tabletop_exercise_covers_every_severity`) and documented in
//! `docs/operations/incident-response.md`.
//!
//! | Alert       | Incident | Response role        | Pause authority        | Comms                          | Escalation deadline |
//! |-------------|----------|----------------------|------------------------|--------------------------------|---------------------|
//! | `Info`      | `SEV4`   | On-call engineer     | None                   | None                           | Next business day   |
//! | `Warning`   | `SEV3`   | On-call engineer     | None                   | Dashboard status note          | 4 hours             |
//! | `Critical`  | `SEV2`   | Incident commander   | Incident commander     | Status page + stakeholder ping | 30 minutes          |
//! | `Emergency` | `SEV1`   | Incident commander   | Incident commander     | Status page + page all + postmortem | 15 minutes      |

use soroban_sdk::{panic_with_error, Address, Env};

use crate::events::{SeverityAlertEvent, SeverityThresholdsSetEvent};
use crate::storage::{get_admin, LEDGER_BUMP, LEDGER_THRESHOLD};
use crate::types::{AlertChannel, AlertSeverity, DataKey, ErrorCode, SeverityThresholds};

/// Default global severity thresholds, in basis points of price movement.
pub const DEFAULT_WARNING_BPS: u32 = 300; // 3%
pub const DEFAULT_CRITICAL_BPS: u32 = 1_000; // 10%
pub const DEFAULT_EMERGENCY_BPS: u32 = 2_500; // 25%

// ─── Incident severity & decision authority matrix ─────────────────────────────

/// Incident severity level (`SEV1`–`SEV4`) derived from an alert severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum IncidentSeverity {
    Sev1 = 1,
    Sev2 = 2,
    Sev3 = 3,
    Sev4 = 4,
}

/// Who is accountable for driving the response for an incident severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResponseRole {
    /// On-call engineer triages and resolves; no formal incident command.
    OnCallEngineer,
    /// Incident commander owns the response, comms, and escalation.
    IncidentCommander,
}

/// Who may pause the oracle for an incident severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PauseAuthority {
    /// No pause is authorized at this severity.
    None,
    /// The incident commander may pause the oracle.
    IncidentCommander,
}

/// Communications obligations for an incident severity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Communications {
    /// No external communications required.
    None,
    /// Post a status note on the operator dashboard.
    DashboardNote,
    /// Update the public status page and ping stakeholders.
    StatusPageAndStakeholders,
    /// Status page, page all responders, and file a postmortem.
    StatusPagePageAllAndPostmortem,
}

/// Escalation deadline, expressed in minutes from incident declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EscalationDeadline {
    /// Escalate by the next business day.
    NextBusinessDay,
    /// Escalate within 4 hours.
    FourHours,
    /// Escalate within 30 minutes.
    ThirtyMinutes,
    /// Escalate within 15 minutes.
    FifteenMinutes,
}

impl EscalationDeadline {
    /// Deadline in minutes; `NextBusinessDay` is reported as 24 hours.
    pub fn minutes(self) -> u32 {
        match self {
            EscalationDeadline::NextBusinessDay => 24 * 60,
            EscalationDeadline::FourHours => 4 * 60,
            EscalationDeadline::ThirtyMinutes => 30,
            EscalationDeadline::FifteenMinutes => 15,
        }
    }
}

/// A single row of the incident severity and decision authority matrix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IncidentResponse {
    pub incident_severity: IncidentSeverity,
    pub response_role: ResponseRole,
    pub pause_authority: PauseAuthority,
    pub communications: Communications,
    pub escalation_deadline: EscalationDeadline,
}

/// Maps an alert severity to its incident response row.
///
/// This is the authoritative decision table: every alert severity resolves to
/// exactly one incident severity, response role, pause authority, communications
/// requirement, and escalation deadline.
pub fn incident_response_for(severity: AlertSeverity) -> IncidentResponse {
    match severity {
        AlertSeverity::Info => IncidentResponse {
            incident_severity: IncidentSeverity::Sev4,
            response_role: ResponseRole::OnCallEngineer,
            pause_authority: PauseAuthority::None,
            communications: Communications::None,
            escalation_deadline: EscalationDeadline::NextBusinessDay,
        },
        AlertSeverity::Warning => IncidentResponse {
            incident_severity: IncidentSeverity::Sev3,
            response_role: ResponseRole::OnCallEngineer,
            pause_authority: PauseAuthority::None,
            communications: Communications::DashboardNote,
            escalation_deadline: EscalationDeadline::FourHours,
        },
        AlertSeverity::Critical => IncidentResponse {
            incident_severity: IncidentSeverity::Sev2,
            response_role: ResponseRole::IncidentCommander,
            pause_authority: PauseAuthority::IncidentCommander,
            communications: Communications::StatusPageAndStakeholders,
            escalation_deadline: EscalationDeadline::ThirtyMinutes,
        },
        AlertSeverity::Emergency => IncidentResponse {
            incident_severity: IncidentSeverity::Sev1,
            response_role: ResponseRole::IncidentCommander,
            pause_authority: PauseAuthority::IncidentCommander,
            communications: Communications::StatusPagePageAllAndPostmortem,
            escalation_deadline: EscalationDeadline::FifteenMinutes,
        },
    }
}

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
        .get

/* … truncated 3349 chars — edit only what you need near the top … */
