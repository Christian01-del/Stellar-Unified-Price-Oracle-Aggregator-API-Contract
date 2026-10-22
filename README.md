# Oracle

A production-grade price oracle service for Stellar/Soroban.

## Overview

This service aggregates price feeds and publishes them on-chain for consumption by Soroban contracts. It is designed to move from feature-complete development into a dependable production service, with explicit, reviewable, and verifiable mainnet readiness decisions.

## Production asset onboarding policy

Every asset enabled at launch must satisfy the policy below. A new asset **cannot** enter the production manifest unless all required review fields are present and signed off.

### 1. Eligibility

An asset is eligible for the production manifest only if it:

- Is a supported Stellar/Soroban asset (native, issued, or contract-wrapped) with a verifiable on-chain identifier.
- Has at least one reliable, independent price source available to the oracle.
- Has sufficient liquidity and market depth to produce a non-manipulable reference price.
- Is not subject to sanctions, legal restrictions, or issuer controls that would make pricing unreliable.

### 2. Required metadata

Each manifest entry must include the following fields. Entries missing any field are rejected by the manifest check.

| Field | Description |
| --- | --- |
| `asset_id` | Canonical on-chain asset identifier (issuer + code, or contract address). |
| `symbol` | Human-readable ticker used in feeds. |
| `decimals` | Decimal precision of the asset. |
| `price_sources` | Ordered list of approved price sources. |
| `min_sources` | Minimum number of sources required for a valid price. |
| `deviation_threshold` | Maximum allowed price deviation before a feed is flagged. |
| `staleness_window` | Maximum age of an accepted price. |

### 3. Risk review

Before an asset is enabled, a risk review must be recorded covering:

- **Liquidity risk** — depth, spread, and concentration of the asset's markets.
- **Oracle risk** — source reliability, independence, and failure modes.
- **Manipulation risk** — susceptibility to price manipulation and mitigation.
- **Operational risk** — monitoring, alerting, and incident response coverage.

The review must name the reviewer and the date it was completed.

### 4. Sign-off

An asset may only be enabled at launch after explicit sign-off from:

- The oracle maintainer responsible for the feed.
- A second reviewer independent of the feed author.

Sign-off is recorded alongside the asset entry. An asset without complete risk review and sign-off fields is treated as not approved and must not be enabled.

### Enforcement

The production manifest is validated by a repeatable check that rejects any asset entry missing required metadata, risk review, or sign-off fields. This check runs as part of the release process so that no unapproved asset can reach production. No secrets or production credentials are committed to the repository.

## Development

See the repository for build and test instructions. Automated coverage exercises the manifest validation across normal, boundary, and failure paths.
