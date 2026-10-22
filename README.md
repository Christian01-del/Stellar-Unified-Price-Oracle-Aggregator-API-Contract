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

## Launch-day canary asset sequence

Broad production coverage is enabled in stages. Each stage starts with a small set of low-risk canary assets (see `config/canary-assets.txt`) and only advances when the measurable criteria below are met. Every stage has explicit advance, pause, and rollback criteria so the mainnet readiness decision is reviewable and verifiable.

### Stage 0 — Pre-launch validation

Canary set: none enabled in production; canary assets exercised against staging feeds only.

- **Advance:** manifest check passes for the full canary set; staging feeds publish within `staleness_window` for 24h with zero deviation alerts.
- **Pause:** any manifest validation failure or staging feed gap longer than `staleness_window`.
- **Rollback:** revert to the last validated manifest; no production assets are enabled at this stage.

### Stage 1 — Single canary asset

Canary set: the single lowest-risk asset from `config/canary-assets.txt`.

- **Advance:** 72h of continuous production publishing with zero deviation alerts, zero staleness breaches, and `min_sources` met on every round.
- **Pause:** any deviation alert, staleness breach, or source count below `min_sources`; hold until root cause is resolved and 24h of clean operation is observed.
- **Rollback:** disable the canary asset, restore the previous manifest, and re-enter Stage 0 validation.

### Stage 2 — Canary cohort

Canary set: the full low-risk canary cohort from `config/canary-assets.txt`.

- **Advance:** 7 days of clean operation across the cohort with zero unresolved alerts and all feeds within `deviation_threshold`.
- **Pause:** any unresolved alert, any feed outside `deviation_threshold`, or any source count below `min_sources`; hold the cohort and block further expansion.
- **Rollback:** disable the affected assets, restore the previous manifest, and return to Stage 1 for the remaining canary.

### Stage 3 — Broad production coverage

Canary set: canary cohort plus additional approved assets.

- **Advance:** 14 days of clean cohort operation and a completed risk review and sign-off for each newly added asset per the onboarding policy above.
- **Pause:** any regression in cohort health or any asset lacking complete review/sign-off; new assets are held back.
- **Rollback:** disable newly added assets, restore the previous manifest, and return to Stage 2.

### Repeatable check

The canary sequence and its configuration are validated by the same repeatable manifest check used for the onboarding policy. It verifies that every canary asset in `config/canary-assets.txt` is present, approved, and ordered by stage, and fails the release if the sequence is inconsistent. No secrets or production credentials are committed to the repository.

## Development

See the repository for build and test instructions. Automated coverage exercises the manifest validation across normal, boundary, and failure paths.
