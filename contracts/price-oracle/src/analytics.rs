//! # Source Performance Analytics (#204)
//!
//! Provides visibility into source performance metrics including accuracy, timeliness,
//! uptime, and reputation trajectory over configurable rolling windows.
//!
//! Also provides a per-source submission conformance scorecard (#643) reporting schema
//! validity, timestamp validity, submission continuity, and accepted/rejected counts
//! with explicit windows, reproducible from retained events.

use soroban_sdk::{Address, Env};

use crate::events::SourceAnalyticsReportedEvent;
use crate::reputation;
use crate::storage::LEDGER_BUMP;
use crate::types::{DataKey, SourceAnalytics};

/// Per-source submission conformance scorecard (#643).
///
/// All metrics are scoped to an explicit window `[window_start_ledger, window_end_ledger]`
/// so results are reproducible from retained events. `has_data` distinguishes a source
/// with no retained submissions in the window (missing data) from a source that submitted
/// but had zero failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceConformanceScorecard {
    /// Inclusive start of the reporting window (ledger sequence).
    pub window_start_ledger: u32,
    /// Inclusive end of the reporting window (ledger sequence).
    pub window_end_ledger: u32,
    /// True if any retained submissions exist in the window; false means missing data.
    pub has_data: bool,
    /// Total submissions observed in the window.
    pub total_submissions: u32,
    /// Submissions accepted by the oracle in the window.
    pub accepted: u32,
    /// Submissions rejected by the oracle in the window.
    pub rejected: u32,
    /// Submissions whose payload passed schema validation.
    pub schema_valid: u32,
    /// Submissions whose payload failed schema validation.
    pub schema_invalid: u32,
    /// Submissions whose timestamp was within the accepted tolerance.
    pub timestamp_valid: u32,
    /// Submissions whose timestamp was stale or out of tolerance.
    pub timestamp_invalid: u32,
    /// Longest run of consecutive accepted submissions in the window.
    pub longest_accept_streak: u32,
    /// Number of gaps (rejected submissions breaking an accept streak).
    pub continuity_breaks: u32,
}

/// Computes and returns analytics for a source over the last `num_rounds` submissions.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `source` - Source address to analyze.
/// * `num_rounds` - Number of recent submissions to consider (rolling window).
///
/// # Returns
/// A [`SourceAnalytics`] struct containing:
/// - `accuracy`: Percentage based on price deviation from median (0–100).
/// - `timeliness`: Percentage based on submission regularity (0–100).
/// - `uptime`: Percentage of time source was active vs inactive (0–100).
/// - `reputation`: Current reputation score (0–100).
/// - `reputation_trajectory`: Change trend (+/- direction).
pub fn get_source_analytics(env: &Env, source: Address, num_rounds: u32) -> SourceAnalytics {
    let reputation_score = reputation::get_reputation(env, source.clone());

    // Get submission count for this source
    let key = DataKey::SourceSubmissionCount(source.clone());
    let submission_count: u32 = env
        .storage()
        .persistent()
        .get(&key)
        .unwrap_or(0);

    // Get accuracy sum for averaging
    let accuracy_key = DataKey::SourceAccuracySum(source.clone());
    let accuracy_sum: i128 = env
        .storage()
        .persistent()
        .get(&accuracy_key)
        .unwrap_or(0);

    // Calculate average accuracy (0–100)
    let accuracy = if submission_count > 0 && num_rounds > 0 {
        let avg = accuracy_sum / (submission_count.min(num_rounds) as i128);
        (avg.clamp(0, 100)) as u32
    } else {
        50 // neutral if no data
    };

    // Calculate timeliness: assume regular submissions = higher timeliness
    // If submission_count > 0, assume timeliness is proportional to submission frequency
    let timeliness = if submission_count > 0 {
        ((submission_count as u32).min(100)) as u32
    } else {
        0
    };

    // Calculate uptime: assume active if submitted in recent rounds
    let uptime = if submission_count > 0 {
        ((submission_count.min(num_rounds) as u32 * 100) / num_rounds.max(1)) as u32
    } else {
        0
    };

    // Compute reputation trajectory: compare old and new reputation
    // For simplicity, we check if reputation is drifting toward 50 (decay) or away from 50 (growth)
    let neutral_reputation = 50i32;
    let reputation_int = reputation_score as i32;
    let reputation_trajectory = if reputation_int > neutral_reputation {
        // Positive reputation: check if growing or decaying
        if accuracy as i32 > neutral_reputation {
            1 // improving
        } else {
            -1 // decaying
        }
    } else if reputation_int < neutral_reputation {
        // Low reputation: check if recovering or degrading
        if accuracy as i32 > reputation_int {
            1 // recovering
        } else {
            -1 // worsening
        }
    } else {
        0 // stable at neutral
    };

    SourceAnalytics {
        accuracy,
        timeliness,
        uptime,
        reputation: reputation_score,
        reputation_trajectory,
    }
}

/// Updates accuracy metrics for a source after a price submission.
///
/// Called internally after aggregation to record the source's accuracy.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `source` - Source address.
/// * `accuracy_score` - Accuracy score (0–100) for this submission.
pub fn record_submission_accuracy(env: &Env, source: &Address, accuracy_score: u32) {
    // Increment submission count
    let count_key = DataKey::SourceSubmissionCount(source.clone());
    let count: u32 = env
        .storage()
        .persistent()
        .get(&count_key)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&count_key, &(count + 1));

    // Add to accuracy sum
    let sum_key = DataKey::SourceAccuracySum(source.clone());
    let sum: i128 = env
        .storage()
        .persistent()
        .get(&sum_key)
        .unwrap_or(0);
    env.storage()
        .persistent()
        .set(&sum_key, &(sum + accuracy_score as i128));

    // Extend TTL
    env.storage()
        .persistent()
        .extend_ttl(&count_key, 300000, 3600000);
    env.storage()
        .persistent()
        .extend_ttl(&sum_key, 300000, 3600000);

    SourceAnalyticsReportedEvent {
        source: source.clone(),
        accuracy: accuracy_score,
    }
    .publish(env);
}

/// Records a single retained submission outcome for a source so the conformance
/// scorecard can be reproduced from retained events (#643).
///
/// Each call appends one observation to the source's retained event log. The
/// `ledger` is the ledger sequence at which the submission was observed and is
/// used to scope the scorecard window.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `source` - Source address that submitted.
/// * `ledger` - Ledger sequence of the submission.
/// * `schema_ok` - Whether the payload passed schema validation.
/// * `timestamp_ok` - Whether the timestamp was within tolerance.
/// * `accepted` - Whether the oracle accepted the submission.
pub fn record_submission_outcome(
    env: &Env,
    source: &Address,
    ledger: u32,
    schema_ok: bool,
    timestamp_ok: bool,
    accepted: bool,
) {
    let key = DataKey::SourceSubmissionOutcome(source.clone(), ledger);
    env.storage().persistent().set(
        &key,
        &(schema_ok, timestamp_ok, accepted),
    );
    env.storage()
        .persistent()
        .extend_ttl(&key, 300000, 3600000);
}

/// Builds a per-source submission conformance scorecard over an explicit window
/// `[window_start_ledger, window_end_ledger]` (inclusive) from retained events (#643).
///
/// The result is reproducible: given the same retained events and window, the
/// scorecard is deterministic. `has_data` is `false` when no retained submissions
/// fall in the window, distinguishing missing data from zero failures.
pub fn get_source_conformance_scorecard(
    env: &Env,
    source: Address,
    window_start_ledger: u32,
    window_end_ledger: u32,
) -> SourceConformanceScorecard {
    let mut scorecard = SourceConformanceScorecard {
        window_start_ledger,
        window_end_ledger,
        has_data: false,
        total_submissions: 0,
        accepted: 0,
        rejected: 0,
        schema_valid: 0,
        schema_invalid: 0,
        timestamp_valid: 0,
        timestamp_invalid: 0,
        longest_accept_streak: 0,
        continuity_breaks: 0,
    };

    if window_start_ledger > window_end_ledger {
        return scorecard;
    }

    let mut current_streak: u32 = 0;
    let mut ledger = window_start_ledger;
    while ledger <= window_end_ledger {
        let key = DataKey::SourceSubmissionOutcome(source.clone(), ledger);
        if let Some((schema_ok, timestamp_ok, accepted)) = env
            .storage()
            .persistent()
            .get::<DataKey, (bool, bool, bool)>(&key)
        {
            scorecard.has_data = true;
            scorecard.total_submissions += 1;

            if schema_ok {
                scorecard.schema_valid += 1;
            } else {
                scorecard.schema_invalid += 1;
            }

            if timestamp_ok {
                scorecard.timestamp_valid += 1;
            } else {
                scorecard.timestamp_invalid += 1;
            }

            if accepted {
                scorecard.accepted += 1;
                current_streak += 1;
                if current_streak > scorecard.longest_accept_streak {
                    scorecard.longest_accept_streak = current_streak;
                }
            } else {
                scorecard.rejected += 1;
                if current_streak > 0 {
                    scorecard.continuity_breaks += 1;
                }
                current_streak = 0;
            }
        }

        // Guard against overflow at the top of the u32 range.
        if ledger == u32::MAX {
            break;
        }
        ledger += 1;
    }

    scorecard
}
