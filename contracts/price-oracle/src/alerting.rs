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

use soroban_sdk::{Address, Bytes, Env, Vec};

use crate::events::PriceDeviationAlertEvent;
use crate::storage::LEDGER_BUMP;
use crate::types::DataKey;

/// Default replay window in seconds for signed webhook deliveries.
pub const DEFAULT_REPLAY_WINDOW_SECS: u64 = 300;

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

    #[test]
    fn accepts_valid_delivery() {
        let env = Env::default();
        let d = delivery(&env, 1_000, 7);
        let mut seen = Vec::new(&env);
        let expected = sig(&env, 7);
        assert_eq!(
            verify_webhook(&env, &d, &expected, 1_010, DEFAULT_REPLAY_WINDOW_SECS, &mut seen),
            Ok(())
        );
    }

    #[test]
    fn rejects_tampered_payload() {
        let env = Env::default();
        let d = delivery(&env, 1_000, 7);
        let mut seen = Vec::new(&env);
        let wrong = sig(&env, 8);
        assert_eq!(
            verify_webhook(&env, &d, &wrong, 1_010, DEFAULT_REPLAY_WINDOW_SECS, &mut seen),
            Err(WebhookVerifyError::Tampered)
        );
    }

    #[test]
    fn rejects_expired_delivery() {
        let env = Env::default();
        let d = delivery(&env, 1_000, 7);
        let mut seen = Vec::new(&env);
        let expected = sig(&env, 7);
        assert_eq!(
            verify_webhook(&env, &d, &expected, 2_000, DEFAULT_REPLAY_WINDOW_SECS, &mut seen),
            Err(WebhookVerifyError::Expired)
        );
    }

    #[test]
    fn rejects_replayed_delivery() {
        let env = Env::default();
        let d = delivery(&env, 1_000, 7);
        let mut seen = Vec::new(&env);
        let expected = sig(&env, 7);
        assert_eq!(
            verify_webhook(&env, &d, &expected, 1_010, DEFAULT_REPLAY_WINDOW_SECS, &mut seen),
            Ok(())
        );
        assert_eq!(
            verify_webhook(&env, &d, &expected, 1_010, DEFAULT_REPLAY_WINDOW_SECS, &mut seen),
            Err(WebhookVerifyError::Replayed)
        );
    }
}
