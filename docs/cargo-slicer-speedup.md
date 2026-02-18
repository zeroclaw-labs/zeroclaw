# Faster Builds with cargo-slicer

[cargo-slicer](https://github.com/nickel-org/cargo-slicer) is an `RUSTC_WRAPPER` that performs whole-program reachability analysis, then stubs unreachable functions at the MIR level. This avoids LLVM codegen for library code that the final binary never calls.

ZeroClaw's workspace has large dependency surfaces — cargo-slicer identified **2,059 unreachable functions** across the two library crates, delivering significant build-time savings.

## Benchmark Results

| Environment | Baseline | With cargo-slicer | Wall-time savings |
|---|---|---|---|
| 48-core server (AMD EPYC) | 192.9 s | 170.4 s | **-11.7%** |
| Raspberry Pi 4 (4-core ARM) | 25m 03s | 17m 54s | **-28.6%** |

All measurements are clean `cargo build --release` on nightly. The Pi 4 sees a larger relative improvement because, with fewer cores, each crate's compile time is a bigger fraction of the total wall time.

### Detailed metrics (48-core server)

| Metric | Baseline | vslice-cc | Delta |
|---|---|---|---|
| Wall time | 192.9 s | 170.4 s | **-11.7%** |
| CPU instructions | 1,507 B | 1,314 B | **-12.8%** |
| Functions stubbed | — | 2,059 | — |

## Setup (3 steps)

### 1. Install cargo-slicer

```bash
# Install the CLI tool
cargo install cargo-slicer

# Install the rustc driver (requires nightly)
rustup component add rust-src rustc-dev llvm-tools-preview --toolchain nightly
cargo +nightly install cargo-slicer --profile release-rustc \
  --bin cargo-slicer-rustc --bin cargo_slicer_dispatch \
  --features rustc-driver
```

### 2. Pre-analyze the workspace

```bash
cd /path/to/zeroclaw
cargo-slicer pre-analyze
```

This runs a lightweight syn-based analysis (~5 seconds) that builds a cross-crate call graph and identifies unreachable functions.

### 3. Build with virtual slicing

```bash
CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
  RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
  cargo +nightly build --release
```

The first build produces the same binary as a normal build — only unreachable function bodies are replaced with abort stubs that the linker discards. Subsequent builds with the same `.slicer-cache/` are even faster.

## CI Integration

Add an optional job to your GitHub Actions workflow:

```yaml
  build-optimized:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@nightly
        with:
          components: rust-src, rustc-dev, llvm-tools-preview
      - name: Install cargo-slicer
        run: |
          cargo install cargo-slicer
          cargo +nightly install cargo-slicer --profile release-rustc \
            --bin cargo-slicer-rustc --bin cargo_slicer_dispatch \
            --features rustc-driver
      - name: Build with virtual slicing
        run: |
          cargo-slicer pre-analyze
          CARGO_SLICER_VIRTUAL=1 CARGO_SLICER_CODEGEN_FILTER=1 \
            RUSTC_WRAPPER=$(which cargo_slicer_dispatch) \
            cargo +nightly build --release
```

## How It Works

Rust's monomorphization collector already prunes unreachable code per-crate, but it must conservatively assume all public functions are roots. cargo-slicer bridges this with a three-step pipeline:

1. **Pre-analysis**: Scans workspace sources via `syn` to build a unified cross-crate call graph.
2. **Cross-crate BFS**: Walks from `main()` to determine which public library functions are actually reachable.
3. **MIR stubbing**: Replaces unreachable function bodies with `Unreachable` terminators inside the compiler. The mono collector finds no callees in stubs, pruning entire subtrees.

No source files are modified. The output binary is functionally identical — only codegen for dead code is skipped.
