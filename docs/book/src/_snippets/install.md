<div class="os-tabs-src">

<!-- ANCHOR: linux -->
### Linux

**One-liner (`install.sh` via curl):**

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | sh
```

**From a clone:**

```sh
./install.sh
```

**Homebrew (Linuxbrew):**

```sh
brew install zeroclaw
```
<!-- ANCHOR_END: linux -->

<!-- ANCHOR: macos -->
### macOS

**One-liner (`install.sh` via curl):**

```sh
curl -fsSL https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/master/install.sh | sh
```

**From a clone:**

```sh
./install.sh
```

**Homebrew:**

```sh
brew install zeroclaw
```
<!-- ANCHOR_END: macos -->

<!-- ANCHOR: windows -->
### Windows

**`setup.bat` (from a release):**

```cmd
setup.bat
```

**Scoop:**

```cmd
scoop install zeroclaw
```

**From source:**

```cmd
cargo install --locked --path .
```

On WSL2, follow the Linux path; `install.sh` runs unchanged. See
[Setup → Windows](../setup/windows.md) for the full walkthrough.
<!-- ANCHOR_END: windows -->

</div>
