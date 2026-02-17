# ESP32 Firmware Setup Guide

Step-by-step setup for building the ZeroClaw ESP32 firmware. Follow this if you run into issues.

## Quick Start (copy-paste)

```sh
# 1. Install Python 3.12 (ESP-IDF needs 3.10–3.13, not 3.14)
brew install python@3.12

# 2. Install virtualenv (PEP 668 workaround on macOS)
/opt/homebrew/opt/python@3.12/bin/python3.12 -m pip install virtualenv --break-system-packages

# 3. Install Rust tools
cargo install espflash ldproxy

# 4. Build
cd firmware/zeroclaw-esp32
export PATH="/opt/homebrew/opt/python@3.12/libexec/bin:$PATH"
cargo build --release

# 5. Flash (connect ESP32 via USB)
espflash flash target/riscv32imc-esp-espidf/release/zeroclaw-esp32 --monitor
```

---

## Detailed Steps

### 1. Python

ESP-IDF requires Python 3.10–3.13. **Python 3.14 is not supported.**

```sh
brew install python@3.12
```

### 2. virtualenv

ESP-IDF tools need `virtualenv`. On macOS with Homebrew Python, PEP 668 blocks `pip install`; use:

```sh
/opt/homebrew/opt/python@3.12/bin/python3.12 -m pip install virtualenv --break-system-packages
```

### 3. Rust Tools

```sh
cargo install espflash ldproxy
```

- **espflash**: flash and monitor
- **ldproxy**: linker for ESP-IDF builds

### 4. Use Python 3.12 for Builds

Before every build (or add to `~/.zshrc`):

```sh
export PATH="/opt/homebrew/opt/python@3.12/libexec/bin:$PATH"
```

### 5. Build

```sh
cd firmware/zeroclaw-esp32
cargo build --release
```

First build downloads and compiles ESP-IDF (~5–15 min).

### 6. Flash

```sh
espflash flash target/riscv32imc-esp-espidf/release/zeroclaw-esp32 --monitor
```

---

## Troubleshooting

### "No space left on device"

Free disk space. Common targets:

```sh
# Cargo cache (often 5–20 GB)
rm -rf ~/.cargo/registry/cache ~/.cargo/registry/src

# Unused Rust toolchains
rustup toolchain list
rustup toolchain uninstall <name>

# iOS Simulator runtimes (~35 GB)
xcrun simctl delete unavailable

# Temp files
rm -rf /var/folders/*/T/cargo-install*
```

### "can't find crate for `core`" / "riscv32imc-esp-espidf target may not be installed"

This project uses **nightly Rust with build-std**, not espup. Ensure:

- `rust-toolchain.toml` exists (pins nightly + rust-src)
- You are **not** sourcing `~/export-esp.sh` (that's for Xtensa targets)
- Run `cargo build` from `firmware/zeroclaw-esp32`

### "externally-managed-environment" / "No module named 'virtualenv'"

Install virtualenv with the PEP 668 workaround:

```sh
/opt/homebrew/opt/python@3.12/bin/python3.12 -m pip install virtualenv --break-system-packages
```

### "expected `i64`, found `i32`" (time_t mismatch)

Already fixed in `.cargo/config.toml` with `espidf_time64` for ESP-IDF 5.x. If you use ESP-IDF 4.4, switch to `espidf_time32`.

### "expected `*const u8`, found `*const i8`" (esp-idf-svc)

Already fixed via `[patch.crates-io]` in `Cargo.toml` using esp-rs crates from git. Do not remove the patch.

### 10,000+ files in `git status`

The `.embuild/` directory (ESP-IDF cache) has ~100k+ files. It is in `.gitignore`. If you see them, ensure `.gitignore` contains:

```
.embuild/
```

---

## Optional: Auto-load Python 3.12

Add to `~/.zshrc`:

```sh
# ESP32 firmware build
export PATH="/opt/homebrew/opt/python@3.12/libexec/bin:$PATH"
```

---

## Xtensa Targets (ESP32, ESP32-S2, ESP32-S3)

For non–RISC-V chips, use espup instead:

```sh
cargo install espup espflash
espup install
source ~/export-esp.sh
```

Then edit `.cargo/config.toml` to use `xtensa-esp32-espidf` (or the correct target).
