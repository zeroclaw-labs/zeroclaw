# Faster Builds with cargo-slicer

[cargo-slicer](https://github.com/nickel-org/cargo-slicer) is a `RUSTC_WRAPPER` that stubs unreachable library functions at the MIR level, skipping LLVM codegen for code the final binary never calls. It identified **2,059 unreachable functions** in ZeroClaw's workspace crates.

## Benchmark Results

| Environment | Baseline | With cargo-slicer | Wall-time savings |
|---|---|---|---|
| 48-core server (AMD EPYC) | 192.9 s | 170.4 s | **-11.7%** |
| Raspberry Pi 4 (4-core ARM) | 25m 03s | 17m 54s | **-28.6%** |
| 2-vCPU CI runner (estimated) | — | — | **~25-30%** |

All measurements are clean `cargo build --release` on nightly. Fewer cores = larger relative improvement, because each crate's compile time is a bigger fraction of total wall time. The 2-vCPU CI runners should see savings similar to the Pi.

## CI Integration

The workflow [`.github/workflows/ci-build-fast.yml`](../.github/workflows/ci-build-fast.yml) runs an accelerated release build alongside the standard one. It does not gate merges — it runs in parallel as a non-blocking check.

## Local Usage

```bash
# One-time install
cargo install cargo-slicer
rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly
cargo +nightly install cargo-slicer --profile release-rustc \
  --bin cargo-slicer-rustc --bin cargo_slicer_dispatch \
  --features rustc-driver

# Build (from zeroclaw root)
cargo-slicer pre-analyze
CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
  RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
  cargo +nightly build --release
```

## How It Works

1. **Pre-analysis** scans workspace sources via `syn` to build a cross-crate call graph (~5 s).
2. **Cross-crate BFS** from `main()` identifies which public library functions are actually reachable.
3. **MIR stubbing** replaces unreachable bodies with `Unreachable` terminators — the mono collector finds no callees and prunes entire codegen subtrees.

No source files are modified. The output binary is functionally identical.
