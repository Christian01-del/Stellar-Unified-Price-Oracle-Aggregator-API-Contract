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

# Health check
./scripts/oracle-cli.sh health-check
```

## Integration Support: Intake & Triage

Consumers integrating the oracle (wallets, lending markets, indexers, dashboards) can file
integration reports through a single public path. This section defines where to file, what to
include, who responds, and how reports are routed by severity.

### Public filing path

File integration reports as a GitHub issue in this repository using the
**Integration Report** template (`.github/ISSUE_TEMPLATE/integration_report.md`).

- One report per issue; do not bundle unrelated problems.
- Do **not** file reports via DM, email, or comments on unrelated issues — those are not tracked.
- Security-sensitive reports (key exposure, exploitable contract behavior) must **not** be filed
  publicly; follow `SECURITY.md` for private disclosure instead.

### Required reproduction data

Every report must include the following so it can be reproduced and triaged:

| Field | Description |
|-------|-------------|
| Network | `testnet`, `futurenet`, or `mainnet` |
| Contract ID | Deployed contract address (`C...`) |
| SDK / client version | `stellar` CLI, `soroban-sdk`, or client library version |
| Endpoint | Contract function or SEP-40 method involved (e.g. `lastprice`, `price`) |
| Asset | Asset identifier passed to the call |
| Expected vs actual | Observed result and what you expected |
| Reproduction steps | Minimal ordered steps or a script/snippet |
| Error output | Full error message or transaction hash (redact secrets) |
| Timestamp / ledger | When the issue occurred (UTC timestamp and/or ledger) |

**Never include** secret keys, signed XDR containing secrets, admin credentials, or API tokens.
Redact addresses only if they are sensitive; public contract/asset IDs are fine to share.

### Response ownership

| Role | Responsibility |
|------|----------------|
| **Intake triager** (on-call maintainer) | Acknowledges new reports, applies severity label, assigns owner |
| **Integration owner** | Reproduces, diagnoses, and drives the fix or workaround |
| **Contract maintainer** | Owns contract-side fixes and releases |
| **Docs owner** | Updates consumer docs when behavior or guidance changes |

The on-call maintainer acknowledges within **2 business days** and assigns an owner. The
owner posts a status update at least weekly until the report is closed.

### Severity routing

| Severity | Definition | Routing |
|----------|------------|---------|
| **S1 — Critical** | Consumer funds at risk, wrong prices served, or contract unavailable | Page on-call immediately; private channel if security-relevant; fix prioritized above all else |
| **S2 — High** | Integration broken with no workaround; incorrect but non-critical data | Assigned to integration owner same day; target fix in next release |
| **S3 — Medium** | Degraded behavior with a documented workaround | Triaged within 2 business days; scheduled normally |
| **S4 — Low** | Docs gaps, questions, cosmetic issues | Triaged within 5 business days; may be resolved via docs |

### Sample report

A minimal, secret-free report that traces to a triage category:

```
Title: [Integration] lastprice returns None for registered asset on testnet

Network: testnet
Contract ID: CAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAABSC4
SDK / client version: stellar CLI 22.0.0, soroban-sdk 26.0.0
Endpoint: lastprice(asset)
Asset: CCCC... (registered via register_asset)
Expected: Some(PriceData) with latest aggregated price
Actual: None returned even though two sources submitted prices
Reproduction steps:
  1. ./scripts/oracle-cli.sh register-asset --asset CCCC... --admin-key <redacted>
  2. ./scripts/oracle-cli.sh submit-price --source GBBB... --asset CCCC... --price 50000000000000000000 --source-key <redacted>
  3. ./scripts/oracle-cli.sh get-price --asset CCCC...
Error output: (none; returns None)
Timestamp / ledger: 2025-01-01T00:00:00Z, ledger 1234567
```

This sample traces to **S2 — High** (integration broken, no workaround) and is routed to the
integration owner. It contains no secret keys or credentials.

## Getting Started

### Prerequisites

- Rust (stable toolchain, see `rust-toolchain.toml`)
- Soroban CLI (optional, for deployment)

### Build

```bash
make build
```

### Test

```bash
make test
```

All **79 tests pass** with zero warnings.

### End-to-End Testnet Test

Deploys the contract to Stellar testnet and runs a full lifecycle test:

```bash
./scripts/e2e-testnet.sh
```

See [docs/e2e-testnet.md](docs/e2e-testnet.md) for prerequisites, configuration, and expected output.

Automated testnet lifecycle CI (with adversarial phases) runs via `scripts/testnet-lifecycle.sh`; see `.github/workflows/testnet-lifecycle.yml`.

Related reports: [gas cost dashboard](docs/gas-dashboard.md), [chaos invariants](docs/chaos-invariants.md), and mutation testing (`scripts/mutation-gate.sh`, config in `.cargo/mutants.toml`).

### Deploy

```bash
soroban contract deploy \
  --wasm target/wasm32v1-none/release/price_oracle.wasm \
  --source <identity> \
  --network testnet
```

## Project Structure

```
contracts/price-oracle/
├── Cargo.toml
├── .cargo/config.toml
└── src/
    ├── lib.rs       # Contract entrypoint and endpoint implementations
    ├── types.rs     # Data types, storage keys, error codes
    ├── storage.rs   # Storage helpers and median computation
    ├── events.rs    # Contract event definitions
    └── test.rs      # Test suite (65 tests)
```

## Tech Stack

| Layer | Technology |
|-------|-----------|
| Contract | Rust, Soroban SDK v26 |
