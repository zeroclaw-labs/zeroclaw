# Justfile - Convenient command runner for ZeroClaw development
# https://github.com/casey/just

# Default recipe to display help
_default:
    @just --list

# Format all code
fmt:
    cargo fmt --all

# Check formatting without making changes
fmt-check:
    cargo fmt --all -- --check

# Run clippy lints
lint:
    cargo clippy --all-targets -- -D warnings

# Run all tests
test:
    cargo test --locked

# Run only unit tests (faster)
test-lib:
    cargo test --lib

# Run the full CI quality gate locally
ci: fmt-check lint test
    @echo "✅ All CI checks passed!"

# Build in release mode
build:
    cargo build --release --locked

# Build in debug mode
build-debug:
    cargo build

# Clean build artifacts
clean:
    cargo clean

# Run zeroclaw with example config (for development)
dev *ARGS:
    cargo run -- {{ARGS}}

# Check code without building
check:
    cargo check --all-targets

# Run cargo doc and open in browser
doc:
    cargo doc --no-deps --open

# Serve the docs site locally (English by default; pass LOCALE=ja for Japanese)
docs LOCALE="en":
    ./scripts/docs.sh serve --locale {{LOCALE}}

# Build the full docs site (all locales) to docs/book/book/
docs-build:
    ./scripts/docs.sh build

# Regenerate reference/cli.md and reference/config.md from code
docs-refs:
    ./scripts/docs.sh refs

# Sync .po files with English source; AI-fills delta if ANTHROPIC_API_KEY is set
docs-sync:
    ./scripts/sync-translations.sh

# Sync a single locale (e.g.: just docs-sync-locale ja)
docs-sync-locale LOCALE:
    ./scripts/sync-translations.sh --locale {{LOCALE}}

# Force-retranslate everything for a quality pass (costs more — use before a release)
# Optionally override model: FILL_MODEL=claude-opus-4-7 just docs-translate-force
docs-translate-force:
    ./scripts/sync-translations.sh --force

# Force-retranslate a single locale
docs-translate-force-locale LOCALE:
    ./scripts/sync-translations.sh --locale {{LOCALE}} --force

# Show translation status: translated/fuzzy/untranslated counts per locale
docs-translate-stats:
    @for po in docs/book/po/*.po; do \
        locale=$$(basename "$$po" .po); \
        printf "%-8s " "$$locale"; \
        LANG=C msgfmt --statistics "$$po" -o /dev/null; \
    done

# Validate .po format for all locales (exits non-zero on format errors)
docs-translate-check:
    @ok=0; \
    for po in docs/book/po/*.po; do \
        msgfmt --check-format "$$po" -o /dev/null || ok=1; \
    done; \
    exit $$ok

# Update dependencies
update:
    cargo update

# Run cargo audit to check for security vulnerabilities
audit:
    cargo audit

# Run cargo deny checks
deny:
    cargo deny check

# Format TOML files (requires taplo)
fmt-toml:
    taplo format

# Check TOML formatting (requires taplo)
fmt-toml-check:
    taplo format --check

# Run all formatting tools
fmt-all: fmt fmt-toml
    @echo "✅ All formatting complete!"
