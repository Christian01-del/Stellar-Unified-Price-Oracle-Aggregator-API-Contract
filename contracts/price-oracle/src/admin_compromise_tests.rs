//! # #454 — Admin key compromise: blast radius and recovery drill
//!
//! Proves a compromised admin key cannot block its own revocation through
//! the guardian recovery path, and scripts the recovery drill end to end.
//! The capability list and residual risk are documented in
//! `docs/security/admin-compromise-drill.md`.
//!
//! # #653 — Production multisig policy and signer eligibility
//!
//! Encodes the production multisig policy as a repeatable check: signer
//! independence, eligibility, conflict declaration/mitigation, rotation
//! cadence, and documented key custody for critical actions. The policy is
//! documented in `docs/security/multisig-policy.md`; the review checklist
//! below records required signers and rejects an unmitigated conflict.

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

fn code(c: ErrorCode) -> soroban_sdk::Error {
    soroban_sdk::Error::from_contract_error(c as u32)
}

// ---------------------------------------------------------------------------
// #653 — Production multisig policy and signer eligibility
// ---------------------------------------------------------------------------

/// Rotation cadence for production signers, in ledgers (~90 days at 5s/ledger).
const ROTATION_CADENCE_LEDGERS: u32 = 1_555_200;
/// Maximum age a signer key may reach before it must be rotated.
const MAX_SIGNER_AGE_LEDGERS: u32 = ROTATION_CADENCE_LEDGERS;
/// Minimum number of independent signers required for critical actions.
const MIN_INDEPENDENT_SIGNERS: u32 = 3;

/// A declared conflict of interest on a signer, with optional mitigation.
struct Conflict {
    declared: bool,
    mitigated: bool,
}

/// A production signer under the multisig policy.
struct Signer {
    address: Address,
    /// Custody/control domain (e.g. org or HSM operator). Signers sharing a
    /// domain are not independent.
    custody_domain: u32,
    /// Ledger at which the key was provisioned; drives rotation cadence.
    provisioned_at: u32,
    /// Whether the signer meets eligibility criteria (vetted, bonded, etc.).
    eligible: bool,
    conflict: Conflict,
}

/// Signer independence: no two signers may share a custody/control domain.
fn signers_are_independent(signers: &[Signer]) -> bool {
    for (i, a) in signers.iter().enumerate() {
        for b in signers.iter().skip(i + 1) {
            if a.custody_domain == b.custody_domain {
                return false;
            }
        }
    }
    true
}

/// Signer eligibility: every signer must be vetted and within rotation age.
fn signers_are_eligible(signers: &[Signer], now: u32) -> bool {
    signers.iter().all(|s| {
        s.eligible && now.saturating_sub(s.provisioned_at) <= MAX_SIGNER_AGE_LEDGERS
    })
}

/// A declared conflict must carry a mitigation; an undeclared conflict is
/// treated as absent. Returns false when a declared conflict lacks mitigation.
fn conflicts_are_mitigated(signers: &[Signer]) -> bool {
    signers
        .iter()
        .all(|s| !s.conflict.declared || s.conflict.mitigated)
}

/// Full review checklist: records required signers and rejects a declared
/// conflict without mitigation.
fn review_checklist_passes(signers: &[Signer], now: u32) -> bool {
    signers.len() as u32 >= MIN_INDEPENDENT_SIGNERS
        && signers_are_independent(signers)
        && signers_are_eligible(signers, now)
        && conflicts_are_mitigated(signers)
}

fn make_signer(e: &Env, domain: u32, provisioned_at: u32) -> Signer {
    Signer {
        address: Address::generate(e),
        custody_domain: domain,
        provisioned_at,
        eligible: true,
        conflict: Conflict {
            declared: false,
            mitigated: false,
        },
    }
}

fn compliant_signers(e: &Env, now: u32) -> Vec<Signer> {
    let mut v: Vec<Signer> = Vec::new(e);
    v.push_back(make_signer(e, 1, now));
    v.push_back(make_signer(e, 2, now));
    v.push_back(make_signer(e, 3, now));
    v
}

#[test]
fn compliant_multisig_passes_review_checklist() {
    let e = Env::default();
    let now = 10_000u32;
    let signers = compliant_signers(&e, now);
    assert!(review_checklist_passes(&signers, now));
}

#[test]
fn shared_custody_domain_fails_independence() {
    let e = Env::default();
    let now = 10_000u32;
    let mut signers = compliant_signers(&e, now);
    // Two signers under the same custody domain are not independent.
    signers.get_mut(1).unwrap().custody_domain = 1;
    assert!(!signers_are_independent(&signers));
    assert!(!review_checklist_passes(&signers, now));
}

#[test]
fn ineligible_signer_fails_eligibility() {
    let e = Env::default();
    let now = 10_000u32;
    let mut signers = compliant_signers(&e, now);
    signers.get_mut(0).unwrap().eligible = false;
    assert!(!signers_are_eligible(&signers, now));
    assert!(!review_checklist_passes(&signers, now));
}

#[test]
fn stale_signer_fails_rotation_cadence() {
    let e = Env::default();
    let now = 10_000u32;
    let mut signers = compliant_signers(&e, now);
    // Provisioned beyond the rotation cadence ago.
    signers.get_mut(2).unwrap().provisioned_at = now - MAX_SIGNER_AGE_LEDGERS - 1;
    assert!(!signers_are_eligible(&signers, now));
    assert!(!review_checklist_passes(&signers, now));
}

#[test]
fn declared_conflict_without_mitigation_is_rejected() {
    let e = Env::default();
    let now = 10_000u32;
    let mut signers = compliant_signers(&e, now);
    signers.get_mut(0).unwrap().conflict = Conflict {
        declared: true,
        mitigated: false,
    };
    assert!(!conflicts_are_mitigated(&signers));
    assert!(!review_checklist_passes(&signers, now));
}

#[test]
fn declared_conflict_with_mitigation_is_accepted() {
    let e = Env::default();
    let now = 10_000u32;
    let mut signers = compliant_signers(&e, now);
    signers.get_mut(0).unwrap().conflict = Conflict {
        declared: true,
        mitigated: true,
    };
    assert!(conflicts_are_mitigated(&signers));
    assert!(review_checklist_passes(&signers, now));
}

#[test]
fn insufficient_signers_fail_review_checklist() {
    let e = Env::default();
    let now = 10_000u32;
    let mut signers: Vec<Signer> = Vec::new(&e);
    signers.push_back(make_signer(&e, 1, now));
    signers.push_back(make_signer(&e, 2, now));
    assert!(!review_checklist_passes(&signers, now));
}
