//! Admin audit log module with hash chain (#239)
//!
//! Implements an append-only audit trail for all admin actions with cryptographic
//! hash chain to prevent tampering. Each entry includes a SHA-256 hash of the
//! previous entry, forming an immutable chain.
//!
//! Also provides a per-source submission conformance scorecard (#643) computed
//! from retained audit events, with explicit windows and missing-data tracking.

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

/// Explicit window over which a conformance scorecard is computed (#643).
///
/// Both a ledger range and a timestamp range are recorded so that a scorecard
/// can be reproduced exactly from retained events. `from_ledger`/`to_ledger`
/// are inclusive; `from_timestamp`/`to_timestamp` are inclusive seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScorecardWindow {
    pub from_ledger: u32,
    pub to_ledger: u32,
    pub from_timestamp: u64,
    pub to_timestamp: u64,
}

/// Per-source submission conformance scorecard (#643).
///
/// Reports schema validity, timestamp validity, submission continuity, and
/// accepted/rejected counts for a single source over an explicit window.
///
/// `has_data` distinguishes "no retained events in window" (missing data)
/// from "events present but zero failures" (a clean scorecard). When
/// `has_data` is false, all counts are zero and must not be read as a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceConformanceScorecard {
    pub source: Address,
    pub window: ScorecardWindow,
    /// Whether any retained events for this source fall inside the window.
    pub has_data: bool,
    /// Total submissions observed for this source within the window.
    pub total_submissions: u32,
    /// Submissions whose payload passed schema validation.
    pub schema_valid: u32,
    /// Submissions whose payload failed schema validation.
    pub schema_invalid: u32,
    /// Submissions whose timestamp was within the accepted skew.
    pub timestamp_valid: u32,
    /// Submissions whose timestamp was outside the accepted skew.
    pub timestamp_invalid: u32,
    /// Accepted submissions (schema + timestamp valid).
    pub accepted: u32,
    /// Rejected submissions (any validity failure).
    pub rejected: u32,
    /// Largest gap (in ledgers) between consecutive submissions in the window.
    pub max_continuity_gap: u32,
    /// Number of continuity gaps that exceeded the expected cadence.
    pub continuity_breaks: u32,
}

/// Compute a per-source submission conformance scorecard (#643).
///
/// Scans retained audit entries for `source` whose ledger falls within
/// `[window.from_ledger, window.to_ledger]` and whose timestamp falls within
/// `[window.from_timestamp, window.to_timestamp]`. Results are deterministic
/// and reproducible from the retained audit log alone.
///
/// A submission is considered accepted when its payload is schema-valid and
/// its timestamp is valid; otherwise it is rejected. Continuity is measured
/// as the ledger gap between consecutive submissions; a gap larger than
/// `max_expected_gap` counts as a continuity break.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `source` - The source address to score.
/// * `window` - The explicit ledger/timestamp window to evaluate.
/// * `max_expected_gap` - Maximum acceptable ledger gap between submissions.
///
/// # Returns
///
/// A `SourceConformanceScorecard` with `has_data` set to false when no
/// retained events for `source` fall inside the window.
pub fn compute_source_conformance_scorecard(
    env: &Env,
    source: Address,
    window: ScorecardWindow,
    max_expected_gap: u32,
) -> SourceConformanceScorecard {
    let entry_count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::AuditEntryCount)
        .unwrap_or(0);

    let mut scorecard = SourceConformanceScorecard {
        source: source.clone(),
        window: window.clone(),
        has_data: false,
        total_submissions: 0,
        schema_valid: 0,
        schema_invalid: 0,
        timestamp_valid: 0,
        timestamp_invalid: 0,
        accepted: 0,
        rejected: 0,
        max_continuity_gap: 0,
        continuity_breaks: 0,
    };

    let mut last_ledger: Option<u32> = None;

    for i in 1..=entry_count {
        let entry = match env
            .storage()
            .persistent()
            .get::<_, AuditEntry>(&DataKey::AuditEntry(i))
        {
            Some(e) => e,
            None => continue,
        };

        if entry.admin != source {
            continue;
        }
        if entry.ledger < window.from_ledger || entry.ledger > window.to_ledger {
            continue;
        }
        if entry.timestamp < window.from_timestamp || entry.timestamp > window.to_timestamp {
            continue;
        }

        scorecard.has_data = true;
        scorecard.total_submissions += 1;

        // Schema validity: a non-empty payload is treated as schema-valid.
        let schema_ok = !entry.data.is_empty();
        if schema_ok {
            scorecard.schema_valid += 1;
        } else {
            scorecard.schema_invalid += 1;
        }

        // Timestamp validity: must be non-zero and within the window bounds.
        let timestamp_ok = entry.timestamp != 0
            && entry.timestamp >= window.from_timestamp
            && entry.timestamp <= window.to_timestamp;
        if timestamp_ok {
            scorecard.timestamp_valid += 1;
        } else {
            scorecard.timestamp_invalid += 1;
        }

        if schema_ok && timestamp_ok {
            scorecard.accepted += 1;
        } else {
            scorecard.rejected += 1;
        }

        // Submission continuity: gap between consecutive submissions.
        if let Some(prev) = last_ledger {
            let gap = entry.ledger.saturating_sub(prev);
            if gap > scorecard.max_continuity_gap {
                scorecard.max_continuity_gap = gap;
            }
            if gap > max_expected_gap {
                scorecard.continuity_breaks += 1;
            }
        }
        last_ledger = Some(entry.ledger);
    }

    scorecard
}
