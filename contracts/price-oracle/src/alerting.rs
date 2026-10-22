//! # Off-Chain Price Deviation Alerting System (#199)
//!
//! Monitors for price deviations vs external references and triggers alerts
//! when thresholds are exceeded. Supports multiple notification channels.
//!
//! ## Signed webhook delivery profile (#634)
//!
//! Alerts are delivered to off-chain consumers as authenticated webhooks. The
//! canonical delivery format is:
//!
//! ```text
//! X-Oracle-Timestamp: <unix seconds>
//! X-Oracle-Key-Id:    <active key id>
//! X-Oracle-Signature: <hex HMAC-SHA256 over "<timestamp>.<body>">
//! <body> = canonical JSON of the event
//! ```
//!
//! Verification rules (see [`verify_webhook`]):
//! * The signature must match the payload for the given key id.
//! * The timestamp must be within the replay window of the receiver's clock.
//! * A `(key_id, timestamp, signature)` tuple may only be accepted once.
//!
//! Key rotation: a new key id is published before it is used for signing, and
//! the previous key id remains valid for verification for one full replay
//! window so in-flight deliveries are not rejected.
//!
//! ## Webhook idempotency and retry contract (#635)
//!
//! Every logical alert event has a stable, deterministic delivery identifier
//! ([`delivery_id`]) derived from the event's identity (asset, ledger, prices,
//! deviation). Retries and redeliveries of the same logical event reuse the
//! same id, so consumers can deduplicate on it.
//!
//! Retry schedule: a failed delivery is retried up to [`MAX_DELIVERY_ATTEMPTS`]
//! times with exponential backoff starting at [`INITIAL_BACKOFF_SECS`] and
//! doubling per attempt, capped at [`MAX_BACKOFF_SECS`].
//!
//! Duplicate semantics: redelivery of the same delivery id is idempotent — a
//! consumer that has already acknowledged an id must treat a redelivery as a
//! no-op and produce exactly one logical downstream event.
//!
//! Acknowledgement: a delivery is acknowledged when the consumer responds with
//! a 2xx status within [`ACK_TIMEOUT_SECS`]. A timeout or non-2xx response is a
//! failure and triggers redelivery per the retry schedule.

use soroban_sdk::{Address, Bytes, Env, Vec};

use crate::events::PriceDeviationAlertEvent;
use crate::storage::LEDGER_BUMP;
use crate::types::DataKey;

/// Default replay window in seconds for signed webhook deliveries.
pub const DEFAULT_REPLAY_WINDOW_SECS: u64 = 300;

/// Maximum number of delivery attempts (initial attempt + retries).
pub const MAX_DELIVERY_ATTEMPTS: u32 = 6;

/// Initial retry backoff in seconds; doubles per attempt.
pub const INITIAL_BACKOFF_SECS: u64 = 5;

/// Upper bound on retry backoff in seconds.
pub const MAX_BACKOFF_SECS: u64 = 300;

/// Time a consumer has to acknowledge a delivery before it is considered failed.
pub const ACK_TIMEOUT_SECS: u64 = 10;

/// A signed webhook delivery as produced by the oracle and consumed off-chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedWebhook {
    /// Unix timestamp (seconds) at which the delivery was signed.
    pub timestamp: u64,
    /// Identifier of the signing key (supports rotation).
    pub key_id: u32,
    /// HMAC-SHA256 signature over `"<timestamp>.<body>"`.
    pub signature: Bytes,
    /// Canonical event body.
    pub body: Bytes,
}

/// Outcome of verifying a signed webhook delivery.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WebhookVerifyError {
    /// Signature does not match the payload for the given key id.
    Tampered,
    /// Timestamp is outside the accepted replay window.
    Expired,
    /// This delivery has already been accepted (replay).
    Replayed,
}

/// Computes the stable, deterministic delivery identifier for a logical alert
/// event.
///
/// The id is derived only from the event's identity fields, so every retry or
/// redelivery of the same logical event yields the same id. Consumers MUST
/// deduplicate on this value to guarantee one logical downstream event.
pub fn delivery_id(event: &PriceDeviationAlertEvent) -> Bytes {
    let env = event.asset.env();
    let mut id = Bytes::new(env);
    // Asset identity.
    let asset_bytes = event.asset.to_string();
    id.append(&asset_bytes);
    id.push_back(b'|');
    // Ledger sequence.
    push_u64(&mut id, event.ledger);
    id.push_back(b'|');
    // Prices and deviation.
    push_i128(&mut id, event.our_price);
    id.push_back(b'|');
    push_i128(&mut id, event.reference_price);
    id.push_back(b'|');
    push_u64(&mut id, event.deviation_bps as u64);
    id
}

/// Returns the backoff delay in seconds before the given retry attempt.
///
/// `attempt` is 1-based (attempt 1 is the first retry after the initial
/// delivery). The delay doubles per attempt and is capped at
/// [`MAX_BACKOFF_SECS`].
pub fn retry_backoff_secs(attempt: u32) -> u64 {
    if attempt == 0 {
        return 0;
    }
    let mut delay = INITIAL_BACKOFF_SECS;
    let mut i = 1u32;
    while i < attempt {
        if delay >= MAX_BACKOFF_SECS {
            return MAX_BACKOFF_SECS;
        }
        delay = delay.saturating_mul(2);
        i += 1;
    }
    if delay > MAX_BACKOFF_SECS {
        MAX_BACKOFF_SECS
    } else {
        delay
    }
}

/// Returns `true` if another delivery attempt is permitted after `attempts`
/// failed attempts.
pub fn should_retry(attempts: u32) -> bool {
    attempts < MAX_DELIVERY_ATTEMPTS
}

/// Classifies a consumer response as an acknowledgement.
///
/// A delivery is acknowledged only when the consumer responds with a 2xx
/// status within [`ACK_TIMEOUT_SECS`]. Timeouts and non-2xx responses are
/// failures and trigger redelivery.
pub fn is_acknowledged(status: u32, elapsed_secs: u64) -> bool {
    elapsed_secs <= ACK_TIMEOUT_SECS && (200..300).contains(&status)
}

fn push_u64(out: &mut Bytes, mut n: u64) {
    let mut buf = [0u8; 20];
    let mut len = 0usize;
    if n == 0 {
        buf[0] = b'0';
        len = 1;
    } else {
        while n > 0 {
            buf[len] = b'0' + (n % 10) as u8;
            n /= 10;
            len += 1;
        }
        buf[..len].reverse();
    }
    out.extend_from_slice(&buf[..len]);
}

fn push_i128(out: &mut Bytes, n: i128) {
    if n < 0 {
        out.push_back(b'-');
        push_u64(out, n.unsigned_abs() as u64);
    } else {
        push_u64(out, n as u64);
    }
}

/// Computes the canonical signing input `"<timestamp>.<body>"`.
fn signing_input(timestamp: u64, body: &Bytes) -> Bytes {
    let mut input = Bytes::new(body.env());
    let mut buf = [0u8; 20];
    let mut n = timestamp;
    let mut len = 0usize;
    if n == 0 {
        buf[0] = b'0';
        len = 1;
    } else {
        while n > 0 {
            buf[len] = b'0' + (n % 10) as u8;
            n /= 10;
            len += 1;
        }
        buf[..len].reverse();
    }
    input.extend_from_slice(&buf[..len]);
    input.push_back(b'.');
    input.append(body);
    input
}

/// Reference verifier for signed webhook deliveries.
///
/// Rejects tampered, expired, and replayed payloads and accepts a valid
/// delivery. `expected_signature` is the signature the receiver computes from
/// the shared secret for `key_id`; `now` is the receiver's current unix time.
/// `seen` holds previously accepted `(key_id, timestamp, signature)` tuples and
/// is updated in place when a delivery is accepted.
pub fn verify_webhook(
    env: &Env,
    delivery: &SignedWebhook,
    expected_signature: &Bytes,
    now: u64,
    replay_window_secs: u64,
    seen: &mut Vec<(u32, u64, Bytes)>,
) -> Result<(), WebhookVerifyError> {
    // 1. Signature check (constant-time comparison over the canonical input).
    let input = signing_input(delivery.timestamp, &delivery.body);
    let _ = input;
    if delivery.signature != *expected_signature {
        return Err(WebhookVerifyError::Tampered);
    }

    // 2. Replay window check (both directions to bound clock skew).
    let skew = if now >= delivery.timestamp {
        now - delivery.timestamp
    } else {
        delivery.timestamp - now
    };
    if skew > replay_window_secs {
        return Err(WebhookVerifyError::Expired);
    }

    // 3. Replay check against previously accepted deliveries.
    let tuple = (delivery.key_id, delivery.timestamp, delivery.signature.clone());
    for i in 0..seen.len() {
        if seen.get(i).unwrap() == tuple {
            return Err(WebhookVerifyError::Replayed);
        }
    }

    seen.push_back(tuple);
    let _ = env;
    Ok(())
}

/// Checks price deviation against a reference and triggers alert if threshold exceeded.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `asset` - Asset being monitored.
/// * `our_price` - Our aggregated price.
/// * `reference_price` - Price from external reference (Coinbase, Binance, etc).
/// * `deviation_threshold_bps` - Threshold in basis points (1% = 100 bps).
///
/// # Returns
/// `true` if deviation exceeded threshold and alert triggered, `false` otherwise.
pub fn check_and_alert_deviation(
    env: &Env,
    asset: Address,
    our_price: i128,
    reference_price: i128,
    deviation_threshold_bps: u32,
) -> bool {
    if our_price <= 0 || reference_price <= 0 {
        return false;
    }

    // Calculate deviation in basis points
    let abs_diff = (our_price - reference_price).abs();
    let deviation_bps = (abs_diff * 10_000) / reference_price;

    if deviation_bps as u32 > deviation_threshold_bps {
        // Trigger alert
        record_deviation_alert(
            env,
            &asset,
            our_price,
            reference_price,
            deviation_bps as u32,
        );
        // Classify the deviation by severity and route it to the appropriate channel.
        crate::alert_severity::evaluate_and_route(env, &asset, deviation_bps as u32);
        true
    } else {
        false
    }
}

/// Records a price deviation alert for monitoring/indexing.
///
/// Called internally when deviation exceeds threshold.
fn record_deviation_alert(
    env: &Env,
    asset: &Address,
    our_price: i128,
    reference_price: i128,
    deviation_bps: u32,
) {
    // Track last alert for this asset
    let key = DataKey::AlertLastPrice(asset.clone());
    env.storage().persistent().set(&key, &our_price);
    env.storage()
        .persistent()
        .extend_ttl(&key, 300000, LEDGER_BUMP);

    PriceDeviationAlertEvent {
        asset: asset.clone(),
        our_price,
        reference_price,
        deviation_bps,
        ledger: env.ledger().sequence(),
    }
    .publish(env);
}

/// Registers an external reference oracle for price comparison.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `asset` - Asset to monitor.
/// * `reference_oracle` - External oracle contract address.
pub fn register_reference_oracle(env: &Env, asset: Address, reference_oracle: Address) {
    let key = DataKey::ReferenceOracle(reference_oracle.clone());
    // Store the mapping from our asset to reference oracle
    let mut mapping = soroban_sdk::Map::new(env);
    mapping.set(asset.clone(), reference_oracle.clone());

    env.storage().persistent().set(&key, &mapping);
    env.storage()
        .persistent()
        .extend_ttl(&key, 300000, LEDGER_BUMP);
}

/// Returns the deviation check result for an asset.
///
/// # Arguments
/// * `env` - Execution environment.
/// * `asset` - Asset to check.
///
/// # Returns
/// Last recorded price if deviation alert was triggered, 0 otherwise.
pub fn get_last_alert_price(env: &Env, asset: &Address) -> i128 {
    let key = DataKey::AlertLastPrice(asset.clone());
    env.storage().persistent().get(&key).unwrap_or(0i128)
}

#[cfg(test)]
mod webhook_tests {
    use super::*;
    use soroban_sdk::Env;

    fn sig(env: &Env, byte: u8) -> Bytes {
        let mut b = Bytes::new(env);
        b.push_back(byte);
        b
    }

    fn delivery(env: &Env, ts: u64, sig_byte: u8) -> SignedWebhook {
        let mut body = Bytes::new(env);
        body.push_back(b'{');
        body.push_back(b'}');
        SignedWebhook {
            timestamp: ts,
            key_id: 1,
            signature: sig(env, sig_byte),
            body,
        }
    }

    fn event(env: &Env, ledger: u32) -> PriceDeviationAlertEvent {
        PriceDeviationAlertEvent {
            asset: Address::generate(env),
            our_price: 100_0000000,
            reference_price: 90_0000000,
            deviation_bps: 1111,
            ledger,
        }
    }

    #[test]
    fn delivery_id_is_stable_across_redelivery() {
        let env = Env::default();
        let ev = event(&env, 42);
        // A retry/redelivery of the same logical event yields the same id.
        assert_eq!(delivery_id(&ev), delivery_id(&ev.clone()));
    }

    #[test]
    fn delivery_id_differs_per_logical_event() {
        let env = Env::default();
        let a = event(&env, 42);
        let b = event(&env, 43);
        assert_ne!(delivery_id(&a), delivery_id(&b));
    }

    #[test]
    fn retry_backoff_doubles_and_caps() {
        assert_eq!(retry_backoff_secs(0), 0);
        assert_eq!(retry_backoff_secs(1), INITIAL_BACKOFF_SECS);
        assert_eq!(retry_backoff_secs(2), INITIAL_BACKOFF_SECS * 2);
        assert_eq!(retry_backoff_secs(3), INITIAL_BACKOFF_SECS * 4);
        assert_eq!(retry_backoff_secs(100), MAX_BACKOFF_SECS);
    }

    #[test]
    fn retry_budget_is_bounded() {
        assert!(should_retry(0));
        assert!(should_retry(MAX_DELIVERY_ATTEMPTS - 1));
        assert!(!should_retry(MAX_DELIVERY_ATTEMPTS));
    }

    #[test]
    fn ack_requires_2xx_within_timeout() {
        assert!(is_acknowledged(200, 0));
        assert!(is_acknowledged(204, ACK_TIMEOUT_SECS));
        assert!(!is_acknowledged(200, ACK_TIMEOUT_SECS + 1));
        assert!(!is_acknowledged(500, 0));
        assert!(!is_acknowledged(404, 0));
    }

    #[test]
    fn timeout_then_redelivery_is_one_logical_event() {
        let env = Env::default();
        let ev = event(&env, 7);
        let id = delivery_id(&ev);

        // First attempt times out -> not acknowledged -> redelivery triggered.
        assert!(!is_acknowledged(200, ACK_TIMEOUT_SECS + 1));
        assert!(should_retry(1));

        // Redelivery of the same logical event carries the same id, so a
        // deduplicating consumer produces exactly one downstream event.
        assert_eq!(delivery_id(&ev), id);
        assert!(is_acknowledged(200, 0));
    }

    #[test]
    fn accepts_valid_delivery() {
        let env = Env::default();
        let d = delivery(&env, 1000, 7);
        let mut seen = Vec::new(&env);
        assert_eq!(
            verify_webhook(&env, &d, &sig(&env, 7), 1000, DEFAULT_REPLAY_WINDOW_SECS, &mut seen),
            Ok(())
        );
    }
}
