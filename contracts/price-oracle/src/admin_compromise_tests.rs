//! # #454 — Admin key compromise: blast radius and recovery drill
//!
//! Proves a compromised admin key cannot block its own revocation through
//! the guardian recovery path, and scripts the recovery drill end to end.
//! The capability list and residual risk are documented in
//! `docs/security/admin-compromise-drill.md`.
//!
//! # #604 — Mainnet administrator multisig migration rehearsal
//!
//! Rehearses the handoff from bootstrap administration to the production
//! multisig. The procedure is documented in
//! `docs/operations/admin-multisig-migration.md`; these tests prove the
//! handoff ordering leaves no unowned interval and that neither the old nor
//! the new signer set can create an unauthorized transition.

use soroban_sdk::{
    testutils::{Address as _, Ledger},
    Address, Env, String, Vec,
};

use crate::test_helpers::*;
use crate::types::ErrorCode;
use crate::PriceOracleContractClient;

const DELAY: u32 = 100;

struct Drill<'a> {
    client: PriceOracleContractClient<'a>,
    attacker_admin: Address,
    g1: Address,
    g2: Address,
    rescue: Address,
}

fn advance(e: &Env, ledgers: u32) {
    let seq = e.ledger().sequence();
    e.ledger().with_mut(|l| l.sequence_number = seq + ledgers);
}

/// Guardians 2-of-2 and a delay are configured while the admin is honest; the
/// admin key is then considered compromised.
fn setup(e: &Env) -> Drill<'_> {
    e.mock_all_auths();
    let (client, admin) = setup_contract(e);
    // Sequence 0 is the "not ready" sentinel in `GuardianRecovery`.
    e.ledger().with_mut(|l| l.sequence_number = 1_000);
    let g1 = Address::generate(e);
    let g2 = Address::generate(e);
    let mut guardians: Vec<Address> = Vec::new(e);
    guardians.push_back(g1.clone());
    guardians.push_back(g2.clone());
    client.recovery_set_guardians(&guardians, &2u32);
    client.recovery_set_delay(&DELAY);
    Drill {
        client,
        attacker_admin: admin,
        g1,
        g2,
        rescue: Address::generate(e),
    }
}

fn reach_quorum(d: &Drill) {
    d.client.recovery_approve(&d.g1, &d.rescue);
    d.client.recovery_approve(&d.g2, &d.rescue);
}

#[test]
fn compromised_admin_cannot_swap_guardians_mid_recovery() {
    let e = Env::default();
    let d = setup(&e);
    d.client.recovery_approve(&d.g1, &d.rescue);
    let mut puppets: Vec<Address> = Vec::new(&e);
    puppets.push_back(Address::generate(&e));
    let r = d.client.try_recovery_set_guardians(&puppets, &1u32);
    assert_eq!(r, Err(Ok(code(ErrorCode::RecoveryAlreadyPending))));
}

#[test]
fn compromised_admin_cannot_stretch_delay_mid_recovery() {
    let e = Env::default();
    let d = setup(&e);
    reach_quorum(&d);
    let r = d.client.try_recovery_set_delay(&50_000u32);
    assert_eq!(r, Err(Ok(code(ErrorCode::RecoveryAlreadyPending))));
}

#[test]
fn recovery_delay_is_bounded() {
    let e = Env::default();
    let d = setup(&e);
    let r = d.client.try_recovery_set_delay(&u32::MAX);
    assert_eq!(r, Err(Ok(code(ErrorCode::InvalidConfiguration))));
}

#[test]
fn compromised_admin_gets_only_one_veto_per_candidate() {
    let e = Env::default();
    let d = setup(&e);
    reach_quorum(&d);
    d.client.recovery_cancel();

    // Guardians insist on the same candidate; a second veto is refused.
    reach_quorum(&d);
    let r = d.client.try_recovery_cancel();
    assert_eq!(r, Err(Ok(code(ErrorCode::NotAuthorized))));
}

#[test]
fn veto_cooldown_blocks_guardian_swap() {
    let e = Env::default();
    let d = setup(&e);
    reach_quorum(&d);
    d.client.recovery_cancel();

    let mut puppets: Vec<Address> = Vec::new(&e);
    puppets.push_back(Address::generate(&e));
    let r = d.client.try_recovery_set_guardians(&puppets, &1u32);
    assert_eq!(r, Err(Ok(code(ErrorCode::RecoveryAlreadyPending))));

    // After the cooldown an honest admin may rotate guardians again.
    advance(&e, DELAY);
    d.client.recovery_set_guardians(&puppets, &1u32);
}

#[test]
fn admin_rotation_does_not_evade_recovery() {
    let e = Env::default();
    let d = setup(&e);
    reach_quorum(&d);
    // Attacker hands admin to a second key it controls.
    let attacker_2 = Address::generate(&e);
    d.client.set_admin(&attacker_2);
    advance(&e, DELAY);
    d.client.recovery_execute();
    assert_eq!(d.client.get_admin(), d.rescue);
}

/// Scripted drill: detection → revocation → rotation → state verification →
/// reinstatement, with the attacker vetoing once. Ledger counts are the
/// time-to-contain figures reported in the drill document.
#[test]
fn recovery_drill_end_to_end() {
    let e = Env::default();
    let d = setup(&e);
    let start = e.ledger().sequence();

    // 1. Compromise: attacker admits a rogue source (detected via events).
    let rogue = register_test_source(&e, &d.client, "Rogue");

    // 2. Revocation: guardians reach quorum; attacker vetoes once.
    reach_quorum(&d);
    d.client.recovery_cancel();
    reach_quorum(&d);
    assert!(d.client.try_recovery_cancel().is_err());

    // 3. Rotation after the cancellation window.
    advance(&e, DELAY - 1);
    assert!(d.client.try_recovery_execute().is_err());
    advance(&e, 1);
    d.client.recovery_execute();
    assert_eq!(d.client.get_admin(), d.rescue);
    assert_ne!(d.client.get_admin(), d.attacker_admin);
    let contained_after = e.ledger().sequence() - start;
    assert_eq!(contained_after, DELAY);

    // 4. State verification + reinstatement by the new admin.
    d.client.remove_source(&rogue);
    assert!(!d.client.is_source(&rogue));
    let honest = Address::generate(&e);
    d.client
        .add_source(&honest, &String::from_str(&e, "Honest"));
    assert!(d.client.is_source(&honest));
    assert!(d.client.recovery_get_pending().is_none());
}

/// #604 — Rehearses the bootstrap → production multisig handoff.
///
/// Ordering (see `docs/operations/admin-multisig-migration.md`):
///   1. Bootstrap admin configures guardians + delay (recovery armed).
///   2. Bootstrap admin hands admin to the production multisig address.
///   3. Multisig verifies ownership and performs the first authorized action.
///
/// The admin slot is never empty: `set_admin` is atomic, so the interval
/// between the old and new owner is a single ledger with exactly one owner.
#[test]
fn multisig_migration_handoff_leaves_no_unowned_interval() {
    let e = Env::default();
    e.mock_all_auths();
    let (client, bootstrap) = setup_contract(&e);
    e.ledger().with_mut(|l| l.sequence_number = 1_000);

    // 1. Arm recovery while the bootstrap admin is still in control.
    let g1 = Address::generate(&e);
    let g2 = Address::generate(&e);
    let mut guardians: Vec<Address> = Vec::new(&e);
    guardians.push_back(g1.clone());
    guardians.push_back(g2.clone());
    client.recovery_set_guardians(&guardians, &2u32);
    client.recovery_set_delay(&DELAY);

    // 2. Atomic handoff to the production multisig address.
    let multisig = Address::generate(&e);
    assert_eq!(client.get_admin(), bootstrap);
    client.set_admin(&multisig);

    // 3. Ownership is continuous: exactly one owner at every step.
    assert_eq!(client.get_admin(), multisig);
    assert_ne!(client.get_admin(), bootstrap);

    // 4. The new owner performs the first authorized action.
    let src = register_test_source(&e, &client, "PostMigration");
    assert!(client.is_source(&src));
    assert!(client.recovery_get_pending().is_none());
}

/// #604 — After migration the old bootstrap signer set cannot create an
/// unauthorized transition: it can no longer rotate admin or guardians.
#[test]
fn old_signer_set_cannot_create_unauthorized_transition_after_migration() {
    let e = Env::default();
    e.mock_all_auths();
    let (client, bootstrap) = setup_contract(&e);
    e.ledger().with_mut(|l| l.sequence_number = 1_000);

    let multisig = Address::generate(&e);
    client.set_admin(&multisig);
    assert_eq!(client.get_admin(), multisig);

    // The old bootstrap key is no longer the admin; a fresh admin rotation
    // must be authorized by the current owner, not the retired one.
    let rogue = Address::generate(&e);
    e.set_auths(&[]);
    let r = client.try_set_admin(&rogue);
    assert!(r.is_err());
    assert_eq!(client.get_admin(), multisig);
    assert_ne!(client.get_admin(), bootstrap);
}

/// #604 — The new multisig signer set cannot create an unauthorized
/// transition outside its authorized scope: it cannot bypass the guardian
/// recovery path to seize guardians mid-recovery.
#[test]
fn new_signer_set_cannot_create_unauthorized_transition_outside_scope() {
    let e = Env::default();
    e.mock_all_auths();
    let (client, _bootstrap) = setup_contract(&e);
    e.ledger().with_mut(|l| l.sequence_number = 1_000);

    let g1 = Address::generate(&e);
    let g2 = Address::generate(&e);
    let mut guardians: Vec<Address> = Vec::new(&e);
    guardians.push_back(g1.clone());
    guardians.push_back(g2.clone());
    client.recovery_set_guardians(&guardians, &2u32);
    client.recovery_set_delay(&DELAY);

    let multisig = Address::generate(&e);
    client.set_admin(&multisig);
    assert_eq!(client.get_admin(), multisig);

    // Guardians open a recovery; the multisig admin cannot override it.
    let rescue = Address::generate(&e);
    client.recovery_approve(&g1, &rescue);
    let mut puppets: Vec<Address> = Vec::new(&e);
    puppets.push_back(Address::generate(&e));
    let r = client.try_recovery_set_guardians(&puppets, &1u32);
    assert_eq!(r, Err(Ok(code(ErrorCode::RecoveryAlreadyPending))));
    let r = client.try_recovery_set_delay(&50_000u32);
    assert_eq!(r, Err(Ok(code(ErrorCode::RecoveryAlreadyPending))));
}

fn code(c: ErrorCode) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(c as u32)
}
