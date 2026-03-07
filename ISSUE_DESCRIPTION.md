# Bug Report: Compilation failure due to edition2024 dependency requirement

## Summary
- **Problem**: `cargo build` fails with "feature `edition2024` is required" error from cpufeatures crate
- **Impact**: Project cannot be compiled on standard Rust toolchains (tested with Cargo 1.75.0)
- **Severity**: S1 - workflow blocked (prevents basic build)

## Error Details
```
error: failed to download `cpufeatures v0.3.0`

Caused by:
  feature `edition2024` is required

  The package requires the Cargo feature called `edition2024`, but that feature is not stabilized in this version of Cargo (1.75.0).
  Consider trying a more recent nightly release.
```

## Environment
- Rust version: 1.75.0
- Operating system: Linux
- ZeroClaw version: main branch (latest)

## Root Cause Analysis
The project's Cargo.toml specifies `rust-version = "1.87"` which is very new, but a transitive dependency (cpufeatures v0.3.0) requires the unstabilized `edition2024` feature. This creates a mismatch between the declared MSRV and actual buildability.

## Impact
- New contributors cannot build the project
- CI/CD pipelines using older Rust versions will fail
- Docker images based on older Rust versions won't work
- This affects project adoption and contribution velocity

## Proposed Solution
1. Pin cpufeatures to a compatible version (v0.2.x) that doesn't require edition2024
2. Add explicit version constraints in Cargo.toml
3. Update CI to test against the declared MSRV (1.87) to catch such issues
4. Consider lowering MSRV to a more widely supported version if possible