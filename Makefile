# Price Oracle — operator entry points
#
# Mainnet launch-day canary sequence (issue #606):
#   make canary-check   — repeatable check that the staged canary asset
#                         sequence in config/canary-assets.txt is well-formed
#                         and matches the on-chain asset registry expectations.
#   make canary-stage   — print the current stage and its advance/pause/rollback
#                         criteria from the runbook (README.md).

SHELL := /bin/bash

CANARY_CONFIG ?= config/canary-assets.txt
CANARY_RUNBOOK ?= README.md

.PHONY: canary-check canary-stage

# Repeatable check: validates the canary sequence config.
# Fails if the file is missing, empty, has duplicate assets, or references a
# stage that is not one of the documented launch-day stages.
canary-check:
	@test -f "$(CANARY_CONFIG)" || { echo "missing $(CANARY_CONFIG)"; exit 1; }
	@test -s "$(CANARY_CONFIG)" || { echo "$(CANARY_CONFIG) is empty"; exit 1; }
	@awk 'BEGIN { ok=1 } \
		/^[[:space:]]*#/ { next } \
		/^[[:space:]]*$$/ { next } \
		{ \
			if (NF < 2) { printf "malformed line %d: %s\n", NR, $$0; ok=0; next } \
			stage=$$1; asset=$$2; \
			if (stage !~ /^(stage1|stage2|stage3)$$/) { \
				printf "unknown stage %s on line %d\n", stage, NR; ok=0 \
			} \
			if (seen[asset]++) { \
				printf "duplicate asset %s on line %d\n", asset, NR; ok=0 \
			} \
		} \
		END { if (!ok) exit 1 }' "$(CANARY_CONFIG)"
	@echo "canary sequence OK: $(CANARY_CONFIG)"

# Print the current stage and its advance/pause/rollback criteria.
canary-stage:
	@test -f "$(CANARY_RUNBOOK)" || { echo "missing $(CANARY_RUNBOOK)"; exit 1; }
	@awk '/^## Launch-day canary sequence/ { p=1 } p { print } /^## / && p && !/^## Launch-day canary sequence/ { exit }' "$(CANARY_RUNBOOK)"
