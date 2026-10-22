# Stellar Unified Price Oracle Aggregator

A **decentralized price oracle aggregator** built on Soroban (Stellar smart contracts). Collects price data from multiple permissioned oracle sources, aggregates via **median**, and exposes historical price data for consumer contracts.

## Features

- **Multi-source aggregation** — register multiple oracle sources per asset, aggregate via median
- **Admin governance** — admin controls sources, assets, decimals, description, history limits
- **Median price** — robust single-statistic aggregation resistant to outliers and manipulation
- **Per-source prices** — inspect individual source submissions for transparency
- **Historical prices** — ledger-based price history with configurable retention
- **Contract upgradability** — WASM-based upgrade mechanism
- **SEP-40 compliant** — full implementation of the Stellar Oracle Consumer Interface standard
- **Contract events** — all state changes emit on-chain events for indexers and monitoring
- **27 public endpoints** — full admin, source, asset, submission, query, history, and SEP-40 interface

## Contract Interface

### Admin

| Function | Description |
|----------|-------------|
| `initialize(admin, min_sources, max_history, decimals, description)` | Initialize the contract |
| `set_admin(new_admin)` | Transfer admin rights |
| `get_admin_address()` | Get current admin |
| `set_min_sources_required(n)` | Set minimum sources for aggregation |
| `get_min_sources_required()` | Get minimum sources required |
| `set_max_history_length(n)` | Set max historical records per asset |
| `get_max_history_length()` | Get max history length |
| `set_decimals(n)` | Set price decimals |
| `get_decimals()` | Get decimals |
| `set_description(s)` | Set contract description |
| `get_description()` | Get description |
| `upgrade(new_wasm_hash)` | Upgrade contract WASM |

### Source Management

| Function | Description |
|----------|-------------|
| `add_source(address, name)` | Register an oracle source |
| `remove_source(address)` | Remove an oracle source |
| `is_source(address) -> bool` | Check if address is a registered source |
| `get_oracle_sources() -> OracleSources` | Get all registered sources |

### Asset Management

| Function | Description |
|----------|-------------|
| `register_asset(asset)` | Register a new asset |
| `unregister_asset(asset)` | Unregister an asset |
| `is_asset_registered(asset) -> bool` | Check if asset is registered |

### Price Submission

| Function | Description |
|----------|-------------|
| `submit_price(source, asset, price, timestamp)` | Submit a price (source only) |

### Price Queries

| Function | Description |
|----------|-------------|
| `get_price(asset) -> AggregatePrice` | Get latest aggregated price for an asset |
| `get_source_price(asset, source) -> PriceEntry` | Get latest price from a specific source |
| `get_all_prices(asset) -> Vec<PriceEntry>` | Get latest prices from all sources |
| `get_latest_ledger() -> u32` | Get the latest ledger with price data |

### Historical

| Function | Description |
|----------|-------------|
| `get_historical_price(asset, ledger) -> PriceHistoryEntry` | Get historical price at a specific ledger (interpolated if enabled) |
| `get_historical_prices(asset, start, end) -> Vec<PriceHistoryEntry>` | Get historical prices in a ledger range |
| `has_historical_price(asset, ledger) -> bool` | Check if historical price exists |
| `set_interpolation_enabled(bool)` | Enable/disable linear interpolation for history gaps (admin) |
| `get_interpolation_enabled() -> bool` | Check if interpolation is enabled |

### SEP-40 Oracle Consumer Interface

| Function | Description |
|----------|-------------|
| `base() → Asset` | Returns the base asset (USD) |
| `assets() → Vec<Asset>` | Returns all registered assets as `Asset::Stellar` |
| `decimals() → u32` | Returns price decimals |
| `resolution() → u32` | Returns staleness window in seconds (0 = no expiry) |
| `price(asset, timestamp) → Option<PriceData>` | Get price at or before a given timestamp |
| `prices(asset, records) → Option<Vec<PriceData>>` | Get latest N historical price records |
| `lastprice(asset) → Option<PriceData>` | Get latest aggregated price |

### Contract Events

| Event | Trigger | Topics | Data |
|-------|---------|--------|------|
| `ContractInitializedEvent` | `initialize()` | admin | min_sources, max_history, decimals, description |
| `SourceAddedEvent` | `add_source()` | source, admin | name |
| `SourceRemovedEvent` | `remove_source()` | source, admin | — |
| `AssetRegisteredEvent` | `register_asset()` | asset, admin | — |
| `AssetUnregisteredEvent` | `unregister_asset()` | asset, admin | — |
| `PriceSubmittedEvent` | `submit_price()` | asset, source | price, timestamp |
| `PriceUpdatedEvent` | aggregate price changes | asset | new_price, old_price, timestamp |
| `AdminChangedEvent` | `set_admin()` | old_admin, new_admin | — |
| `ContractUpgradedEvent` | `upgrade()` | new_wasm_hash | — |

## Source Availability & Coverage Review

Provider concentration, asset coverage, and source-specific outage history are reviewed on a
**monthly cadence** via the `source-coverage-review` GitHub Actions workflow
(`.github/workflows/source-coverage-review.yml`). It runs on a cron schedule and can be
triggered manually (`workflow_dispatch`) for ad-hoc reviews.

Each run generates a review artifact that:

- Reports provider concentration across registered sources.
- Lists every registered asset and its number of active sources.
- **Flags assets below the configured diversity threshold** and assigns an owner.
- Summarizes source-specific outage history.

### Configuration

Diversity thresholds and owner assignments live in `config/source-coverage.json`:

```json
{
  "diversity_threshold": 3,
  "default_owner": "@oracle-ops",
  "owners": {
    "GCCC...": "@asset-team"
  }
}
```

- `diversity_threshold` — minimum number of distinct sources an asset must have.
- `default_owner` — owner assigned when no asset-specific owner is configured.
- `owners` — optional per-asset owner overrides (falls back to `CODEOWNERS`).

If the config is missing or malformed, the review fails loudly rather than silently passing.

### Reading the review

The generated `source-coverage-review.md` artifact contains a summary table. Assets below the
threshold are marked `⚠️ BELOW THRESHOLD` with their assigned owner so the on-call operator can
follow up. When no assets are below the threshold, the review reports a clean pass.

## Provider Contract & Dependency Inventory

Every production source has a recorded upstream provider contract so an operator can identify
its dependency, terms, limits, and fallback. The inventory lives in
`config/provider-inventory.json` and is validated by the `provider-inventory` check
(`.github/workflows/provider-inventory.yml`), which runs on every PR that touches the file and
on a weekly schedule.

### Inventory schema

```json
{
  "sources": [
    {
      "source": "GBBB...",
      "name": "Chainlink",
      "provider": "Chainlink Labs",
      "endpoint": "https://...",
      "terms_url": "https://...",
      "rate_limit": "60 req/min",
      "dependencies": ["stellar-rpc", "chainlink-node"],
      "fallback": "GBCC...",
      "replacement_contact": "@oracle-ops"
    }
  ]
}
```

Required fields per source: `source`, `name`, `provider`, `terms_url`, `rate_limit`,
`dependencies`, `fallback`, and `replacement_contact`. The check fails if any production source
is missing a field, references an unregistered fallback, or contains a value that looks like a
secret (e.g. API keys or bearer tokens).

### Operator workflow

1. When onboarding a source, add its entry to `config/provider-inventory.json` in the same PR
   that calls `add_source`.
2. When a provider changes terms, limits, or contacts, update the entry and note the change in
   the PR description.
3. When a provider is deprecated, set `fallback` to the replacement source and update
   `replacement_contact` before removing the old source.

No credentials, API keys, or tokens are stored in the inventory — only public contract metadata
and contact handles.

## oracle-cli

`scripts/oracle-cli.sh` is a shell script plugin that wraps `stellar contract invoke` with
oracle-specific ergonomics.

### Prerequisites

- `stellar` CLI installed and configured
- `ORACLE_CONTRACT_ID` environment variable set to your deployed contract address

### Commands

| Command | Description |
|---------|-------------|
| `oracle-cli init` | Initialize a newly deployed contract |
| `oracle-cli submit-price` | Submit a price from a registered source |
| `oracle-cli get-price` | Get the latest aggregated price for an asset |
| `oracle-cli add-source` | Register a new oracle source (admin) |
| `oracle-cli register-asset` | Register a new asset (admin) |
| `oracle-cli health-check` | Display contract configuration and live status |

### Quick start

```bash
export ORACLE_CONTRACT_ID=CAAAA...
export ORACLE_NETWORK=testnet        # default: testnet

# Initialize
./scripts/oracle-cli.sh init \
  --admin GAAA... --admin-key my-admin \
  --description "My Oracle" --decimals 18

# Add a source
./scripts/oracle-cli.sh add-source \
  --address GBBB... --name "Chainlink" --admin-key my-admin

# Register an asset
./scripts/oracle-cli.sh register-asset \
  --asset GCCC... --admin-key my-admin

# Submit a price  (price = 50 000 × 10^18)
./scripts/oracle-cli.sh submit-price \
  --source GBBB... --asset GCCC... \
  --price 50000000000000000000 \
  --source-key my-source-identity

# Query latest price
./scripts/oracle-cli.sh get-price --asset GCCC...

# Health 
