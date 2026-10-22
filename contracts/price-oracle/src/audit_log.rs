//! Admin audit log module with hash chain (#239)
//!
//! Implements an append-only audit trail for all admin actions with cryptographic
//! hash chain to prevent tampering. Each entry includes a SHA-256 hash of the
//! previous entry, forming an immutable chain.
//!
//! Also provides governance participation and concentration guardrails (#662):
//! transparent indicators for approval concentration, quorum participation, and
//! inactive signers, computed from the auditable audit records, plus a scheduled
//! report that alerts when configured thresholds are breached.

use soroban_sdk::{panic_with_error, symbol_short, Address, Bytes, Env, Symbol, Vec};

use crate::events::AdminAuditEntryAppendedEvent;
use crate::storage::{get_admin, LEDGER_BUMP, LEDGER_THRESHOLD};
use crate::types::{AuditEntry, DataKey, ErrorCode};

/// Append an admin action to the immutable audit log.
///
/// This is called internally by all admin actions to create a tamper-evident record.
/// Each entry is hashed with the previous entry's hash to form a chain.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `action` - The action symbol (e.g., "pause", "setadm").
/// * `admin` - The admin address that performed the action.
/// * `data` - Arbitrary data associated with the action.
pub fn append_audit_entry(env: &Env, action: Symbol, admin: Address, data: Bytes) {
    let current_ledger = env.ledger().sequence();
    let timestamp = env.ledger().timestamp();

    // Get the current audit log head (previous hash) as Bytes
    let previous_hash: Bytes = env
        .storage()
        .persistent()
        .get(&DataKey::AuditLogHead)
        .unwrap_or_else(|| Bytes::new(env));

    // Get next entry ID
    let entry_count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::AuditEntryCount)
        .unwrap_or(0);
    let entry_id = entry_count + 1;

    // Compute hash of this entry: sha256(previous_hash || data || timestamp || entry_id)
    let mut entry_data = Bytes::new(env);
    entry_data.append(&previous_hash);

    entry_data.append(&data);

    entry_data.append(&Bytes::from_slice(env, &timestamp.to_le_bytes()));

    entry_data.append(&Bytes::from_slice(env, &entry_id.to_le_bytes()));

    // Compute SHA-256 hash
    // Compute SHA-256 and store as `Bytes`
    let sha = env.crypto().sha256(&entry_data);
    let sha_bytesn: soroban_sdk::BytesN<32> = sha.into();
    let current_hash = Bytes::from_slice(env, &sha_bytesn.to_array());

    // Create audit entry
    let entry = AuditEntry {
        id: entry_id,
        action: action.clone(),
        admin: admin.clone(),
        timestamp,
        data: data.clone(),
        previous_hash: previous_hash.clone(),
        current_hash: current_hash.clone(),
        ledger: current_ledger,
    };

    // Store entry
    env.storage()
        .persistent()
        .set(&DataKey::AuditEntry(entry_id), &entry);

    // Update head pointer to current hash
    env.storage()
        .persistent()
        .set(&DataKey::AuditLogHead, &current_hash);

    // Increment counter
    env.storage()
        .persistent()
        .set(&DataKey::AuditEntryCount, &entry_id);

    // Emit event
    AdminAuditEntryAppendedEvent {
        entry_id,
        action,
        admin: admin.clone(),
        timestamp,
        ledger: current_ledger,
    }
    .publish(env);
}

/// Get audit log entries within a range.
///
/// Retrieves audit entries starting from `from_id` up to `limit` entries.
/// Returns entries in chronological order (oldest first).
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `from_id` - Starting entry ID (inclusive). Use 0 to start from the beginning.
/// * `limit` - Maximum number of entries to return (0 = no limit, capped at 500).
///
/// # Returns
///
/// Ordered list of audit entries.
pub fn get_admin_audit_log(env: &Env, from_id: u32, limit: u32) -> Vec<AuditEntry> {
    let effective_limit = if limit == 0 || limit > 500 {
        500
    } else {
        limit
    };
    let start_id = if from_id == 0 { 1 } else { from_id };

    let entry_count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::AuditEntryCount)
        .unwrap_or(0);

    let mut results = Vec::new(env);
    let mut i = start_id;
    let mut returned = 0u32;

    while i <= entry_count && returned < effective_limit {
        if let Some(entry) = env
            .storage()
            .persistent()
            .get::<_, AuditEntry>(&DataKey::AuditEntry(i))
        {
            results.push_back(entry);
            returned += 1;
        }
        i += 1;
    }

    results
}

/// Verify the integrity of the audit chain.
///
/// Walks through the entire audit log and verifies that each entry's hash
/// matches the hash of (previous_hash || entry_data). Returns true if the
/// chain is intact, false if tampering is detected.
///
/// WARNING: This operation is O(n) where n is the number of audit entries.
/// Use sparingly in time-critical paths.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
///
/// # Returns
///
/// `true` if the audit chain is valid, `false` if tampering is detected.
pub fn verify_audit_chain(env: &Env) -> bool {
    let entry_count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::AuditEntryCount)
        .unwrap_or(0);

    if entry_count == 0 {
        return true; // Empty chain is valid
    }

    let mut previous_hash: Bytes = Bytes::new(env);

    for i in 1..=entry_count {
        if let Some(entry) = env
            .storage()
            .persistent()
            .get::<_, AuditEntry>(&DataKey::AuditEntry(i))
        {
            // Verify previous hash matches expected
            if entry.previous_hash != previous_hash {
                return false; // Chain broken or unexpected previous hash
            }

            // Recompute hash over the same fields used when appending
            let mut entry_data = Bytes::new(env);
            entry_data.append(&entry.previous_hash);
            entry_data.append(&entry.data);
            entry_data.append(&Bytes::from_slice(env, &entry.timestamp.to_le_bytes()));
            entry_data.append(&Bytes::from_slice(env, &entry.id.to_le_bytes()));

            let recomputed_sha = env.crypto().sha256(&entry_data);
            let recomputed_bytesn: soroban_sdk::BytesN<32> = recomputed_sha.into();
            let recomputed_hash = Bytes::from_slice(env, &recomputed_bytesn.to_array());
            if recomputed_hash != entry.current_hash {
                return false; // Hash mismatch
            }

            previous_hash = entry.current_hash.clone();
        } else {
            return false; // Missing entry
        }
    }

    true
}

/// Get the total number of audit entries in the log.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
///
/// # Returns
///
/// The total count of audit entries.
pub fn get_audit_log_count(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::AuditEntryCount)
        .unwrap_or(0)
}

/// Get the current audit log head hash.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
///
/// # Returns
///
/// The SHA-256 hash of the most recent audit entry, or empty Bytes if log is empty.
pub fn get_audit_log_head(env: &Env) -> Bytes {
    env.storage()
        .persistent()
        .get(&DataKey::AuditLogHead)
        .unwrap_or_else(|| Bytes::new(env))
}

// ---------------------------------------------------------------------------
// Governance participation and concentration guardrails (#662)
// ---------------------------------------------------------------------------

/// Transparent governance indicators computed from auditable audit records.
///
/// All values are expressed in basis points (0..=10_000) so they are
/// deterministic, integer-only, and easy to compare against thresholds.
///
/// * `approval_concentration_bps` - share of admin actions performed by the
///   single most active signer. High values indicate one signer dominates.
/// * `quorum_participation_bps` - share of distinct signers that have acted
///   at least once relative to the total number of known signers.
/// * `inactive_signers` - number of known signers with no recorded action.
/// * `total_actions` - total auditable admin actions considered.
/// * `distinct_signers` - number of distinct signers that have acted.
/// * `known_signers` - total number of known signers considered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernanceIndicators {
    pub approval_concentration_bps: u32,
    pub quorum_participation_bps: u32,
    pub inactive_signers: u32,
    pub total_actions: u32,
    pub distinct_signers: u32,
    pub known_signers: u32,
}

/// Configured thresholds for governance guardrail alerts.
///
/// A report raises an alert for an indicator when its value breaches the
/// configured threshold. Thresholds are expressed in basis points for the
/// concentration and participation indicators, and as a raw count for the
/// inactive signer indicator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernanceThresholds {
    /// Alert when approval concentration exceeds this value (bps).
    pub max_approval_concentration_bps: u32,
    /// Alert when quorum participation falls below this value (bps).
    pub min_quorum_participation_bps: u32,
    /// Alert when inactive signers exceed this count.
    pub max_inactive_signers: u32,
}

/// Result of a scheduled governance guardrail report.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GovernanceReport {
    pub indicators: GovernanceIndicators,
    pub concentration_alert: bool,
    pub participation_alert: bool,
    pub inactive_alert: bool,
    pub ledger: u32,
    pub timestamp: u64,
}

impl GovernanceReport {
    /// Returns true if any configured threshold was breached.
    pub fn has_alert(&self) -> bool {
        self.concentration_alert || self.participation_alert || self.inactive_alert
    }
}

/// Compute governance indicators from the auditable audit records.
///
/// `known_signers` is the set of signers expected to participate (e.g. the
/// configured admin set). Indicators are derived purely from the append-only
/// audit log so they are reproducible and auditable.
///
/// Boundary conditions:
/// * zero known signers -> all indicators are zero, no division by zero.
/// * zero actions -> concentration and participation are zero.
/// * single signer performing all actions -> concentration is 10_000 bps.
pub fn compute_governance_indicators(
    env: &Env,
    known_signers: &Vec<Address>,
) -> GovernanceIndicators {
    let entry_count = get_audit_log_count(env);
    let known = known_signers.len();

    // Count actions per known signer and track distinct active signers.
    let mut counts: Vec<u32> = Vec::new(env);
    for _ in 0..known {
        counts.push_back(0);
    }

    let mut total_actions: u32 = 0;
    let mut i: u32 = 1;
    while i <= entry_count {
        if let Some(entry) = env
            .storage()
            .persistent()
            .get::<_, AuditEntry>(&DataKey::AuditEntry(i))
        {
            total_actions += 1;
            let mut j: u32 = 0;
            while j < known {
                if known_signers.get(j).unwrap() == entry.admin {
                    let c = counts.get(j).unwrap();
                    counts.set(j, c + 1);
                    break;
                }
                j += 1;
            }
        }
        i += 1;
    }

    // Determine the most active signer and the number of distinct active signers.
    let mut max_count: u32 = 0;
    let mut distinct_signers: u32 = 0;
    let mut j: u32 = 0;
    while j < known {
        let c = counts.get(j).unwrap();
        if c > max_count {
            max_count = c;
        }
        if c > 0 {
            distinct_signers += 1;
        }
        j += 1;
    }

    let approval_concentration_bps = if total_actions == 0 {
        0
    } else {
        // max_count / total_actions in bps, integer math.
        (max_count as u64 * 10_000 / total_actions as u64) as u32
    };

    let quorum_participation_bps = if known == 0 {
        0
    } else {
        (distinct_signers as u64 * 10_000 / known as u64) as u32
    };

    let inactive_signers = known.saturating_sub(distinct_signers);

    GovernanceIndicators {
        approval_concentration_bps,
        quorum_participation_bps,
        inactive_signers,
        total_actions,
        distinct_signers,
        known_signers: known,
    }
}

/// Run the scheduled governance guardrail report.
///
/// Computes the indicators from auditable records and evaluates them against
/// the configured thresholds, producing a report with per-indicator alerts.
/// This is intended to be invoked on a schedule (e.g. by an operator cron or
/// an off-chain monitor) and is deterministic for a given ledger state.
pub fn run_governance_report(
    env: &Env,
    known_signers: &Vec<Address>,
    thresholds: &GovernanceThresholds,
) -> GovernanceReport {
    let indicators = compute_governance_indicators(env, known_signers);

    let concentration_alert =
        indicators.approval_concentration_bps > thresholds.max_approval_concentration_bps;
    let participation_alert =
        indicators.quorum_participation_bps < thresholds.min_quorum_participation_bps;
    let inactive_alert = indicators.inactive_signers > thresholds.max_inactive_signers;

    GovernanceReport {
        indicators,
        concentration_alert,
        participation_alert,
        inactive_alert,
        ledger: env.ledger().sequence(),
        timestamp: env.ledger().timestamp(),
    }
}
