use crate::storage::{get_admin, LEDGER_BUMP, LEDGER_THRESHOLD};
use crate::types::DataKey;
use soroban_sdk::{symbol_short, Address, Env};

/// Default global inactivity timeout: 0 = disabled.
const DEFAULT_INACTIVITY_TIMEOUT: u32 = 0;

/// Default per-source clock-skew threshold in seconds: 0 = disabled.
const DEFAULT_SKEW_THRESHOLD: u64 = 0;

// --- Configuration ---

/// Sets the global default inactivity timeout in ledgers (admin only).
/// 0 disables auto-deregistration globally.
pub fn set_inactivity_timeout(env: &Env, timeout_ledgers: u32) {
    let admin = get_admin(env);
    admin.require_auth();
    env.storage()
        .persistent()
        .set(&DataKey::CfgInactivityTimeout, &timeout_ledgers);
    env.storage().persistent().extend_ttl(
        &DataKey::CfgInactivityTimeout,
        LEDGER_THRESHOLD,
        LEDGER_BUMP,
    );
}

pub fn get_inactivity_timeout(env: &Env) -> u32 {
    env.storage()
        .persistent()
        .get(&DataKey::CfgInactivityTimeout)
        .unwrap_or(DEFAULT_INACTIVITY_TIMEOUT)
}

/// Sets per-asset inactivity timeout override in ledgers (admin only, 0 = use global).
pub fn set_asset_inactivity_timeout(env: &Env, asset: Address, timeout_ledgers: u32) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);
    let key = DataKey::AssetInactivityTimeout(asset.clone());
    if timeout_ledgers == 0 {
        env.storage().persistent().remove(&key);
    } else {
        env.storage().persistent().set(&key, &timeout_ledgers);
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
}

pub fn get_asset_inactivity_timeout(env: &Env, asset: &Address) -> u32 {
    let key = DataKey::AssetInactivityTimeout(asset.clone());
    let per_asset: Option<u32> = env.storage().persistent().get(&key);
    match per_asset {
        Some(t) if t > 0 => {
            env.storage()
                .persistent()
                .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
            t
        }
        _ => get_inactivity_timeout(env),
    }
}

// --- Activity tracking ---

/// Called from submit_price to record that an asset was active at this ledger.
pub fn record_asset_activity(env: &Env, asset: &Address) {
    let ledger = env.ledger().sequence();
    let key = DataKey::AssetLastActivity(asset.clone());
    env.storage().persistent().set(&key, &ledger);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
}

pub fn get_asset_last_activity(env: &Env, asset: &Address) -> Option<u32> {
    let key = DataKey::AssetLastActivity(asset.clone());
    let val: Option<u32> = env.storage().persistent().get(&key);
    if val.is_some() {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
    val
}

// --- Inactivity check and deregistration ---

/// Returns true if the asset is inactive (past its timeout threshold).
pub fn is_asset_inactive(env: &Env, asset: &Address) -> bool {
    let timeout = get_asset_inactivity_timeout(env, asset);
    if timeout == 0 {
        return false; // disabled
    }
    let last = match get_asset_last_activity(env, asset) {
        Some(l) => l,
        None => return false, // never had a submission — not tracked yet
    };
    let current = env.ledger().sequence();
    current.saturating_sub(last) >= timeout
}

/// Admin can trigger deregistration of an inactive asset.
/// Emits a warning event if within grace period (20% of timeout remaining),
/// and actually deregisters if past the full timeout.
pub fn check_and_deregister_if_inactive(env: &Env, asset: Address) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);

    let timeout = get_asset_inactivity_timeout(env, &asset);
    if timeout == 0 {
        // Auto-deregistration disabled for this asset
        return;
    }

    let last = match get_asset_last_activity(env, &asset) {
        Some(l) => l,
        None => return, // no activity recorded yet
    };

    let current = env.ledger().sequence();
    let idle_ledgers = current.saturating_sub(last);

    // Grace period = 20% of the timeout window
    let grace_start = timeout.saturating_mul(4) / 5;

    if idle_ledgers >= timeout {
        // Fully inactive — auto-deregister
        emit_asset_auto_deregistered(env, &asset, idle_ledgers);
        crate::assets::unregister_asset(env, asset);
    } else if idle_ledgers >= grace_start {
        // Within grace period — emit warning only
        emit_asset_inactivity_warning(env, &asset, idle_ledgers, timeout);
    }
}

// --- Clock-skew monitoring ---

/// Sets the global default clock-skew threshold in seconds (admin only).
/// 0 disables skew monitoring globally.
pub fn set_skew_threshold(env: &Env, threshold_secs: u64) {
    let admin = get_admin(env);
    admin.require_auth();
    env.storage()
        .persistent()
        .set(&DataKey::CfgSkewThreshold, &threshold_secs);
    env.storage().persistent().extend_ttl(
        &DataKey::CfgSkewThreshold,
        LEDGER_THRESHOLD,
        LEDGER_BUMP,
    );
}

pub fn get_skew_threshold(env: &Env) -> u64 {
    env.storage()
        .persistent()
        .get(&DataKey::CfgSkewThreshold)
        .unwrap_or(DEFAULT_SKEW_THRESHOLD)
}

/// Sets per-source clock-skew threshold override in seconds (admin only, 0 = use global).
pub fn set_source_skew_threshold(env: &Env, source: Address, threshold_secs: u64) {
    let admin = get_admin(env);
    admin.require_auth();
    let key = DataKey::SourceSkewThreshold(source.clone());
    if threshold_secs == 0 {
        env.storage().persistent().remove(&key);
    } else {
        env.storage().persistent().set(&key, &threshold_secs);
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
}

pub fn get_source_skew_threshold(env: &Env, source: &Address) -> u64 {
    let key = DataKey::SourceSkewThreshold(source.clone());
    let per_source: Option<u64> = env.storage().persistent().get(&key);
    match per_source {
        Some(t) if t > 0 => {
            env.storage()
                .persistent()
                .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
            t
        }
        _ => get_skew_threshold(env),
    }
}

/// Measures a source's submission timestamp skew against both the ledger
/// timestamp and the observed submission time, emitting a source-scoped alert
/// when either exceeds the configured per-source threshold.
///
/// Returns the maximum observed skew in seconds (0 when monitoring disabled).
/// Skew alerts are emitted independently of other source health signals so
/// they never suppress inactivity or other health events.
pub fn check_source_skew(
    env: &Env,
    source: &Address,
    source_timestamp: u64,
    observed_submission_time: u64,
) -> u64 {
    let threshold = get_source_skew_threshold(env, source);
    if threshold == 0 {
        return 0; // monitoring disabled for this source
    }

    let ledger_time = env.ledger().timestamp();
    let ledger_skew = abs_diff(source_timestamp, ledger_time);
    let submission_skew = abs_diff(source_timestamp, observed_submission_time);
    let max_skew = if ledger_skew > submission_skew {
        ledger_skew
    } else {
        submission_skew
    };

    if max_skew > threshold {
        emit_source_skew_alert(
            env,
            source,
            ledger_skew,
            submission_skew,
            threshold,
        );
    }

    max_skew
}

fn abs_diff(a: u64, b: u64) -> u64 {
    if a >= b {
        a - b
    } else {
        b - a
    }
}

// --- Events ---

fn emit_asset_inactivity_warning(env: &Env, asset: &Address, idle_ledgers: u32, timeout: u32) {
    env.events().publish(
        (symbol_short!("inact_wrn"), asset.clone()),
        (idle_ledgers, timeout),
    );
}

fn emit_asset_auto_deregistered(env: &Env, asset: &Address, idle_ledgers: u32) {
    env.events()
        .publish((symbol_short!("auto_dreg"), asset.clone()), (idle_ledgers,));
}

fn emit_source_skew_alert(
    env: &Env,
    source: &Address,
    ledger_skew: u64,
    submission_skew: u64,
    threshold: u64,
) {
    env.events().publish(
        (symbol_short!("skew_alrt"), source.clone()),
        (ledger_skew, submission_skew, threshold),
    );
}
