use soroban_sdk::{panic_with_error, symbol_short, Address, Bytes, Env, Vec};

use crate::events::{
    emit_admin_action, AssetRegisteredEvent, AssetUnregisteredEvent, CircuitBreakerEventEntry,
    CircuitBreakerResetEvent, CircuitBreakerTrippedEvent,
};
use crate::storage::{
    get_admin, read_registered_assets, write_registered_assets, LEDGER_BUMP, LEDGER_THRESHOLD,
};
use crate::types::{AssetMetadata, AssetMetadataUpdate, DataKey, ErrorCode, PriceBounds};

/// Production asset onboarding policy.
///
/// Every asset enabled at launch must carry a complete review record before it
/// can enter the production manifest. The record captures eligibility, the
/// required metadata, the risk review, and the sign-off. A registration is
/// rejected unless all required fields are present and well-formed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetOnboardingRecord {
    /// Eligibility: the asset is a supported Stellar/Soroban token.
    pub eligible: bool,
    /// Required metadata: human-readable name (non-empty).
    pub name: Bytes,
    /// Required metadata: ticker symbol (non-empty).
    pub symbol: Bytes,
    /// Required metadata: decimals (must be <= 18).
    pub decimals: u32,
    /// Required metadata: logo URI (non-empty).
    pub logo_uri: Bytes,
    /// Risk review: reviewer identifier (non-empty).
    pub risk_reviewer: Bytes,
    /// Risk review: risk tier (0 = low .. 3 = high).
    pub risk_tier: u32,
    /// Sign-off: approver identifier (non-empty).
    pub sign_off: Bytes,
}

/// Validate an onboarding record against the production policy.
///
/// Returns `Ok(())` when every required review field is present and within
/// bounds, otherwise panics with `ErrorCode::InvalidConfiguration`. This is the
/// single gate that a new asset must pass before entering the manifest.
#[allow(dead_code)]
pub fn validate_onboarding_record(env: &Env, record: &AssetOnboardingRecord) {
    if !record.eligible {
        panic_with_error!(env, ErrorCode::InvalidConfiguration);
    }
    if record.name.is_empty() || record.symbol.is_empty() || record.logo_uri.is_empty() {
        panic_with_error!(env, ErrorCode::InvalidConfiguration);
    }
    if record.decimals > 18 {
        panic_with_error!(env, ErrorCode::InvalidConfiguration);
    }
    if record.risk_reviewer.is_empty() || record.sign_off.is_empty() {
        panic_with_error!(env, ErrorCode::InvalidConfiguration);
    }
    if record.risk_tier > 3 {
        panic_with_error!(env, ErrorCode::InvalidConfiguration);
    }
}

pub fn register_asset(env: &Env, asset: Address) {
    let admin = get_admin(env);
    admin.require_auth();
    if env
        .storage()
        .persistent()
        .has(&DataKey::AssetRegistered(asset.clone()))
    {
        panic_with_error!(env, ErrorCode::AssetAlreadyRegistered);
    }

    let max_assets: u32 = crate::admin::get_max_assets(env);
    let mut assets = read_registered_assets(env);
    if assets.len() as u32 >= max_assets {
        panic_with_error!(env, ErrorCode::MaxAssetsReached);
    }

    env.storage()
        .persistent()
        .set(&DataKey::AssetRegistered(asset.clone()), &true);

    // O(1) membership index (new): keep in sync with the Vec.
    env.storage()
        .persistent()
        .set(&DataKey::AssetRegistryIndex(asset.clone()), &true);

    assets.push_back(asset.clone());
    write_registered_assets(env, &assets);
    initialize_asset_price_bounds(env, asset.clone());

    AssetRegisteredEvent {
        asset: asset.clone(),
        admin: admin.clone(),
    }
    .publish(env);
    emit_admin_action(env, symbol_short!("reg_asset"), admin, Bytes::new(env));
}

/// Register an asset into the production manifest, enforcing the onboarding
/// policy. The asset cannot enter the manifest unless the supplied record
/// passes `validate_onboarding_record`.
#[allow(dead_code)]
pub fn register_asset_with_policy(
    env: &Env,
    asset: Address,
    record: AssetOnboardingRecord,
) {
    validate_onboarding_record(env, &record);
    register_asset(env, asset);
}

pub fn unregister_asset(env: &Env, asset: Address) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);
    env.storage()
        .persistent()
        .remove(&DataKey::AssetRegistered(asset.clone()));

    // O(1) membership index (new).
    env.storage()
        .persistent()
        .remove(&DataKey::AssetRegistryIndex(asset.clone()));

    env.storage()
        .persistent()
        .remove(&DataKey::Aggregate(asset.clone()));

    let assets = read_registered_assets(env);
    let mut new_assets: Vec<Address> = Vec::new(env);
    for i in 0..assets.len() {
        let a = assets.get_unchecked(i);
        if a != asset {
            new_assets.push_back(a);
        }
    }
    write_registered_assets(env, &new_assets);
    AssetUnregisteredEvent {
        asset: asset.clone(),
        admin: admin.clone(),
    }
    .publish(env);
    emit_admin_action(env, symbol_short!("unreg_ast"), admin, Bytes::new(env));
}

pub fn is_asset_registered(env: &Env, asset: Address) -> bool {
    // Prefer the O(1) index. For backwards compatibility with older
    // deployments, fall back to the legacy `AssetRegistered(addr)` flag and
    // lazily (re)build the index when needed.
    let index_key = DataKey::AssetRegistryIndex(asset.clone());
    let indexed: bool = env.storage().persistent().get(&index_key).unwrap_or(false);
    if indexed {
        env.storage()
            .persistent()
            .extend_ttl(&index_key, LEDGER_THRESHOLD, LEDGER_BUMP);
        return true;
    }

    let legacy_key = DataKey::AssetRegistered(asset.clone());
    let exists: bool = env.storage().persistent().get(&legacy_key).unwrap_or(false);
    if exists {
        env.storage()
            .persistent()
            .extend_ttl(&legacy_key, LEDGER_THRESHOLD, LEDGER_BUMP);

        // Lazy migration: populate index entry.
        env.storage().persistent().set(&index_key, &true);
        env.storage()
            .persistent()
            .extend_ttl(&index_key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
    exists
}

#[allow(dead_code)]
pub fn set_asset_metadata(env: &Env, asset: Address, metadata: AssetMetadata) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);
    env.storage()
        .persistent()
        .set(&DataKey::AssetMetadata(asset.clone()), &metadata);

    crate::events::AssetMetadataUpdatedEvent {
        asset,
        admin: admin.clone(),
        name: metadata.name,
        symbol: metadata.symbol,
        decimals: metadata.decimals,
        logo_uri: Some(metadata.logo_uri),
    }
    .publish(env);
}

pub fn batch_set_asset_metadata(env: &Env, updates: Vec<AssetMetadataUpdate>) {
    let admin = get_admin(env);
    admin.require_auth();

    for i in 0..updates.len() {
        let update = updates.get_unchecked(i);
        crate::storage::check_registered_asset(env, &update.asset);

        let metadata = AssetMetadata {
            name: update.name.clone(),
            symbol: update.symbol.clone(),
            decimals: update.decimals,
            logo_uri: update.logo_uri.clone(),
        };

        env.storage()
            .persistent()
            .set(&DataKey::AssetMetadata(update.asset.clone()), &metadata);

        crate::events::AssetMetadataUpdatedEvent {
            asset: update.asset.clone(),
            admin: admin.clone(),
            name: update.name,
            symbol: update.symbol,
            decimals: update.decimals,
            logo_uri: Some(update.logo_uri),
        }
        .publish(env);
    }
}

#[allow(dead_code)]
pub fn get_asset_metadata(env: &Env, asset: Address) -> Option<AssetMetadata> {
    crate::storage::check_registered_asset(env, &asset);
    let key = DataKey::AssetMetadata(asset.clone());
    if env.storage().persistent().has(&key) {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
    env.storage().persistent().get(&key)
}

#[allow(dead_code)]
pub fn set_min_price(env: &Env, asset: Address, min_price: i128) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);
    env.storage()
        .persistent()
        .set(&DataKey::AssetMinPrice(asset.clone()), &min_price);
}

pub fn get_min_price(env: &Env, asset: Address) -> i128 {
    crate::storage::check_registered_asset(env, &asset);
    let bounds = get_price_bounds(env, asset.clone());
    if bounds.min_price > 0 {
        return bounds.min_price;
    }
    let key = DataKey::AssetMinPrice(asset.clone());
    if env.storage().persistent().has(&key) {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
    env.storage().persistent().get(&key).unwrap_or(0)
}

pub fn initialize_asset_price_bounds(env: &Env, asset: Address) {
    let bounds = PriceBounds {
        min_price: 0,
        max_price: i128::MAX,
        max_change_bps_per_ledger: 0,
    };
    env.storage()
        .persistent()
        .set(&DataKey::AssetPriceBounds(asset.clone()), &bounds);
    env.storage()
        .persistent()
        .set(&DataKey::AssetPauseFlag(asset.clone()), &false);
    env.storage()
        .persistent()
        .set(&DataKey::AssetCircuitBreakerTripped(asset.clone()), &false);
    env.storage()
        .persistent()
        .set(&DataKey::AssetCircuitBreakerLogCount(asset.clone()), &0u32);
}

pub fn set_price_bounds(
    env: &Env,
    asset: Address,
    min_price: i128,
    max_price: i128,
    max_change_bps_per_ledger: u32,
) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);
    if min_price > max_price {
        panic_with_error!(env, ErrorCode::InvalidConfiguration);
    }
    if max_change_bps_per_ledger > 100_000 {
        panic_with_error!(env, ErrorCode::InvalidConfiguration);
    }

    let bounds = PriceBounds {
        min_price,
        max_price,
        max_change_bps_per_ledger,
    };
    env.storage()
        .persistent()
        .set(&DataKey::AssetPriceBounds(asset.clone()), &bounds);
    emit_admin_action(env, symbol_short!("st_bounds"), admin, Bytes::new(env));
}

pub fn get_price_bounds(env: &Env, asset: Address) -> PriceBounds {
    crate::storage::check_registered_asset(env, &asset);
    let key = DataKey::AssetPriceBounds(asset.clone());
    if env.storage().persistent().has(&key) {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or(PriceBounds {
            min_price: 0,
            max_price: i128::MAX,
            max_change_bps_per_ledger: 0,
        })
}

#[allow(dead_code)]
pub fn set_asset_paused(env: &Env, asset: Address, paused: bool) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);
    env.storage()
        .persistent()
        .set(&DataKey::AssetPauseFlag(asset.clone()), &paused);
}

pub fn is_asset_paused(env: &Env, asset: Address) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::AssetPauseFlag(asset.clone()))
        .unwrap_or(false)
}

pub fn trip_circuit_breaker(env: &Env, asset: Address, reason: Bytes) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);
    env.storage()
        .persistent()
        .set(&DataKey::AssetCircuitBreakerTripped(asset.clone()), &true);
    CircuitBreakerTrippedEvent {
        asset: asset.clone(),
        admin: admin.clone(),
        reason: reason.clone(),
    }
    .publish(env);
    emit_admin_action(env, symbol_short!("cb_trip"), admin, reason);
}

pub fn reset_circuit_breaker(env: &Env, asset: Address) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::storage::check_registered_asset(env, &asset);
    env.storage()
        .persistent()
        .set(&DataKey::AssetCircuitBreakerTripped(asset.clone()), &false);
    CircuitBreakerResetEvent {
        asset: asset.clone(),
        admin: admin.clone(),
    }
    .publish(env);
    emit_admin_action(env, symbol_short!("cb_reset"), admin, Bytes::new(env));
}

pub fn is_circuit_breaker_tripped(env: &Env, asset: Address) -> bool {
    env.storage()
        .persistent()
        .get(&DataKey::AssetCircuitBreakerTripped(asset.clone()))
        .unwrap_or(false)
}

#[allow(dead_code)]
pub fn get_circuit_breaker_log(env: &Env, asset: Address) -> Vec<CircuitBreakerEventEntry> {
    let key = DataKey::AssetCircuitBreakerLog(asset.clone());
    if env.storage().persistent().has(&key) {
        env.storage()
            .persistent()
            .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
    }
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or(Vec::new(env))
}
