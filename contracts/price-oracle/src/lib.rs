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
//!
//! Incentive policy versioning and migration (issue #677):
//! incentive policies are version-tagged. Accrued rewards are preserved
//! verbatim across a version bump (grandfathered); only the policy
//! parameters change. Migration conserves total accrued balances exactly
//! (no rounding is applied to accrued amounts).

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

/// Version-tagged incentive policy (issue #677).
///
/// `version` is the policy version this parameter set belongs to. Accrued
/// rewards are tracked separately and are never recomputed on a version
/// bump; existing participants are grandfathered under the version that
/// was active when their rewards accrued.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct IncentivePolicy {
    pub version: u32,
    /// Reward rate in basis points applied to measured contribution.
    pub reward_rate_bps: u32,
    /// Minimum contribution (in stroops) that accrues any reward.
    pub min_contribution: i128,
}

/// Accrued reward balance for a participant, tagged with the policy version
/// under which it accrued. Migration preserves `amount` verbatim.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Accrual {
    pub participant: Address,
    pub amount: i128,
    pub version: u32,
}

/// Result of a policy migration, for auditability.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MigrationResult {
    pub from_version: u32,
    pub to_version: u32,
    /// Total accrued balance before migration (conserved).
    pub total_accrued: i128,
    /// Number of participant accruals carried over.
    pub participants: u32,
}

#[contracttype]
#[derive(Clone)]
pub enum DataKey {
    Admin,
    NextAppealId,
    Appeal(u64),
    Corrections(u64),
    Budget,
    Policy,
    Accruals,
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

    /// Admin sets the incentive policy for a given version.
    ///
    /// The version must be strictly greater than the currently stored
    /// version (or this is the first policy). Setting a policy does not
    /// touch accrued balances; use `migrate_policy` to transition.
    pub fn set_incentive_policy(
        env: Env,
        admin: Address,
        version: u32,
        reward_rate_bps: u32,
        min_contribution: i128,
    ) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        if min_contribution < 0 {
            panic!("min_contribution must be non-negative");
        }
        if let Some(current) = Self::policy(&env) {
            if version <= current.version {
                panic!("version must increase");
            }
        }
        let policy = IncentivePolicy {
            version,
            reward_rate_bps,
            min_contribution,
        };
        env.storage().instance().set(&DataKey::Policy, &policy);
    }

    /// Fetch the current incentive policy.
    pub fn get_incentive_policy(env: Env) -> IncentivePolicy {
        Self::policy(&env).expect("policy not set")
    }

    /// Record an accrued reward for a participant under the current policy
    /// version. Accrued amounts are stored verbatim and never recomputed.
    pub fn accrue_reward(env: Env, admin: Address, participant: Address, amount: i128) {
        admin.require_auth();
        Self::require_admin(&env, &admin);
        if amount < 0 {
            panic!("amount must be non-negative");
        }
        let policy = Self::policy(&env).expect("policy not set");
        let mut accruals = Self::accruals(&env);
        accruals.push_back(Accrual {
            participant,
            amount,
            version: policy.version,
        });
        env.storage().instance().set(&DataKey::Accruals, &accruals);
    }

    /// Total accrued reward balance across all participants.
    pub fn total_accrued(env: Env) -> i128 {
        let accruals = Self::accruals(&env);
        let mut total: i128 = 0;
        for a in accruals.iter() {
            total += a.amount;
        }
        total
    }

    /// Migrate the incentive policy from its current version to `to_version`.
    ///
    /// Migration semantics:
    /// - Accrued balances are preserved verbatim (grandfathered). No rounding
    ///   is applied to accrued amounts, so total accrued balance is conserved
    ///   exactly across the migration.
    /// - Each accrual is re-tagged with `to_version` so future accounting
    ///   reflects the active policy, while the amount is unchanged.
    /// - The policy parameters for `to_version` must already be set via
    ///   `set_incentive_policy`.
    ///
    /// Returns a `MigrationResult` documenting the transition for audit.
    pub fn migrate_policy(env: Env, admin: Address, to_version: u32) -> MigrationResult {
        admin.require_auth();
        Self::require_admin(&env, &admin);

        let current = Self::policy(&env).expect("policy not set");
        if to_version <= current.version {
            panic!("target version must increase");
        }

        let accruals = Self::accruals(&env);
        let mut total: i128 = 0;
        let mut migrated: Vec<Accrual> = Vec::new(&env);
        for a in accruals.iter() {
            total += a.amount;
            migrated.push_back(Accrual {
                participant: a.participant,
                amount: a.amount,
                version: to_version,
            });
        }
        let participants = migrated.len();

        env.storage().instance().set(&DataKey::Accruals, &migrated);

        let new_policy = IncentivePolicy {
            version: to_version,
            reward_rate_bps: current.reward_rate_bps,
            min_contribution: current.min_contribution,
        };
        env.storage().instance().set(&DataKey::Policy, &new_policy);

        MigrationResult {
            from_version: current.version,
            to_version,
            total_accrued: total,
            participants,
        }
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

    fn policy(env: &Env) -> Option<IncentivePolicy> {
        env.storage().instance().get(&DataKey::Policy)
    }

    fn accruals(env: &Env) -> Vec<Accrual> {
        match env.storage().instance().get::<DataKey, Vec<Accrual>>(&DataKey::Accruals) {
            Some(accruals) => accruals,
            None => Vec::new(env),
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
        sour

/* … truncated 4180 chars — edit only what you need near the top … */
