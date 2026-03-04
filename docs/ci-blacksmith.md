# Blacksmith Production Build Pipeline

This document describes the production binary build lane for ZeroClaw on Blacksmith-backed GitHub Actions runners.

## Workflow

- File: `.github/workflows/release-build.yml`
- Workflow name: `Production Release Build`
- Triggers:
    - Push to `main`
    - Push tags matching `v*`
    - Manual dispatch (`workflow_dispatch`)

## Runner Labels

The workflow runs on the same Blacksmith self-hosted runner label-set used by the rest of CI:

`[self-hosted, Linux, X64, aws-india, blacksmith-2vcpu-ubuntu-2404, hetzner]`

This keeps runner routing consistent with existing CI jobs and actionlint policy.

## Canonical Commands

Quality gates (must pass before release build):

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --verbose
```

Production build command (canonical):

```bash
cargo build --release --locked
```

## Artifact Output

- Binary path: `target/release/zeroclaw`
- Uploaded artifact name: `zeroclaw-linux-amd64`
- Uploaded files:
    - `artifacts/zeroclaw`
    - `artifacts/zeroclaw.sha256`

## Re-run and Debug

1. Open Actions run for `Production Release Build`.
2. Use `Re-run failed jobs` (or full rerun) from the run page.
3. Inspect step logs in this order: `Rust quality gates` -> `Build production binary (canonical)` -> `Prepare artifact bundle`.
4. Download `zeroclaw-linux-amd64` from the run artifacts and verify checksum:

```bash
sha256sum -c zeroclaw.sha256
```

5. Reproduce locally from repository root with the same command set:

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --verbose
cargo build --release --locked
```
