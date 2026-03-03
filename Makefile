.PHONY: build build-release build-dist check fmt fmt-check lint lint-strict \
       test test-nextest bench clean doc help \
       ci-lint ci-test ci-build ci-all ci-shell ci-clean

CARGO := cargo
PROFILE ?= dev

# ─── Development ──────────────────────────────────────────────────────

build: ## Build (debug)
	$(CARGO) build

build-release: ## Build (release profile, size-optimized)
	$(CARGO) build --profile release

build-dist: ## Build (dist profile, maximum size optimization)
	$(CARGO) build --profile dist

check: ## Type-check without codegen (fastest feedback loop)
	$(CARGO) check --all-targets

# ─── Code quality ─────────────────────────────────────────────────────

fmt: ## Format all code in-place
	$(CARGO) fmt --all

fmt-check: ## Verify formatting without changes
	$(CARGO) fmt --all -- --check

lint: fmt-check ## Format check + clippy (correctness only)
	$(CARGO) clippy --all-targets -- -D clippy::correctness

lint-strict: fmt-check ## Format check + clippy -D warnings (CI-level)
	$(CARGO) clippy --all-targets -- -D warnings

# ─── Testing ──────────────────────────────────────────────────────────

test: ## Run all tests
	$(CARGO) test

test-nextest: ## Run tests via cargo-nextest (parallel, faster)
	$(CARGO) nextest run

bench: ## Run benchmarks
	$(CARGO) bench

# ─── Documentation ────────────────────────────────────────────────────

doc: ## Build rustdoc
	$(CARGO) doc --no-deps --document-private-items

# ─── Pre-push validation (matches CLAUDE.md §8) ──────────────────────

pre-push: fmt-check lint-strict test ## Full local validation gate
	@echo "✓ pre-push passed"

# ─── Docker CI (delegates to dev/ci.sh) ───────────────────────────────

ci-lint: ## Lint inside Docker CI container
	./dev/ci.sh lint

ci-test: ## Test inside Docker CI container
	./dev/ci.sh test

ci-build: ## Release build inside Docker CI container
	./dev/ci.sh build

ci-all: ## Full CI pipeline in Docker (lint+test+build+security+smoke)
	./dev/ci.sh all

ci-shell: ## Interactive shell in CI container
	./dev/ci.sh shell

ci-clean: ## Remove CI containers and volumes
	./dev/ci.sh clean

# ─── Housekeeping ─────────────────────────────────────────────────────

clean: ## cargo clean
	$(CARGO) clean

# ─── Help ─────────────────────────────────────────────────────────────

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*##' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*## "}; {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
