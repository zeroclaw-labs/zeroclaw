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
    cargo clippy --all-targets --features ci-all -- -D warnings

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
