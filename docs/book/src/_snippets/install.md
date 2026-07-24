<div class="os-tabs-src">

<!-- ANCHOR: linux -->
### Linux

**Piped noninteractive install (`install.sh` via curl):**

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | sh
```

The piped path chooses a prebuilt binary when one is available and falls back to a source build otherwise. It skips the setup prompt and prints `zeroclaw quickstart` as the next step.

**Guided install from a clone:**

```sh
./install.sh
```

When the platform maps to a supported prebuilt target, run this path in an interactive terminal to choose prebuilt or source; other platforms build from source. The source path also lets you select apps and optional features. For an unconfigured install, the installer then offers CLI or browser-based setup.

**Homebrew (Linuxbrew):**

```sh
brew install zeroclaw
```
<!-- ANCHOR_END: linux -->

<!-- ANCHOR: macos -->
### macOS

**Piped noninteractive install (`install.sh` via curl):**

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | sh
```

The piped path chooses a prebuilt binary when one is available and falls back to a source build otherwise. It skips the setup prompt and prints `zeroclaw quickstart` as the next step.

**Guided install from a clone:**

```sh
./install.sh
```

When the platform maps to a supported prebuilt target, run this path in an interactive terminal to choose prebuilt or source; other platforms build from source. The source path also lets you select apps and optional features. For an unconfigured install, the installer then offers CLI or browser-based setup.

**Homebrew:**

```sh
brew install zeroclaw
```
<!-- ANCHOR_END: macos -->

<!-- ANCHOR: windows -->
### Windows

**Prebuilt binary (recommended):**

Download the latest `zeroclaw-x86_64-pc-windows-msvc.zip`, extract it, add its directory to `PATH`, and run `zeroclaw quickstart`. The [Windows setup guide](../setup/windows.md) provides an idempotent PowerShell installation block.

**`setup.bat` (currently checks for Rust before its prebuilt path):**

```cmd
setup.bat --prebuilt
```

**Scoop:**

```cmd
scoop bucket add zeroclaw https://github.com/zeroclaw-labs/scoop-zeroclaw
scoop install zeroclaw
```

**From source:**

```cmd
cargo install --locked --path .
```

On WSL2, follow the Linux path; `install.sh` runs unchanged. See
[Setup → Windows](../setup/windows.md) for the full walkthrough and current Scoop status.
<!-- ANCHOR_END: windows -->

</div>
