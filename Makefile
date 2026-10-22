# Oracle contract build & operational tooling
#
# Targets:
#   build            - compile the Soroban contract to wasm
#   test             - run the contract test suite
#   bench-fees       - measure per-operation fee/resource budgets on
#                      mainnet-equivalent network settings and write the
#                      committed baseline to benchmarks/fee-budget.json
#   check-fees       - compare a fresh measurement against the committed
#                      baseline and fail on material regressions (CI gate)

WASM := target/wasm32-unknown-unknown/release/oracle.wasm
BENCH_DIR := benchmarks
BENCH_BASELINE := $(BENCH_DIR)/fee-budget.json
BENCH_CURRENT := $(BENCH_DIR)/fee-budget.current.json

# Mainnet-equivalent Soroban resource limits used for the measurement run.
# These mirror the target network settings so budgets reflect production.
BENCH_NETWORK ?= mainnet
BENCH_INSTRUCTION_LEEWAY ?= 1.0
BENCH_READ_BYTES_LEEWAY ?= 1.0
BENCH_WRITE_BYTES_LEEWAY ?= 1.0

# Regression thresholds (percent). A measurement exceeding the committed
# baseline by more than these amounts is flagged as a material regression.
BENCH_MAX_INSTRUCTION_REGRESSION ?= 10
BENCH_MAX_READ_BYTES_REGRESSION ?= 10
BENCH_MAX_WRITE_BYTES_REGRESSION ?= 10
BENCH_MAX_FEE_REGRESSION ?= 10

.PHONY: build test bench-fees check-fees clean

build:
	cargo build --target wasm32-unknown-unknown --release

test:
	cargo test

# Measure deployment, initialization, source updates, and representative
# price submissions under mainnet-equivalent resource limits, then write the
# committed baseline. Run this when intentionally changing resource usage.
bench-fees: build
	@mkdir -p $(BENCH_DIR)
	cargo run --release --bin fee-bench -- \
		--wasm $(WASM) \
		--network $(BENCH_NETWORK) \
		--instruction-leeway $(BENCH_INSTRUCTION_LEEWAY) \
		--read-bytes-leeway $(BENCH_READ_BYTES_LEEWAY) \
		--write-bytes-leeway $(BENCH_WRITE_BYTES_LEEWAY) \
		--out $(BENCH_BASELINE)
	@echo "Wrote fee budget baseline to $(BENCH_BASELINE)"

# Repeatable CI check: re-measure and compare against the committed baseline.
# Fails (non-zero exit) when any per-operation budget regresses materially.
check-fees: build
	@test -f $(BENCH_BASELINE) || { \
		echo "error: missing committed baseline $(BENCH_BASELINE); run 'make bench-fees' first" >&2; \
		exit 1; \
	}
	cargo run --release --bin fee-bench -- \
		--wasm $(WASM) \
		--network $(BENCH_NETWORK) \
		--instruction-leeway $(BENCH_INSTRUCTION_LEEWAY) \
		--read-bytes-leeway $(BENCH_READ_BYTES_LEEWAY) \
		--write-bytes-leeway $(BENCH_WRITE_BYTES_LEEWAY) \
		--out $(BENCH_CURRENT)
	cargo run --release --bin fee-bench -- compare \
		--baseline $(BENCH_BASELINE) \
		--current $(BENCH_CURRENT) \
		--max-instruction-regression $(BENCH_MAX_INSTRUCTION_REGRESSION) \
		--max-read-bytes-regression $(BENCH_MAX_READ_BYTES_REGRESSION) \
		--max-write-bytes-regression $(BENCH_MAX_WRITE_BYTES_REGRESSION) \
		--max-fee-regression $(BENCH_MAX_FEE_REGRESSION)

clean:
	cargo clean
	rm -f $(BENCH_CURRENT)
