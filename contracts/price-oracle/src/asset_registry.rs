//! # Canonical Cross-Chain Asset Registry
//!
//! A single source of truth mapping a Stellar asset address to its
//! representation(s) on foreign chains (contract/token address, decimals).
//! Cross-chain modules — Axelar GMP ([`crate::axelar_gmp`]), LayerZero
//! ([`crate::layerzero`]), and manual cross-reference checks
//! ([`crate::cross_chain_verify`]) — resolve foreign asset identifiers
//! through this registry instead of maintaining their own ad-hoc mappings.
//!
//! ## Schema
//!
//! A mapping is keyed by `(chain, foreign_address)`:
//! * `chain` — a short canonical chain identifier (e.g. `"ethereum"`,
//!   `"polygon"`). Bridge integrations that identify chains differently
//!   (LayerZero's numeric `src_eid`, for example) translate their own
//!   identifier onto this canonical namespace rather than inventing a
//!   parallel one — see [`crate::layerzero::set_lz_chain_name`].
//! * `foreign_address` — the 32-byte canonical representation of the
//!   asset's contract/token address on that chain (left-padded for
//!   shorter addresses such as 20-byte EVM addresses).
//!
//! Each mapping also carries the foreign asset's `decimals` so callers can
//! rescale a price without a second lookup, and an `enabled` flag so admins
//! can suspend a mapping without losing its history.
//!
//! ## Dry-run previews
//!
//! The mutating entry points below have `*_preview` counterparts that compute
//! the exact proposed change (before/after values) without writing storage or
//! emitting events. They are pure reads: they cannot submit a transaction.

use soroban_sdk::{panic_with_error, Address, BytesN, Env, String, Vec};

use crate::events::{
    ForeignAssetMappedEvent, ForeignAssetMappingRemovedEvent, ForeignAssetMappingUpdatedEvent,
};
use crate::storage::{check_registered_asset, get_admin, LEDGER_BUMP, LEDGER_THRESHOLD};
use crate::types::{DataKey, ErrorCode, ForeignAssetMapping};

fn mapping_key(chain: &String, foreign_address: &BytesN<32>) -> DataKey {
    DataKey::ForeignAssetMapping(chain.clone(), foreign_address.clone())
}

fn append_index(env: &Env, key: DataKey, entry: (String, BytesN<32>)) {
    let mut list: Vec<(String, BytesN<32>)> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(Vec::new(env));
    list.push_back(entry);
    env.storage().persistent().set(&key, &list);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);
}

fn remove_from_index(env: &Env, key: DataKey, entry: &(String, BytesN<32>)) {
    let list: Vec<(String, BytesN<32>)> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(Vec::new(env));
    let mut updated: Vec<(String, BytesN<32>)> = Vec::new(env);
    for item in list.iter() {
        if item != *entry {
            updated.push_back(item);
        }
    }
    env.storage().persistent().set(&key, &updated);
}

/// Non-mutating preview of [`register_foreign_asset_mapping`].
///
/// Returns the mapping that *would* be created. Performs the same validation
/// as the mutating path (admin auth, chain name, registered asset, duplicate
/// check) but never writes storage or emits events, so it cannot submit a
/// transaction.
pub fn preview_register_foreign_asset_mapping(
    env: &Env,
    stellar_asset: Address,
    chain: String,
    foreign_address: BytesN<32>,
    decimals: u32,
) -> ForeignAssetMapping {
    let admin = get_admin(env);
    admin.require_auth();
    crate::input_validation::validate_string(
        env,
        "preview_register_foreign_asset_mapping",
        "chain",
        &chain,
    );
    check_registered_asset(env, &stellar_asset);

    let key = mapping_key(&chain, &foreign_address);
    if env.storage().persistent().has(&key) {
        panic_with_error!(env, ErrorCode::ForeignAssetAlreadyMapped);
    }

    ForeignAssetMapping {
        stellar_asset,
        chain,
        foreign_address,
        decimals,
        enabled: true,
    }
}

/// Non-mutating preview of [`update_foreign_asset_mapping`].
///
/// Returns `(before, after)` so operators can review the exact proposed
/// change. Never writes storage or emits events.
pub fn preview_update_foreign_asset_mapping(
    env: &Env,
    chain: String,
    foreign_address: BytesN<32>,
    decimals: u32,
    enabled: bool,
) -> (ForeignAssetMapping, ForeignAssetMapping) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::input_validation::validate_string(
        env,
        "preview_update_foreign_asset_mapping",
        "chain",
        &chain,
    );

    let key = mapping_key(&chain, &foreign_address);
    let before: ForeignAssetMapping = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| panic_with_error!(env, ErrorCode::ForeignAssetNotMapped));

    let mut after = before.clone();
    after.decimals = decimals;
    after.enabled = enabled;
    (before, after)
}

/// Non-mutating preview of [`remove_foreign_asset_mapping`].
///
/// Returns the mapping that *would* be removed. Never writes storage or emits
/// events.
pub fn preview_remove_foreign_asset_mapping(
    env: &Env,
    chain: String,
    foreign_address: BytesN<32>,
) -> ForeignAssetMapping {
    let admin = get_admin(env);
    admin.require_auth();
    crate::input_validation::validate_string(
        env,
        "preview_remove_foreign_asset_mapping",
        "chain",
        &chain,
    );

    let key = mapping_key(&chain, &foreign_address);
    env.storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| panic_with_error!(env, ErrorCode::ForeignAssetNotMapped))
}

/// Registers a new canonical mapping from `stellar_asset` to its representation
/// on `chain`. Admin-only.
///
/// # Errors
///
/// * [`ErrorCode::NotAuthorized`] — caller is not the admin.
/// * [`ErrorCode::AssetNotRegistered`] — `stellar_asset` is not a registered asset.
/// * [`ErrorCode::ForeignAssetAlreadyMapped`] — a mapping already exists for
///   `(chain, foreign_address)`.
pub fn register_foreign_asset_mapping(
    env: &Env,
    stellar_asset: Address,
    chain: String,
    foreign_address: BytesN<32>,
    decimals: u32,
) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::input_validation::validate_string(
        env,
        "register_foreign_asset_mapping",
        "chain",
        &chain,
    );
    check_registered_asset(env, &stellar_asset);

    let key = mapping_key(&chain, &foreign_address);
    if env.storage().persistent().has(&key) {
        panic_with_error!(env, ErrorCode::ForeignAssetAlreadyMapped);
    }

    let mapping = ForeignAssetMapping {
        stellar_asset: stellar_asset.clone(),
        chain: chain.clone(),
        foreign_address: foreign_address.clone(),
        decimals,
        enabled: true,
    };
    env.storage().persistent().set(&key, &mapping);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);

    append_index(
        env,
        DataKey::ForeignAssetRegistryList,
        (chain.clone(), foreign_address.clone()),
    );
    append_index(
        env,
        DataKey::AssetForeignMappings(stellar_asset.clone()),
        (chain.clone(), foreign_address.clone()),
    );

    ForeignAssetMappedEvent {
        stellar_asset,
        chain,
        foreign_address,
        decimals,
    }
    .publish(env);
}

/// Updates an existing mapping's `decimals` and `enabled` flag. Admin-only.
///
/// # Errors
///
/// * [`ErrorCode::NotAuthorized`] — caller is not the admin.
/// * [`ErrorCode::ForeignAssetNotMapped`] — no mapping exists for `(chain, foreign_address)`.
pub fn update_foreign_asset_mapping(
    env: &Env,
    chain: String,
    foreign_address: BytesN<32>,
    decimals: u32,
    enabled: bool,
) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::input_validation::validate_string(env, "update_foreign_asset_mapping", "chain", &chain);

    let key = mapping_key(&chain, &foreign_address);
    let mut mapping: ForeignAssetMapping = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| panic_with_error!(env, ErrorCode::ForeignAssetNotMapped));

    mapping.decimals = decimals;
    mapping.enabled = enabled;
    env.storage().persistent().set(&key, &mapping);
    env.storage()
        .persistent()
        .extend_ttl(&key, LEDGER_THRESHOLD, LEDGER_BUMP);

    ForeignAssetMappingUpdatedEvent {
        stellar_asset: mapping.stellar_asset,
        chain,
        foreign_address,
        decimals,
        enabled,
    }
    .publish(env);
}

/// Permanently removes a foreign asset mapping. Admin-only.
///
/// # Errors
///
/// * [`ErrorCode::NotAuthorized`] — caller is not the admin.
/// * [`ErrorCode::ForeignAssetNotMapped`] — no mapping exists for `(chain, foreign_address)`.
pub fn remove_foreign_asset_mapping(env: &Env, chain: String, foreign_address: BytesN<32>) {
    let admin = get_admin(env);
    admin.require_auth();
    crate::input_validation::validate_string(env, "remove_foreign_asset_mapping", "chain", &chain);

    let key = mapping_key(&chain, &foreign_address);
    let mapping: ForeignAssetMapping = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| panic_with_error!(env, ErrorCode::ForeignAssetNotMapped));

    env.storage().persistent().remove(&key);

    let entry = (chain.clone(), foreign_address.clone());
    remove_from_index(env, DataKey::ForeignAssetRegistryList, &entry);
    remove_from_index(
        env,
        DataKey::AssetForeignMappings(mapping.stellar_asset.clone()),
        &entry,
    );

    ForeignAssetMappingRemovedEvent {
        stellar_asset: mapping.stellar_asset,
        chain,
        foreign_address,
    }
    .publish(env);
}

/// Returns the mapping for `(chain, foreign_address)`, if any — regardless of
/// its `enabled` state.
pub fn get_foreign_asset_mapping(
    env: &Env,
    chain: String,
    foreign_address: BytesN<32>,
) -> Option<ForeignAssetMapping> {
    // A lookup key is still untrusted input: a canonical chain name means an
    // attacker cannot probe for near-miss keys, and an oversized name is
    // rejected instead of being hashed into a storage key.
    crate::input_validation::validate_string(env, "get_foreign_asset_mapping", "chain", &chain);
    env.storage()
        .persistent()
        .get(&mapping_key(&chain, &foreign_address))
}

/// Returns every foreign-chain mapping currently registered for `asset`.
pub fn get_foreign_mappings_for_asset(env: &Env, asset: Address) -> Vec<ForeignAssetMapping> {
    let keys: Vec<(String, BytesN<32>)> = env
        .storage()
        .persistent()
        .get(&DataKey::AssetForeignMappings(asset))
        .unwrap_or(Vec::new(env));

    let mut result = Vec::new(env);
    for (chain, foreign_address) in keys.iter() {
        if let Some(mapping) = get_foreign_asset_mapping(env, chain, foreign_address) {
            result.push_back(mapping);

/* … truncated 6234 chars — edit only what you need near the top … */
