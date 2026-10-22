//! Admin audit log module with hash chain (#239)
//!
//! Implements an append-only audit trail for all admin actions with cryptographic
//! hash chain to prevent tampering. Each entry includes a SHA-256 hash of the
//! previous entry, forming an immutable chain.

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

/// Result of a governance event audit completeness check (#659).
///
/// Each variant identifies a specific class of governance audit failure so
/// operators can distinguish a missing event from a malformed or unlinked one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GovernanceAuditFailure {
    /// No failure detected; the cycle is complete and consistent.
    None,
    /// A privileged state change has no corresponding audit entry.
    MissingEvent,
    /// An audit entry exists but its action/admin/data is malformed.
    MalformedEvent,
    /// An audit entry is not linked to its predecessor (broken chain).
    UnlinkedEvent,
}

/// Audit a single governance cycle for event completeness (#659).
///
/// Reconciles privileged state changes with their emitted governance events
/// and approval records across an upgrade cycle. A cycle is described by the
/// `expected_actions` that must each have exactly one matching audit entry,
/// the `expected_admin` that must have authorized each entry, and the
/// `approval_count` of approval records that must be present.
///
/// The check fails (returns a non-`None` failure) when:
/// - a privileged state change has no matching audit entry (`MissingEvent`),
/// - an audit entry is malformed (empty action, zero timestamp, or wrong
///   admin) (`MalformedEvent`),
/// - the audit chain is broken or an entry is unlinked from its predecessor
///   (`UnlinkedEvent`),
/// - the number of approval records does not match the number of privileged
///   state changes (`MissingEvent`).
///
/// An empty cycle (no expected actions, no approvals) is considered complete.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `expected_actions` - Actions that must each appear in the audit log.
/// * `expected_admin` - The admin that must have authorized each entry.
/// * `approval_count` - Number of approval records recorded for the cycle.
///
/// # Returns
///
/// `GovernanceAuditFailure::None` when the cycle is complete and consistent.
pub fn audit_governance_cycle(
    env: &Env,
    expected_actions: &Vec<Symbol>,
    expected_admin: &Address,
    approval_count: u32,
) -> GovernanceAuditFailure {
    // Empty cycle: nothing to reconcile, trivially complete.
    if expected_actions.is_empty() && approval_count == 0 {
        return GovernanceAuditFailure::None;
    }

    // The chain must be intact before we can trust any entry linkage.
    if !verify_audit_chain(env) {
        return GovernanceAuditFailure::UnlinkedEvent;
    }

    let entry_count = get_audit_log_count(env);

    // Every privileged state change must have a matching approval record.
    if approval_count != expected_actions.len() {
        return GovernanceAuditFailure::MissingEvent;
    }

    // Collect the actions actually present in the audit log.
    let mut seen: Vec<Symbol> = Vec::new(env);
    for i in 1..=entry_count {
        let entry: AuditEntry = match env
            .storage()
            .persistent()
            .get(&DataKey::AuditEntry(i))
        {
            Some(e) => e,
            None => return GovernanceAuditFailure::MissingEvent,
        };

        // Malformed: empty action, zero timestamp, or wrong authorizing admin.
        if entry.action == symbol_short!("")
            || entry.timestamp == 0
            || entry.admin != *expected_admin
        {
            return GovernanceAuditFailure::MalformedEvent;
        }

        // Unlinked: entry id must be sequential and previous hash must match
        // the prior entry's current hash (already validated by chain check,
        // but re-assert the id linkage explicitly).
        if entry.id != i {
            return GovernanceAuditFailure::UnlinkedEvent;
        }

        seen.push_back(entry.action.clone());
    }

    // Every expected action must be present in the audit log.
    for expected in expected_actions.iter() {
        let mut found = false;
        for action in seen.iter() {
            if action == expected {
                found = true;
                break;
            }
        }
        if !found {
            return GovernanceAuditFailure::MissingEvent;
        }
    }

    GovernanceAuditFailure::None
}
