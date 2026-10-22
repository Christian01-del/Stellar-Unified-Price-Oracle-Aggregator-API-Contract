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

/// Explicit window over which a conformance scorecard is computed (#643).
///
/// Both a ledger range and a timestamp range are recorded so that a scorecard
/// can be reproduced exactly from retained events. `from_ledger`/`to_ledger`
/// are inclusive; `from_timestamp`/`to_timestamp` are inclusive seconds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScorecardWindow {
    pub from_ledger: u32,
    pub to_ledger: u32,
    pub from_timestamp: u64,
    pub to_timestamp: u64,
}

/// Per-source submission conformance scorecard (#643).
///
/// Reports schema validity, timestamp validity, submission continuity, and
/// accepted/rejected counts for a single source over an explicit window.
///
/// `has_data` distinguishes "no retained events in window" (missing data)
/// from "events present but zero failures" (a clean scorecard). When
/// `has_data` is false, all counts are zero and must not be read as a pass.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceConformanceScorecard {
    pub source: Address,
    pub window: ScorecardWindow,
    /// Whether any retained events for this source fall inside the window.
    pub has_data: bool,
    /// Total submissions observed for this source within the window.
    pub total_submissions: u32,
    /// Submissions whose payload passed schema validation.
    pub schema_valid: u32,
    /// Submissions whose payload failed schema validation.
    pub schema_invalid: u32,
    /// Submissions whose timestamp was within the accepted skew.
    pub timestamp_valid: u32,
    /// Submissions whose timestamp was outside the accepted skew.
    pub timestamp_invalid: u32,
    /// Accepted submissions (schema + timestamp valid).
    pub accepted: u32,
    /// Rejected submissions (any validity failure).
    pub rejected: u32,
    /// Largest gap (in ledgers) between consecutive submissions in the window.
    pub max_continuity_gap: u32,
    /// Number of continuity gaps that exceeded the expected cadence.
    pub continuity_breaks: u32,
}

/// Compute a per-source submission conformance scorecard (#643).
///
/// Scans retained audit entries for `source` whose ledger falls within
/// `[window.from_ledger, window.to_ledger]` and whose timestamp falls within
/// `[window.from_timestamp, window.to_timestamp]`. Results are deterministic
/// and reproducible from the retained audit log alone.
///
/// A submission is considered accepted when its payload is schema-valid and
/// its timestamp is valid; otherwise it is rejected. Continuity is measured
/// as the ledger gap between consecutive submissions; a gap larger than
/// `max_expected_gap` counts as a continuity break.
///
/// # Arguments
///
/// * `env` - The Soroban execution environment.
/// * `source` - The source address to score.
/// * `window` - The explicit ledger/timestamp window to evaluate.
/// * `max_expected_gap` - Maximum acceptable ledger gap between submissions.
///
/// # Returns
///
/// A `SourceConformanceScorecard` with `has_data` set to false when no
/// retained events for `source` fall inside the window.
pub fn compute_source_conformance_scorecard(
    env: &Env,
    source: Address,
    window: ScorecardWindow,
    max_expected_gap: u32,
) -> SourceConformanceScorecard {
    let entry_count: u32 = env
        .storage()
        .persistent()
        .get(&DataKey::AuditEntryCount)
        .unwrap_or(0);

    let mut scorecard = SourceConformanceScorecard {
        source: source.clone(),
        window: window.clone(),
        has_data: false,
        total_submissions: 0,
        schema_valid: 0,
        schema_invalid: 0,
        timestamp_valid: 0,
        timestamp_invalid: 0,
        accepted: 0,
        rejected: 0,
        max_continuity_gap: 0,
        continuity_breaks: 0,
    };

    let mut last_ledger: Option<u32> = None;

    for i in 1..=entry_count {
        let entry = match env
            .storage()
            .persistent()
            .get::<_, AuditEntry>(&DataKey::AuditEntry(i))
        {
            Some(e) => e,
            None => continue,
        };

        if entry.admin != source {
            continue;
        }
        if entry.ledger < window.from_ledger || entry.ledger > window.to_ledger {
            continue;
        }
        if entry.timestamp < window.from_timestamp || entry.timestamp > window.to_timestamp {
            continue;
        }

        scorecard.has_data = true;
        scorecard.total_submissions += 1;

        // Schema validity: a non-empty payload is treated as schema-valid.
        let schema_ok = !entry.data.is_empty();
        if schema_ok {
            scorecard.schema_valid += 1;
        } else {
            scorecard.schema_invalid += 1;
        }

        // Timestamp validity: must be non-zero and within the window bounds.
        let timestamp_ok = entry.timestamp != 0
            && entry.timestamp >= window.from_timestamp
            && entry.timestamp <= window.to_timestamp;
        if timestamp_ok {
            scorecard.timestamp_valid += 1;
        } else {
            scorecard.timestamp_invalid += 1;
        }

        if schema_ok && timestamp_ok {
            scorecard.accepted += 1;
        } else {
            scorecard.rejected += 1;
        }

        // Submission continuity: gap between consecutive submissions.
        if let Some(prev) = last_ledger {
            let gap = entry.ledger.saturating_sub(prev);
            if gap > scorecard.max_continuity_gap {
                scorecard.max_continuity_gap = gap;
            }
            if gap > max_expected_gap {
                scorecard.continuity_breaks += 1;
            }
        }
        last_ledger = Some(entry.ledger);
    }

    scorecard
}
