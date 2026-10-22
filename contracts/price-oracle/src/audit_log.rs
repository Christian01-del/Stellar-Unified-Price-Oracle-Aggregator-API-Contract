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
//!
//! Also provides a public governance decision archive (#665): decisions are
//! published with rationale, execution status, and evidence links in a
//! searchable, versioned, append-only archive so an auditor can trace a sample
//! decision from proposal through on-chain result without internal access.

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
// Public governance decision archive (#665)
// ---------------------------------------------------------------------------

/// Current schema version of the public governance decision archive.
///
/// Bump this when the on-chain archive record layout changes so auditors can
/// distinguish format revisions. Entries are immutable once published; a new
/// schema version is recorded alongside each entry so historical records remain
/// traceable across revisions.
pub const GOVERNANCE_ARCHIVE_SCHEMA_VERSION: u32 = 1;

/// Execution status of a published governance decision.
///
/// Encoded as a `u32` so it is stable across SDK versions and easy to filter.
/// * `0` = Proposed (recorded, not yet executed)
/// * `1` = Executed (on-chain result available)
/// * `2` = Failed (execution attempted and reverted)
/// * `3` = Cancelled (withdrawn before execution)
#[derive(Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum GovernanceDecisionStatus {
    Proposed = 0,
    Executed = 1,
    Failed = 2,
    Cancelled = 3,
}

impl GovernanceDecisionStatus {
    pub fn as_u32(self) -> u32 {
        self as u32
    }

    pub fn from_u32(value: u32) -> Option<Self> {
        match value {
            0 => Some(Self::Proposed),
            1 => Some(Self::Executed),
            2 => Some(Self::Failed),
            3 => Some(Self::Cancelled),
            _ => None,
        }
    }
}

/// A single immutable entry in the public governance decision archive.
///
/// Contains everything an external auditor needs to trace a decision from
/// proposal through on-chain result: the decision id, rationale, execution
/// status, evidence links (e.g. proposal/transaction hashes), the ledger and
/// timestamp at which it was recorded, and the schema version it was written
/// under.
#[derive(Clone)]
pub struct GovernanceDecisionRecord {
    pub id: u32,
    pub schema_version: u32,
    pub status: u32,
    pub rationale: Bytes,
    pub evidence: Bytes,
    pub ledger: u32,
    pub timestamp: u64,
}

/// Publish a governance decision to the public archive.
///
/// Entries are append-only and immutable: once an id is published it cannot be
/// overwritten, guaranteeing a stable, versioned audit trail. The record is
/// stamped with the current schema version and the ledger/timestamp at which it
/// was published.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `id` - Unique decision id (must not already exist).
/// * `status` - Execution status (see [`GovernanceDecisionStatus`]).
/// * `rationale` - Human-readable rationale bytes.
/// * `evidence` - Evidence links (proposal/transaction hashes, URLs, etc.).
///
/// # Panics
///
/// Panics with `ErrorCode::InvalidParameter` if `id` is zero, the status is
/// unknown, or a decision with the same id has already been published.
pub fn publish_governance_decision(
    env: &Env,
    id: u32,
    status: u32,
    rationale: Bytes,
    evidence: Bytes,
) {
    if id == 0 || GovernanceDecisionStatus::from_u32(status).is_none() {
        panic_with_error!(env, ErrorCode::InvalidParameter);
    }

    if env
        .storage()
        .persistent()
        .has(&DataKey::GovernanceDecision(id))
    {
        // Immutable archive: reject attempts to overwrite an existing entry.
        panic_with_error!(env, ErrorCode::InvalidParameter);
    }

    let record = GovernanceDecisionRecord {
        id,
        schema_version: GOVERNANCE_ARCHIVE_SCHEMA_VERSION,
        status,
        rationale,
        evidence,
        ledger: env.ledger().sequence(),
        timestamp: env.ledger().timestamp(),
    };

    env.storage()
        .persistent()
        .set(&DataKey::GovernanceDecision(id), &record);

    let count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::GovernanceDecisionCount)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&DataKey::GovernanceDecisionCount, &(count + 1));
}

/// Fetch a single governance decision by id.
///
/// Returns `None` for unknown ids so callers can handle missing entries
/// without panicking.
pub fn get_governance_decision(env: &Env, id: u32) -> Option<GovernanceDecisionRecord> {
    env.storage()
        .persistent()
        .get(&DataKey::GovernanceDecision(id))
}

/// Total number of published governance decisions.
pub fn get_governance_decision_count(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::GovernanceDecisionCount)
        .unwrap_or(0)
}

/// Paginated listing of governance decisions, optionally filtered by status.
///
/// Iterates ids in `[from_id, from_id + limit)` (or from 1 when `from_id == 0`),
/// returning only entries whose status matches `status_filter` when provided.
/// `limit` is capped at 500 to bound compute. Unknown ids are skipped, so the
/// result may be shorter than `limit`.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `from_id` - Starting decision id (inclusive). Use 0 to start from the beginning.
/// * `limit` - Maximum number of entries to return (0 = no limit, capped at 500).
/// * `status_filter` - Optional status to filter by; `None` returns all statuses.
pub fn list_governance_decisions(
    env: &Env,
    from_id: u32,
    limit: u32,
    status_filter: Option<u32>,
) -> Vec<GovernanceDecisionRecord> {
    let effective_limit = if limit == 0 || limit > 500 {
        500
    } else {
        limit
    };
    let start_id = if from_id == 0 { 1 } else { from_id };
    let end_id = start_id.saturating_add(effective_limit);

    let mut results = Vec::new(env);
    let mut i = start_id;
    while i < end_id {
        if let Some(record) = env
            .storage()
            .persistent()
            .get::<_, GovernanceDecisionRecord>(&DataKey::GovernanceDecision(i))
        {
            let matches = match status_filter {
                Some(s) => record.status == s,
                None => true,
            };
            if matches {
                results.push_back(record);
            }
        }
        i += 1;
    }

    results
}

// ---------------------------------------------------------------------------
// Governance participation and concentration guardrails (#662)
// ---------------------------------------------------------------------------

/// Transparent governance indicators computed from auditable audit records.
///
/// All values are expressed in basis points (0..=10_000) so they are
/// deterministic, integer-only, and easy to compare against thresholds.
///
/// * `approval_concentration_bp