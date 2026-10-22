//! Price oracle contract.
//!
//! Provides price feeds, source reward accounting, and an appeal/correction
//! process for disputed reward measurements. Corrections are appended as new
//! records so the original audit trail is never overwritten.
//!
//! Fee schedule disclosure and consumer impact review (issue #675):
//! the fee assumptions and the reproducible consumer-impact method are
//! published in `docs/fee-schedule.md` and `docs/fee-schedule-review.md`.
//! The pure calculation below mirrors that method so the review is a
//! repeatable check rather than a one-off spreadsheet.

#![no_std]

use soroban_sdk::{contract, contractimpl, contracttype, Address, Env, Symbol, Vec};

/// A single recorded reward measurement for a source in an epoch.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewardRecord {
    pub source: Address,
    pub epoch: u64,
    pub amount: i128,
    pub recorded_at: u64,
}

/// An appeal filed by a source disputing a measured contribution or payout.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Appeal {
    pub id: u64,
    pub source: Address,
    pub epoch: u64,
    pub disputed_amount: i128,
    pub reason: Symbol,
    pub filed_at: u64,
    pub resolved: bool,
}

/// A correction appended when an appeal is approved. It references the
/// original record and the appeal that produced it, preserving the audit trail.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Correction {
    pub appeal_id: u64,
    pub source: Address,
    pub epoch: u64,
    pub original_amount: i128,
    pub corrected_amount: i128,
    pub approved_by: Address,
    pub recorded_at: u64,
}

/// Published fee assumptions for a schedule review (issue #675).
///
/// All amounts are in stroops (1 XLM = 10_000_000 stroops). The schedule is
/// disclosed in `docs/fee-schedule.md`; this struct is the machine-readable
/// form used by the reproducible consumer-impact calculation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeSchedule {
    /// Flat fee charged per submitted update, in stroops.
    pub base_fee: i128,
    /// Additional fee per asset covered by an update, in stroops.
    pub per_asset_fee: i128,
    /// Additional fee per update beyond the first in a batch, in stroops.
    pub per_update_fee: i128,
    /// Storage/rent fee per asset per epoch, in stroops.
    pub storage_fee_per_asset: i128,
    /// Volume tier threshold in updates; at or above it the tier discount
    /// applies. Zero disables tiering.
    pub tier_threshold: u64,
    /// Discount applied to the variable fees at/above the tier threshold,
    /// expressed in basis points (e.g. 1000 = 10%).
    pub tier_discount_bps: u32,
}

/// Result of the consumer-impact calculation for one integration profile.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FeeImpact {
    /// Total fee for the profile, in stroops.
    pub total_fee: i128,
    /// Fee as a fraction of gross value, in basis points.
    pub effective_bps: u32,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    NextAppealId,
    Appeal(u64),
    Corrections(u64),
}

#[contract]
pub struct PriceOracle;

#[contractimpl]
impl PriceOracle {
    /// Initialize the contract with an authorized reviewer/admin.
    pub fn initialize(env: Env, admin: Address) {
        if env.storage().instance().has(&DataKey::Admin) {
            panic!("already initialized");
        }
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::NextAppealId, &0u64);
    }

    /// A source disputes a measured contribution or payout for a given epoch.
    /// Returns the new appeal id.
    pub fn file_appeal(
        env: Env,
        source: Address,
        epoch: u64,
        disputed_amount: i128,
        reason: Symbol,
    ) -> u64 {
        source.require_auth();

        let id: u64 = env
            .storage()
            .instance()
            .get(&DataKey::NextAppealId)
            .unwrap_or(0);

        let appeal = Appeal {
            id,
            source: source.clone(),
            epoch,
            disputed_amount,
            reason,
            filed_at: env.ledger().timestamp(),
            resolved: false,
        };

        env.storage().persistent().set(&DataKey::Appeal(id), &appeal);
        env.storage()
            .instance()
            .set(&DataKey::NextAppealId, &(id + 1));

        id
    }

    /// An authorized reviewer approves an appeal and records a correction.
    /// The original reward record is left untouched; the correction is appended
    /// and linked to the appeal for a complete audit trail.
    pub fn approve_appeal(
        env: Env,
        reviewer: Address,
        appeal_id: u64,
        original_amount: i128,
        corrected_amount: i128,
    ) {
        reviewer.require_auth();

        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        if reviewer != admin {
            panic!("unauthorized reviewer");
        }

        let mut appeal: Appeal = env
            .storage()
            .persistent()
            .get(&DataKey::Appeal(appeal_id))
            .expect("appeal not found");
        if appeal.resolved {
            panic!("appeal already resolved");
        }

        let correction = Correction {
            appeal_id,
            source: appeal.source.clone(),
            epoch: appeal.epoch,
            original_amount,
            corrected_amount,
            approved_by: reviewer,
            recorded_at: env.ledger().timestamp(),
        };

        let mut corrections: Vec<Correction> = env
            .storage()
            .persistent()
            .get(&DataKey::Corrections(appeal_id))
            .unwrap_or(Vec::new(&env));
        corrections.push_back(correction);
        env.storage()
            .persistent()
            .set(&DataKey::Corrections(appeal_id), &corrections);

        appeal.resolved = true;
        env.storage()
            .persistent()
            .set(&DataKey::Appeal(appeal_id), &appeal);
    }

    /// Fetch an appeal by id.
    pub fn get_appeal(env: Env, appeal_id: u64) -> Appeal {
        env.storage()
            .persistent()
            .get(&DataKey::Appeal(appeal_id))
            .expect("appeal not found")
    }

    /// Fetch the correction records appended for an appeal.
    pub fn get_corrections(env: Env, appeal_id: u64) -> Vec<Correction> {
        env.storage()
            .persistent()
            .get(&DataKey::Corrections(appeal_id))
            .unwrap_or(Vec::new(&env))
    }

    /// Reproducible consumer-impact calculation for a fee schedule review.
    ///
    /// Given a schedule, the number of assets and updates an integration
    /// submits per epoch, and its gross value in stroops, returns the total
    /// fee and the effective fee in basis points. This is the exact method
    /// documented in `docs/fee-schedule.md`; the review artifact records the
    /// inputs and the resulting numbers so they can be re-derived.
    ///
    /// Boundary behavior:
    /// - zero updates or zero assets yields a zero fee (no activity, no charge);
    /// - tiering only applies when `tier_threshold > 0` and updates reach it;
    /// - a zero gross value reports `effective_bps == 0` to avoid division by zero.
    pub fn compute_fee_impact(
        schedule: FeeSchedule,
        assets: u64,
        updates: u64,
        gross_value: i128,
    ) -> FeeImpact {
        if assets == 0 || updates == 0 {
            return FeeImpact {
                total_fee: 0,
                effective_bps: 0,
            };
        }

        let assets_i = assets as i128;
        let updates_i = updates as i128;

        // Variable fees: per-asset and per-update (beyond the first update).
        let per_asset_total = schedule.per_asset_fee * assets_i;
        let extra_updates = if updates > 1 { updates_i - 1 } else { 0 };
        let per_update_total = schedule.per_update_fee * extra_updates;
        let storage_total = schedule.storage_fee_per_asset * assets_i;

        let mut variable = per_asset_total + per_update_total + storage_total;

        // Volume tier discount on variable fees only; base fee is never discounted.
        if schedule.tier_threshold > 0 && updates >= schedule.tier_threshold {
            let bps = schedule.tier_discount_bps as i128;
            variable -= variable * bps / 10_000;
        }

        let total_fee = schedule.base_fee + variable;

        let effective_bps = if gross_value > 0 {
            let bps = total_fee * 10_000 / gross_value;
            if bps > u32::MAX as i128 {
                u32::MAX
            } else {
                bps as u32
            }
        } else {
            0
        };

        FeeImpact {
            total_fee,
            effective_bps,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::{Env, Symbol};

    #[test]
    fn resolves_dispute_without_overwriting_audit_record() {
        let env = Env::default();
        env.mock_all_auths();

        let contract_id = env.register_contract(None, PriceOracle);
        let client = PriceOracleClient::new(&env, &contract_id);

        let admin = Address::generate(&env);
        let source = Address::generate(&env);
        client.initialize(&admin);

        let appeal_id = client.file_appeal(
            &source,
            &7u64,
            &100i128,
            &Symbol::new(&env, "underpaid"),
        );

        let original = client.get_appeal(&appeal_id);
        assert_eq!(original.disputed_amount, 100);
        assert!(!original.resolved);

        client.approve_appeal(&admin, &appeal_id, &100i128, &150i128);

        // Original appeal record remains intact and is marked resolved.
        let after = client.get_appeal(&appeal_id);
        assert_eq!(after.disputed_amount, 100);
        assert_eq!(after.epoch, 7);
        assert!(after.resolved);

        // Correction is appended alongside the original audit record.
        let corrections = client.get_corrections(&appeal_id);
        assert_eq!(corrections.len(), 1);
        let c = corrections.get(0).unwrap();
        assert_eq!(c.original_amount, 100);
        assert_eq!(c.corrected_amount, 150);
        assert_eq!(c.approved_by, admin);
    }

    fn sample_schedule() -> FeeSchedule {
        // Matches the published assumptions in docs/fee-schedule.md.
        FeeSchedule {
            base_fee: 100_000,
            per_asset_fee: 10_000,
            per_update_fee: 5_000,
            storage_fee_per_asset: 2_000,
            tier_threshold: 100,
            tier_discount_bps: 1_000,
        }
    }

    #[test]
    fn fee_impact_matches_documented_worked_example() {
        // Worked example from docs/fee-schedule-review.md:
        // 3 assets, 10 updates, gross 50_000_000 stroops (5 XLM).
        // variable = 10_000*3 + 5_000*9 + 2_000*3 = 30_000 + 45_000 + 6_000 = 81_000
        // below tier threshold (10 < 100), no discount.
        // total = 100_000 + 81_000 = 181_000 stroops.
        // effective_bps = 181_000 * 10_000 / 50_000_000 = 36 bps.
        let impact = PriceOracle::compute_fee_impact(sample_schedule(), 3, 10, 50_000_000);
        assert_eq!(impact.total_fee, 181_000);
        assert_eq!(impact.effective_bps, 36);
    }

    #[test]
    fn fee_impact_applies_tier_discount_at_threshold() {
        // 1 asset, 100 updates (at threshold), gross 1_000_000_000 stroops.
        // variable = 10_000 + 5_000*99 + 2_000 = 507_000
        // discount 10% -> 456_300; total = 100_000 + 456_300 = 556_300.
        let impact = PriceOracle::compute_fee_impact(sample_schedule(), 1, 100, 1_000_000_000);
        assert_eq!(impact.total_fee, 556_300);
    }

    #[test]
    fn fee_impact_is_zero_without_activity() {
        let zero_updates = PriceOracle::compute_fee_impact(sample_schedule(), 5, 0, 1_000_000);
        assert_eq!(zero_updates.total_fee, 0);
        assert_eq!(zero_updates.effective_bps, 0);

        let zero_assets = PriceOracle::compute_fee_impact(sample_schedule(), 0, 5, 1_000_000);
        assert_eq!(zero_assets.total_fee, 0);
        assert_eq!(zero_assets.effective_bps, 0);
    }

    #[test]
    fn fee_impact_handles_zero_gross_value() {
        let impact = PriceOracle::compute_fee_impact(sample_schedule(), 1, 1, 0);
        assert_eq!(impact.total_fee, 100_000);
        assert_eq!(impact.effective_bps, 0);
    }

    #[test]
    fn small_integration_cost_is_bounded() {
        // Smallest active integration: 1 asset, 1 update, gross 10_000_000 (1 XLM).
        // total = base 100_000 + per_asset 10_000 + storage 2_000 = 112_000.
        // effective_bps = 112_000 * 10_000 / 10_000_000 = 112 bps.
        let impact = PriceOracle::compute_fee_impact(sample_schedule(), 1, 1, 10_000_000);
        assert_eq!(impact.total_fee, 112_000);
        assert_eq!(impact.effective_bps, 112);
    }
}
