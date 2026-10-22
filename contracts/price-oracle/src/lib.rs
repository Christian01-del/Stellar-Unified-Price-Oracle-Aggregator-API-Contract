//! Price oracle contract.
//!
//! Provides price feeds, source reward accounting, and an appeal/correction
//! process for disputed reward measurements. Corrections are appended as new
//! records so the original audit trail is never overwritten.

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
}
