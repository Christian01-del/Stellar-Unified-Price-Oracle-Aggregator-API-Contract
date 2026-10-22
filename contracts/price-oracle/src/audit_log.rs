//! Audit log for treasury spend authorization and evidence workflow.
//!
//! Non-routine treasury expenditure must carry a proposal reference, a budget
//! category, an explicit approval, and payment evidence. Every spend attempt
//! (approved or rejected) is recorded so the decision remains traceable.

use soroban_sdk::{contracttype, symbol_short, Address, Env, String, Symbol, Vec};

/// Budget category a treasury spend is charged against.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
#[allow(dead_code)]
pub enum BudgetCategory {
    Operations = 0,
    Development = 1,
    Security = 2,
    Marketing = 3,
    Reserve = 4,
}

/// Outcome of a treasury spend authorization attempt.
#[contracttype]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u32)]
pub enum SpendOutcome {
    Approved = 0,
    RejectedUnapproved = 1,
    RejectedOverBudget = 2,
    RejectedMissingEvidence = 3,
    RejectedInvalidAmount = 4,
}

/// A single treasury spend request with its authorization evidence.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreasurySpendRequest {
    pub proposal_id: Symbol,
    pub category: BudgetCategory,
    pub amount: i128,
    pub approver: Address,
    pub approved: bool,
    pub payment_evidence: String,
}

/// Traceable record retained for every spend attempt.
#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TreasurySpendRecord {
    pub proposal_id: Symbol,
    pub category: BudgetCategory,
    pub amount: i128,
    pub approver: Address,
    pub payment_evidence: String,
    pub outcome: SpendOutcome,
    pub timestamp: u64,
}

const TREASURY_RECORDS: Symbol = symbol_short!("TREASREC");
const TREASURY_BUDGET: Symbol = symbol_short!("TREASBUD");

fn records_key(env: &Env) -> Symbol {
    let _ = env;
    TREASURY_RECORDS
}

fn budget_key(env: &Env, category: BudgetCategory) -> (Symbol, u32) {
    let _ = env;
    (TREASURY_BUDGET, category as u32)
}

/// Set the remaining budget for a category. Used by treasury governance.
pub fn set_category_budget(env: &Env, category: BudgetCategory, budget: i128) {
    env.storage().persistent().set(&budget_key(env, category), &budget);
}

/// Read the remaining budget for a category (0 if unset).
pub fn category_budget(env: &Env, category: BudgetCategory) -> i128 {
    env.storage()
        .persistent()
        .get(&budget_key(env, category))
        .unwrap_or(0)
}

/// Append a spend record to the traceable audit trail.
fn append_record(env: &Env, record: TreasurySpendRecord) {
    let key = records_key(env);
    let mut records: Vec<TreasurySpendRecord> = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or_else(|| Vec::new(env));
    records.push_back(record);
    env.storage().persistent().set(&key, &records);
}

/// Retrieve all retained treasury spend records.
pub fn treasury_spend_records(env: &Env) -> Vec<TreasurySpendRecord> {
    env.storage()
        .persistent()
        .get(&records_key(env))
        .unwrap_or_else(|| Vec::new(env))
}

/// Authorize a non-routine treasury spend.
///
/// Requires a proposal reference, budget category, approval, and payment
/// evidence. Rejects unapproved, over-budget, evidence-less, or non-positive
/// amounts. Every attempt is recorded, including rejections, so the decision
/// remains traceable.
///
/// Returns the outcome; on `Approved` the category budget is decremented.
pub fn authorize_treasury_spend(env: &Env, request: TreasurySpendRequest) -> SpendOutcome {
    let outcome = evaluate_spend(env, &request);

    if outcome == SpendOutcome::Approved {
        let remaining = category_budget(env, request.category);
        env.storage()
            .persistent()
            .set(&budget_key(env, request.category), &(remaining - request.amount));
    }

    append_record(
        env,
        TreasurySpendRecord {
            proposal_id: request.proposal_id,
            category: request.category,
            amount: request.amount,
            approver: request.approver,
            payment_evidence: request.payment_evidence,
            outcome,
            timestamp: env.ledger().timestamp(),
        },
    );

    outcome
}

fn evaluate_spend(env: &Env, request: &TreasurySpendRequest) -> SpendOutcome {
    if request.amount <= 0 {
        return SpendOutcome::RejectedInvalidAmount;
    }
    if !request.approved {
        return SpendOutcome::RejectedUnapproved;
    }
    if request.payment_evidence.len() == 0 {
        return SpendOutcome::RejectedMissingEvidence;
    }
    if request.amount > category_budget(env, request.category) {
        return SpendOutcome::RejectedOverBudget;
    }
    SpendOutcome::Approved
}
