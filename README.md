# Price Oracle

A Soroban-based price oracle contract for Stellar. It aggregates signed price
submissions from a set of authorized providers, exposes the median price per
asset, and records an auditable trail of every accepted update.

## Repository layout

- `contracts/price-oracle/` — the Soroban contract crate.
  - `src/lib.rs` — contract entry points and storage layout.
  - `src/analytics.rs` — usage and reward analytics.
  - `src/audit_log.rs` — append-only audit trail of accepted updates.
  - `src/audit_log_tests.rs` — focused tests for the audit trail.
- `README.md` — this document.

## Building and testing

```sh
cargo build --release --target wasm32-unknown-unknown
cargo test
```

## Treasury runway and funding scenarios

This section models the operational economics of running the oracle as a
production service. It states the assumptions behind the model, the projected
runway under three scenarios, and the threshold that triggers a funding review.

### Assumptions

All figures are monthly, denominated in USD, and reviewed whenever the
underlying inputs change materially.

| Input | Value | Notes |
| --- | --- | --- |
| Committed funding (treasury balance) | $60,000 | Funds already committed and available. |
| Baseline operational spend | $2,000 / month | Hosting, RPC/indexer access, monitoring, on-call stipend. |
| Low-usage operational spend | $1,400 / month | Reduced infrastructure tier; fewer provider submissions. |
| High-incident operational spend | $4,500 / month | Incident response, extra redundancy, surge support. |
| Funding-review threshold | 6 months of runway | Below this, a funding review is triggered. |

Operational spend covers infrastructure, provider incentives, monitoring, and
incident response. It excludes one-off protocol development costs.

### Runway per scenario

Runway is `committed funding / monthly operational spend`.

| Scenario | Monthly spend | Runway | Status vs. 6-month threshold |
| --- | --- | --- | --- |
| Baseline | $2,000 | 30.0 months | Healthy |
| Low-usage | $1,400 | ~42.9 months | Healthy |
| High-incident | $4,500 | ~13.3 months | Healthy, but monitor |

### Funding-review threshold

A funding review is triggered when projected runway falls **below 6 months**
under the scenario currently in effect. At the baseline spend of $2,000/month,
that corresponds to a treasury balance below **$12,000**. The review covers
replenishment options, spend reduction, and whether the high-incident scenario
has become the steady state.

### Boundary and failure/recovery paths

- **Boundary:** runway exactly at the 6-month threshold does not trigger a
  review; only strictly below it does.
- **Failure:** if the high-incident scenario persists, spend rises to
  $4,500/month and runway shrinks accordingly; sustained high-incident spend is
  treated as a funding-review trigger even while nominal runway is above the
  threshold.
- **Recovery:** once incident spend returns to baseline, runway is recomputed
  from the restored spend and the review condition is re-evaluated.

### Repeatable check

The model is a deterministic function of the inputs above. Recompute runway as
`funding / spend` for each scenario and confirm the funding-review threshold is
applied consistently; any change to the inputs must update this table in the
same change.
