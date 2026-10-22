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
//!
//! Reward budget cap and exhaustion behavior (issue #676):
//! an admin sets a hard total payout budget. Cumulative payouts can never
//! exceed it. A payout that would exceed the remaining allocation fails
//! atomically with no partial payout; partial payouts are only permitted
//! when the admin explicitly enables that policy.

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

/// Reward budget state (issue #676).
///
/// `total_budget` is the hard cap on cumulative payouts. `paid_out` tracks
/// the cumulative amount already paid. `allow_partial` is the explicit
/// policy switch: when false (default) a payout that would exceed the
/// remaining allocation fails atomically; when true the payout is clamped
/// to the remaining allocation.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RewardBudget {
    pub total_budget: i128,
    pub paid_out: i128,
    pub allow_partial: bool,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    NextAppealId,
    Appeal(u64),
    Corrections(u64),
    Budget,
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

    /// Admin sets (or updates) the hard reward budget and partial-payout policy.
    ///
    /// The budget cannot be lowered below what has already been paid out, so
    /// the cap can never be retroactively violated. `allow_partial` defaults
    /// to false; it must be set explicitly to permit clamped payouts.
    pub fn set_reward_budget(
        env: Env,
        admin: Address,
        total_budget: i128,
        allow_partial: bool,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        if total_budget < 0 {
            panic!("budget must be non-negative");
        }

        let paid_out = Self::paid_out(&env);
        if total_budget < paid_out {
            panic!("budget below already paid out");
        }

        let budget = RewardBudget {
            total_budget,
            paid_out,
            allow_partial,
        };
        env.storage().instance().set(&DataKey::Budget, &budget);
    }

    /// Pay out a reward against the budget.
    ///
    /// Returns the amount actually paid. If the requested amount fits within
    /// the remaining allocation it is paid in full. If it would exceed the
    /// remaining allocation the payout fails atomically (no partial payout)
    /// unless the admin explicitly enabled `allow_partial`, in which case the
    /// payout is clamped to the remaining allocation.
    pub fn pay_reward(env: Env, admin: Address, amount: i128) -> i128 {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        if amount < 0 {
            panic!("amount must be non-negative");
        }

        let mut budget: RewardBudget = env
            .storage()
            .instance()
            .get(&DataKey::Budget)
            .expect("budget not set");

        let remaining = budget.total_budget - budget.paid_out;

        let payout = if amount <= remaining {
            amount
        } else if budget.allow_partial {
            remaining
        } else {
            panic!("insufficient reward budget");
        };

        budget.paid_out += payout;
        env.storage().instance().set(&DataKey::Budget, &budget);

        payout
    }

    /// Fetch the current reward budget state.
    pub fn get_reward_budget(env: Env) -> RewardBudget {
        env.storage()
            .instance()
            .get(&DataKey::Budget)
            .expect("budget not set")
    }

    /// Remaining allocation available for future payouts.
    pub fn remaining_budget(env: Env) -> i128 {
        let budget: RewardBudget = env
            .storage()
            .instance()
            .get(&DataKey::Budget)
            .expect("budget not set");
        budget.total_budget - budget.paid_out
    }

    fn require_admin(env: &Env, caller: &Address) {
        let admin: Address = env
            .storage()
            .instance()
            .get(&DataKey::Admin)
            .expect("not initialized");
        if caller != &admin {
            panic!("unauthorized reviewer");
        }
    }

    fn paid_out(env: &Env) -> i128 {
        match env.storage().instance().get::<DataKey, RewardBudget>(&DataKey::Budget) {
            Some(budget) => budget.paid_out,
            None => 0,
        }
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
        let extra_updat

/* … truncated 5094 chars — edit only what you need near the top … */
