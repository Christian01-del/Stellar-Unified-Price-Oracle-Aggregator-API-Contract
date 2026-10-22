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
//! ## Onboarding sandbox
//!
//! Before a source is approved it can run a non-production certification
//! pass with [`certify_source`]. The sandbox validates authentication,
//! asset identifiers, units (decimals), and timing against the live
//! registry and returns a [`SourceCertification`] report. It never writes
//! to production storage: the report is returned to the caller and no
//! mapping, price, or source record is mutated.

use soroban_sdk::{panic_with_error, Address, BytesN, Env, String, Vec};

use crate::events::{
    ForeignAssetMappedEvent, ForeignAssetMappingRemovedEvent, ForeignAssetMappingUpdatedEvent,
};
use crate::storage::{check_registered_asset, get_admin, LEDGER_BUMP, LEDGER_THRESHOLD};
use crate::types::{DataKey, ErrorCode, ForeignAssetMapping};

/// Maximum number of decimals a foreign asset may declare. Mirrors the
/// widest representation the oracle can rescale without overflow.
pub const MAX_FOREIGN_DECIMALS: u32 = 18;

/// Result of a sandbox certification run for a prospective source.
///
/// Every field is a diagnostic flag so a test source receives clear,
/// actionable feedback before approval. A source is certified only when
/// all flags are `true`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceCertification {
    /// The source's authentication was accepted (address is non-zero and
    /// the caller authorized the sandbox run).
    pub auth_ok: bool,
    /// The Stellar asset identifier is registered in the oracle.
    pub asset_ok: bool,
    /// The foreign asset identifier resolves to a known mapping.
    pub foreign_asset_ok: bool,
    /// The declared decimals are within the supported unit range.
    pub units_ok: bool,
    /// The declared observation timestamp is not in the future relative to
    /// the ledger close time.
    pub timing_ok: bool,
    /// Aggregate: `true` only when every individual check passed.
    pub certified: bool,
}

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

/// Runs a non-production certification pass for a prospective source.
///
/// The sandbox validates the source's authentication, asset identifiers,
/// units (decimals), and timing against the live registry and returns a
/// [`SourceCertification`] report. It performs **no** writes: no mapping,
/// price, or source record is created or mutated, so a test source cannot
/// write to production during certification.
///
/// # Arguments
/// * `source` — the prospective source address; must authorize the run.
/// * `stellar_asset` — the Stellar asset the source intends to report on.
/// * `chain` / `foreign_address` — the foreign identifier to resolve.
/// * `decimals` — the units the source will report in.
/// * `observed_at` — the source's observation timestamp (ledger seconds).
///
/// # Errors
///
/// * [`ErrorCode::NotAuthorized`] — `source` did not authorize the run.
/// * [`ErrorCode::InvalidString`] — `chain` is not a canonical identifier.
pub fn certify_source(
    env: &Env,
    source: Address,
    stellar_asset: Address,
    chain: String,
    foreign_address: BytesN<32>,
    decimals: u32,
    observed_at: u64,
) -> SourceCertification {
    // Authentication: the source must authorize the sandbox run. This is a
    // read-only check — no source record is persisted.
    source.require_auth();
    crate::input_validation::validate_string(env, "certify_source", "chain", &chain);

    let asset_ok = env
        .storage()
        .persistent()
        .has(&DataKey::AssetConfig(stellar_asset.clone()));

    let foreign_asset_ok = env
        .storage()
        .persistent()
        .has(&mapping_key(&chain, &foreign_address));

    let units_ok = decimals <= MAX_FOREIGN_DECIMALS;

    let now = env.ledger().timestamp();
    let timing_ok = observed_at <= now;

    let certified = asset_ok && foreign_asset_ok && units_ok && timing_ok;

    SourceCertification {
        auth_ok: true,
        asset_ok,
        foreign_asset_ok,
        units_ok,
        timing_ok,
        certified,
    }
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
        }
    }
    result
}
