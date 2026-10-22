//! Admin audit log module with hash chain (#239)
//!
//! Implements an append-only audit trail for all admin actions with cryptographic
//! hash chain to prevent tampering. Each entry includes a SHA-256 hash of the
//! previous entry, forming an immutable chain.
//!
//! # Source reward appeals and corrections (#674)
//!
//! A source may dispute a measured contribution or payout by submitting an
//! appeal. Appeals are recorded as audit entries (action `appeal`) carrying the
//! source identity, the disputed epoch, the disputed amount and a reason. An
//! authorized reviewer resolves the appeal by appending a `correct` audit entry
//! that references the original appeal entry id. Corrections are *appended* to
//! the chain; the original audit record is never mutated or overwritten, so the
//! full dispute history remains verifiable via [`verify_audit_chain`].

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

/// Encode an appeal payload for storage in the audit log.
///
/// Layout: `source(32) || epoch(4 LE) || disputed_amount(16 LE) || reason`.
/// The reason is appended verbatim so reviewers can inspect the full dispute.
fn encode_appeal(env: &Env, source: &Address, epoch: u32, disputed_amount: i128, reason: &Bytes) -> Bytes {
    let mut payload = Bytes::new(env);
    payload.append(&Bytes::from_slice(env, &source.to_string().into_bytes().to_array()));
    payload.append(&Bytes::from_slice(env, &epoch.to_le_bytes()));
    payload.append(&Bytes::from_slice(env, &disputed_amount.to_le_bytes()));
    payload.append(reason);
    payload
}

/// Submit an appeal disputing a measured contribution or payout.
///
/// The source records a dispute against a specific epoch and amount. The appeal
/// is appended to the audit chain as an `appeal` entry; the original measurement
/// record is left untouched. Returns the id of the appended appeal entry so the
/// reviewer can reference it when resolving.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `source` - The source disputing the measurement (must authorize).
/// * `epoch` - The epoch whose contribution/payout is disputed.
/// * `disputed_amount` - The amount the source believes is incorrect.
/// * `reason` - Human-readable justification for the dispute.
pub fn submit_reward_appeal(
    env: &Env,
    source: Address,
    epoch: u32,
    disputed_amount: i128,
    reason: Bytes,
) -> u32 {
    source.require_auth();

    let payload = encode_appeal(env, &source, epoch, disputed_amount, &reason);
    append_audit_entry(env, symbol_short!("appeal"), source, payload);

    get_audit_log_count(env)
}

/// Resolve a source reward appeal by appending a correction record.
///
/// Only the configured admin (reviewer) may approve a correction. The correction
/// is appended as a `correct` audit entry that references the original appeal
/// entry id and carries the approved amount. The original appeal entry is never
/// modified, so the audit trail preserves both the dispute and its resolution.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `appeal_id` - The audit entry id of the appeal being resolved.
/// * `approved_amount` - The corrected amount approved by the reviewer.
/// * `note` - Reviewer justification recorded alongside the correction.
///
/// # Panics
///
/// Panics with `ErrorCode::Unauthorized` if the caller is not the admin, or with
/// `ErrorCode::InvalidInput` if `appeal_id` does not reference an existing appeal.
pub fn resolve_reward_appeal(env: &Env, appeal_id: u32, approved_amount: i128, note: Bytes) -> u32 {
    let admin = get_admin(env);
    admin.require_auth();

    // The referenced entry must exist and be an appeal; corrections cannot be
    // chained onto arbitrary or missing records.
    let appeal: AuditEntry = match env
        .storage()
        .persistent()
        .get::<_, AuditEntry>(&DataKey::AuditEntry(appeal_id))
    {
        Some(entry) => entry,
        None => panic_with_error!(env, ErrorCode::InvalidInput),
    };
    if appeal.action != symbol_short!("appeal") {
        panic_with_error!(env, ErrorCode::InvalidInput);
    }

    // Encode correction: appeal_id(4 LE) || approved_amount(16 LE) || note.
    let mut payload = Bytes::new(env);
    payload.append(&Bytes::from_slice(env, &appeal_id.to_le_bytes()));
    payload.append(&Bytes::from_slice(env, &approved_amount.to_le_bytes()));
    payload.append(&note);

    append_audit_entry(env, symbol_short!("correct"), admin, payload);

    get_audit_log_count(env)
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

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::testutils::Address as _;

    #[test]
    fn resolving_appeal_appends_correction_without_overwriting_original() {
        let env = Env::default();
        env.mock_all_auths();

        let admin = Address::generate(&env);
        let source = Address::generate(&env);
        env.storage()
            .persistent()
            .set(&DataKey::Admin, &admin);

        let reason = Bytes::from_slice(&env, b"payout undercounted");
        let appeal_id = submit_reward_appeal(&env, source.clone(), 7, 1_000, reason.clone());
        assert_eq!(appeal_id, 1);

        // Snapshot the original appeal record before resolution.
        let original: AuditEntry = env
            .storage()
            .persistent()
            .get(&DataKey::AuditEntry(appeal_id))
            .unwrap();

        let note = Bytes::from_slice(&env, b"approved after review");
        let correction_id = resolve_reward_appeal(&env, appeal_id, 1_500, note);
        assert_eq!(correction_id, 2);

        // Original appeal record is untouched.
        let after: AuditEntry = env
            .storage()
            .persistent()
            .get(&DataKey::AuditEntry(appeal_id))
            .unwrap();
        assert_eq!(after.id, original.id);
        assert_eq!(after.action, original.action);
        assert_eq!(after.data, original.data);
        assert_eq!(after.current_hash, original.current_hash);

        // Correction is a distinct appended entry referencing the appeal.
        let correction: AuditEntry = env
            .storage()
            .persistent()
            .get(&DataKey::AuditEntry(correction_id))
            .unwrap();
        assert_eq!(correction.action, symbol_short!("correct"));
        assert_eq!(correction.previous_hash, original.current_hash);

        // Chain remains valid with both records present.
        assert!(verify_audit_chain(&env));
        assert_eq!(get_audit_log_count(&env), 2);
    }
}
