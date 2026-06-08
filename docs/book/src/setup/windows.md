# Windows

Install, update, run as a scheduled task / Windows Service, and uninstall on Windows 10 / 11.

`setup.bat` is the Windows counterpart to `install.sh`, same job, different shell.

## Install

```cmd
setup.bat
```

That is the whole install: grab `setup.bat` from a ZeroClaw release and run it. It prompts for a build mode, then either downloads the prebuilt binary or (for source modes) installs a stable Rust toolchain via `rustup` and compiles. Either way the binary lands at `%USERPROFILE%\.zeroclaw\bin\zeroclaw.exe`, and it points you at [`zeroclaw quickstart`](../getting-started/quickstart.md).

To skip the interactive prompt, pass a build-mode flag — `--prebuilt` (download a release binary, no toolchain), `--minimal` (core only), `--standard`, or `--full`. Run `setup.bat --help` for the authoritative list of modes and the exact feature set each one compiles; that output is generated from the script itself, so it never drifts. With `--minimal`, quickstart is unavailable; configure `%USERPROFILE%\.zeroclaw\config.toml` manually and use the reduced CLI path (`zeroclaw agent ...`).

### Scoop

<div class="os-tabs-src">

#### cmd

```cmd
scoop install zeroclaw
```

</div>

### From source

Requires Rust (`rustup`) and Visual Studio Build Tools:

<div class="os-tabs-src">

#### cmd

```cmd
git clone https://github.com/zeroclaw-labs/zeroclaw
cd zeroclaw
cargo install --locked --path .
```

</div>

If you're running WSL2, follow the [Linux setup](./linux.md) instead; `install.sh` runs unchanged under WSL.

## System dependencies

Windows builds use the MSVC toolchain. You need:

- Visual Studio Build Tools (or full Visual Studio) with the "Desktop development with C++" workload
- Rust stable (via `rustup`)

If you're using `--prebuilt` you don't need the Rust toolchain; the binary is self-contained.

## Running as a service

Windows has two options: a scheduled task (user session) or a Windows Service (system session).

### Scheduled task (recommended for single-user machines)

<div class="os-tabs-src">

#### cmd

```cmd
zeroclaw service install
zeroclaw service start
```

</div>

This creates a task that runs under your user account and starts on login. Managed via Task Scheduler (`taskschd.msc`).

Logs go to `%LOCALAPPDATA%\ZeroClaw\logs\`.

### Windows Service (for server installs)

Running as a true service requires Administrator privileges during install. Open an elevated `cmd.exe` and:

<div class="os-tabs-src">

#### cmd

```cmd
zeroclaw service install
```

</div>

When run elevated, the installer registers a Windows Service under `LocalSystem` instead of a user-scoped scheduled task. Consider creating a dedicated service account if the agent touches user-scoped resources.

Full details: [Service management](./service.md).

## Update

### From `setup.bat` / release zip

Re-download the latest release and re-run `setup.bat --prebuilt` (or whichever flag you used originally). Then:

<div class="os-tabs-src">

#### cmd

```cmd
zeroclaw service restart
```

</div>

### Scoop

<div class="os-tabs-src">

#### cmd

```cmd
scoop update zeroclaw
zeroclaw service restart
```

</div>

### From source

<div class="os-tabs-src">

#### cmd

```cmd
cd C:\path\to\zeroclaw
git pull
cargo install --locked --path . --force
zeroclaw service restart
```

</div>

## Uninstall

Stop and remove the service:

<div class="os-tabs-src">

#### cmd

```cmd
zeroclaw service stop
zeroclaw service uninstall
```

</div>

Remove the binary:

<div class="os-tabs-src">

#### cmd

```cmd
:: setup.bat
del "%USERPROFILE%\.zeroclaw\bin\zeroclaw.exe"

:: cargo install
del "%USERPROFILE%\.cargo\bin\zeroclaw.exe"

:: Scoop
scoop uninstall zeroclaw
```

</div>

Remove config and workspace (optional, this deletes conversation history):

<div class="os-tabs-src">

#### cmd

```cmd
rmdir /s /q "%USERPROFILE%\.zeroclaw"
rmdir /s /q "%LOCALAPPDATA%\ZeroClaw"
```

</div>

## Gotchas

- **Long paths.** Some Windows file systems still cap path lengths at 260 characters. Enable long path support if you hit `path too long` errors during build (`reg add HKLM\SYSTEM\CurrentControlSet\Control\FileSystem /v LongPathsEnabled /t REG_DWORD /d 1 /f`).
- **SmartScreen.** The unsigned binary may trip SmartScreen on first launch. Right-click → Properties → "Unblock" is the standard workaround until we add a signed MSI.
- **Task Scheduler stop-at-idle.** By default Windows may terminate scheduled tasks on idle / battery. The installed task explicitly disables these conditions; verify under Task Scheduler → ZeroClaw → Properties → Conditions.

## Next

- [Service management](./service.md)
- [Quickstart](../getting-started/quickstart.md)
- [Operations → Overview](../ops/overview.md)
