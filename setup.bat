@echo off
setlocal enabledelayedexpansion

:: ============================================================================
:: ZeroClaw Windows Setup Script
:: Simplifies building and installing ZeroClaw on Windows.
:: Usage: setup.bat [--prebuilt | --minimal | --dist | --default | --all | --dry-run | --help]
:: ============================================================================

set "VERSION=0.8.0"
set "RUST_MIN_VERSION=1.87"
set "TARGET=x86_64-pc-windows-msvc"
set "REPO=https://github.com/zeroclaw-labs/zeroclaw"

:: Colors via ANSI (Windows 10+ Terminal)
set "GREEN=[32m"
set "YELLOW=[33m"
set "RED=[31m"
set "BLUE=[34m"
set "BOLD=[1m"
set "RESET=[0m"

:: Parse arguments
set "MODE=interactive"
set "DRY_RUN=false"
:parse_args
if "%~1"==""           goto :start
if "%~1"=="--help"     goto :show_help
if "%~1"=="-h"         goto :show_help
if "%~1"=="--dry-run"  set "DRY_RUN=true" & shift & goto :parse_args
if "%~1"=="--prebuilt" set "MODE=prebuilt" & shift & goto :parse_args
if "%~1"=="--minimal"  set "MODE=minimal"  & shift & goto :parse_args
if "%~1"=="--dist"     set "MODE=dist"     & shift & goto :parse_args
if "%~1"=="--default"  set "MODE=default"  & shift & goto :parse_args
if "%~1"=="--all"      set "MODE=all"      & shift & goto :parse_args
echo Unknown option: %~1
goto :show_help

:start
echo.
echo %BOLD%%BLUE%=========================================%RESET%
echo %BOLD%%BLUE%  ZeroClaw Windows Setup  v%VERSION%%RESET%
echo %BOLD%%BLUE%=========================================%RESET%
echo.

:: ---- Step 1: Check prerequisites ----
echo %BOLD%[1/5] Checking prerequisites...%RESET%

:: Check available RAM (rough estimate via wmic)
for /f "tokens=2 delims==" %%a in ('wmic os get FreePhysicalMemory /value 2^>nul ^| find "="') do (
    set /a "FREE_RAM_MB=%%a / 1024"
)
if defined FREE_RAM_MB (
    if !FREE_RAM_MB! LSS 2048 (
        echo   %YELLOW%WARNING: Only !FREE_RAM_MB! MB free RAM detected. 2048 MB recommended for source builds.%RESET%
        echo   %YELLOW%Consider using --prebuilt instead.%RESET%
    ) else (
        echo   %GREEN%OK%RESET% Free RAM: !FREE_RAM_MB! MB
    )
)

:: Check disk space
for /f %%a in ('powershell -Command "[math]::Round((Get-PSDrive $env:SystemDrive).Free / 1GB)"') do (
    set "FREE_DISK_GB=%%a"
)

:: Check Rust
where cargo >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo   %YELLOW%Rust not found.%RESET%
    if "%DRY_RUN%"=="true" (
        echo   [dry-run] Would install Rust via rustup
    ) else (
        goto :install_rust
    )
) else (
    for /f "tokens=2" %%v in ('rustc --version 2^>nul') do set "RUST_VER=%%v"
    echo   %GREEN%OK%RESET% Rust !RUST_VER! found
)

:: Check Node.js (optional)
where node >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo   %YELLOW%Node.js not found - optional, web dashboard will use stub%RESET%
) else (
    for /f "tokens=1" %%v in ('node --version 2^>nul') do set "NODE_VER=%%v"
    echo   %GREEN%OK%RESET% Node.js !NODE_VER! found
)

:: Check Git
where git >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo   %RED%ERROR: Git is required but not found.%RESET%
    echo   Install Git from https://git-scm.com/download/win
    goto :error_exit
) else (
    echo   %GREEN%OK%RESET% Git found
)

goto :choose_mode

:: ---- Install Rust ----
:install_rust
echo.
echo %BOLD%Installing Rust...%RESET%
echo   Downloading rustup-init.exe...

:: Download rustup-init.exe
curl -sSfL -o "%TEMP%\rustup-init.exe" https://win.rustup.rs
if %ERRORLEVEL% NEQ 0 (
    echo   %RED%ERROR: Failed to download rustup-init.exe%RESET%
    echo   Please install Rust manually from https://rustup.rs
    goto :error_exit
)

:: Run rustup-init with defaults
"%TEMP%\rustup-init.exe" -y --default-toolchain stable --target %TARGET%
if %ERRORLEVEL% NEQ 0 (
    echo   %RED%ERROR: Rust installation failed.%RESET%
    goto :error_exit
)

:: Refresh PATH
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"
echo   %GREEN%OK%RESET% Rust installed successfully.
echo   %YELLOW%NOTE: You may need to restart your terminal for PATH changes.%RESET%
goto :choose_mode

:: ---- Choose build mode ----
:: >>> generated:menu by `cargo generate installers` - do not edit <<<
:choose_mode
if "%MODE%"=="prebuilt" goto :install_prebuilt
if "%MODE%"=="minimal" goto :build_minimal
if "%MODE%"=="dist" goto :build_dist
if "%MODE%"=="default" goto :build_default
if "%MODE%"=="all" goto :build_all

echo %BOLD%[2/5] Choose installation method:%RESET%
echo.
echo   1) Prebuilt binary - Download pre-compiled release (fastest)
echo   2) minimal build - core only, no default features
echo   3) dist build - all channels, no heavyweight extras (recommended)
echo   4) default build - default feature set
echo   5) all build - every feature including hardware and browser
echo.
set /p "CHOICE=  Select [1-5] (default: 1): "
if "%CHOICE%"=="" set "CHOICE=1"
if "%CHOICE%"=="1" goto :install_prebuilt
if "%CHOICE%"=="2" goto :build_minimal
if "%CHOICE%"=="3" goto :build_dist
if "%CHOICE%"=="4" goto :build_default
if "%CHOICE%"=="5" goto :build_all
echo   %RED%Invalid choice. Please enter 1-5.%RESET%
goto :choose_mode
:: >>> end generated:menu <<<

:: ---- Prebuilt binary ----
:install_prebuilt
echo.
echo %BOLD%[3/5] Downloading prebuilt binary...%RESET%

if "%DRY_RUN%"=="true" (
    echo   [dry-run] Would download the prebuilt Windows release archive
    echo   [dry-run] Would install to %USERPROFILE%\.zeroclaw\bin
    echo   [dry-run] Would add %USERPROFILE%\.zeroclaw\bin to PATH
    goto :dry_run_done
)

:: Try to get latest release URL via gh or curl
where gh >nul 2>&1
if %ERRORLEVEL% EQU 0 (
    for /f "tokens=*" %%u in ('gh release view --repo %REPO% --json assets --jq ".assets[] | select(.name | test(\"windows-msvc\")) | .url" 2^>nul') do (
        set "DOWNLOAD_URL=%%u"
    )
)

if not defined DOWNLOAD_URL (
    :: Fallback: construct URL from known release pattern
    set "DOWNLOAD_URL=https://github.com/zeroclaw-labs/zeroclaw/releases/latest/download/zeroclaw-%TARGET%.zip"
)

echo   Downloading from release...
curl -sSfL -o "%TEMP%\zeroclaw-windows.zip" "!DOWNLOAD_URL!"
if %ERRORLEVEL% NEQ 0 (
    echo   %YELLOW%Prebuilt binary not available. Falling back to source build - dist%RESET%
    goto :build_dist
)

:: Extract
echo   Extracting...
mkdir "%USERPROFILE%\.zeroclaw\bin" 2>nul
tar -xf "%TEMP%\zeroclaw-windows.zip" -C "%USERPROFILE%\.zeroclaw\bin"
if %ERRORLEVEL% NEQ 0 (
    powershell -Command "Expand-Archive -Force '%TEMP%\zeroclaw-windows.zip' '%USERPROFILE%\.zeroclaw\bin'"
)

:: Add to PATH if not already there
echo %PATH% | findstr /I /C:".zeroclaw\bin" >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    setx PATH "%PATH%;%USERPROFILE%\.zeroclaw\bin" >nul 2>&1
    set "PATH=%PATH%;%USERPROFILE%\.zeroclaw\bin"
    echo   %GREEN%OK%RESET% Added to PATH
)

echo   %GREEN%OK%RESET% Binary installed to %USERPROFILE%\.zeroclaw\bin\zeroclaw.exe
if exist "%USERPROFILE%\.zeroclaw\bin\zerocode.exe" (
    echo   %GREEN%OK%RESET% TUI installed to %USERPROFILE%\.zeroclaw\bin\zerocode.exe
)
goto verify

:: ---- Source build presets ----
:: >>> generated:presets by `cargo generate installers` - do not edit <<<
:build_minimal
set "FEATURES=--no-default-features"
set "BUILD_DESC=minimal (core only, no default features)"
goto :do_build

:build_dist
set "FEATURES=--no-default-features --features acp-bridge,agent-runtime,channel-acp-server,channel-amqp,channel-bluesky,channel-clawdtalk,channel-dingtalk,channel-discord,channel-email,channel-imessage,channel-irc,channel-lark,channel-linq,channel-mattermost,channel-mochat,channel-mqtt,channel-nextcloud,channel-notion,channel-qq,channel-reddit,channel-signal,channel-slack,channel-telegram,channel-twitch,channel-twitter,channel-voice-call,channel-wati,channel-webhook,channel-wecom,channel-wecom-ws,channel-whatsapp-cloud,gateway,observability-prometheus,schema-export"
set "BUILD_DESC=dist (all channels, no heavyweight extras (recommended))"
goto :do_build

:build_default
set "FEATURES="
set "BUILD_DESC=default (default feature set)"
goto :do_build

:build_all
set "FEATURES=--no-default-features --features acp-bridge,agent-runtime,browser-native,channel-acp-server,channel-amqp,channel-bluesky,channel-clawdtalk,channel-dingtalk,channel-discord,channel-email,channel-feishu,channel-imessage,channel-irc,channel-lark,channel-line,channel-linq,channel-matrix,channel-mattermost,channel-mochat,channel-mqtt,channel-nextcloud,channel-nostr,channel-notion,channel-qq,channel-reddit,channel-signal,channel-slack,channel-telegram,channel-twitch,channel-twitter,channel-voice-call,channel-wati,channel-webhook,channel-wechat,channel-wecom,channel-wecom-ws,channel-whatsapp-cloud,dev-sim,gateway,hardware,memory-postgres,observability-otel,observability-prometheus,peripheral-rpi,plugins-wasm,probe,rag-pdf,sandbox-bubblewrap,sandbox-landlock,schema-export,webauthn,whatsapp-web"
set "BUILD_DESC=all (every feature including hardware and browser)"
goto :do_build
:: >>> end generated:presets <<<

:: ---- Build from source ----
:do_build
echo.
echo %BOLD%[3/5] Building ZeroClaw (%BUILD_DESC%)...%RESET%
echo   Target: %TARGET%

if "%DRY_RUN%"=="true" (
    echo   [dry-run] Would run: cargo build --release --locked %FEATURES% --target %TARGET%
    echo   [dry-run] Would run: cargo build --release --locked -p zerocode --target %TARGET%
    echo   [dry-run] Would install to %USERPROFILE%\.zeroclaw\bin
    echo   [dry-run] Would build web dashboard ^(cargo web build^) and install to %LOCALAPPDATA%\zeroclaw\web\dist
    echo   [dry-run] Would add %USERPROFILE%\.zeroclaw\bin to PATH
    goto :dry_run_done
)

:: Ensure we're in the repo root (check for Cargo.toml)
if not exist "Cargo.toml" (
    echo   %RED%ERROR: Cargo.toml not found. Run this script from the zeroclaw repository root.%RESET%
    echo   Example:
    echo     git clone %REPO%
    echo     cd zeroclaw
    echo     setup.bat
    goto :error_exit
)

:: Add target if missing
rustup target add %TARGET% >nul 2>&1

echo   This may take 15-30 minutes on first build...
echo.

echo   Command: cargo build --release --locked %FEATURES% --target %TARGET%
cargo build --release --locked %FEATURES% --target %TARGET%
if %ERRORLEVEL% NEQ 0 (
    echo.
    echo   %RED%ERROR: Build failed.%RESET%
    echo   Common fixes:
    echo   - Ensure Visual Studio Build Tools are installed - C++ workload
    echo   - Run: rustup update
    echo   - Check disk space - 6 GB needed
    goto :error_exit
)

echo   %GREEN%OK%RESET% Build succeeded.

echo   Command: cargo build --release --locked -p zerocode --target %TARGET%
cargo build --release --locked -p zerocode --target %TARGET%
if %ERRORLEVEL% NEQ 0 (
    echo.
    echo   %RED%ERROR: zerocode TUI build failed.%RESET%
    echo   zerocode ships with every install; a partial install is not produced.
    echo   Fix the build error above and re-run setup.bat.
    goto :error_exit
)

:: Copy binary to a convenient location
echo.
echo %BOLD%[4/5] Installing binary...%RESET%
mkdir "%USERPROFILE%\.zeroclaw\bin" 2>nul
copy /Y "target\%TARGET%\release\zeroclaw.exe" "%USERPROFILE%\.zeroclaw\bin\zeroclaw.exe" >nul
if exist "target\%TARGET%\release\zerocode.exe" (
    copy /Y "target\%TARGET%\release\zerocode.exe" "%USERPROFILE%\.zeroclaw\bin\zerocode.exe" >nul
    echo   %GREEN%OK%RESET% TUI installed to %USERPROFILE%\.zeroclaw\bin\zerocode.exe
)
set "BIN_PATH=%USERPROFILE%\.zeroclaw\bin\zeroclaw.exe"
for /f %%S in ('powershell -NoProfile -Command "[math]::Round(((Get-Item -LiteralPath ''%BIN_PATH%'').Length / 1MB), 2)"') do (
    set "BINARY_MB=%%S"
)
if defined BINARY_MB (
    echo   %GREEN%OK%RESET% Installed to %USERPROFILE%\.zeroclaw\bin\zeroclaw.exe ^(%BINARY_MB% MB^)
) else (
    echo   %GREEN%OK%RESET% Installed to %USERPROFILE%\.zeroclaw\bin\zeroclaw.exe ^(size unavailable^)
)

:: Add to PATH if not already there
echo %PATH% | findstr /I /C:".zeroclaw\bin" >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    setx PATH "%PATH%;%USERPROFILE%\.zeroclaw\bin" >nul 2>&1
    set "PATH=%PATH%;%USERPROFILE%\.zeroclaw\bin"
    echo   %GREEN%OK%RESET% Added to PATH
)

:: Build and install the web dashboard so the gateway serves it. Mirrors
:: install.sh: assets must land where the gateway auto-detects them
:: (%LOCALAPPDATA%\zeroclaw\web\dist) so a service-launched daemon finds
:: them regardless of working directory.
where npm >nul 2>&1
if %ERRORLEVEL% EQU 0 (
    echo   Building web dashboard ^(cargo web build^)...
    cargo web build
    if %ERRORLEVEL% EQU 0 (
        if exist "web\dist\index.html" (
            mkdir "%LOCALAPPDATA%\zeroclaw\web\dist" 2>nul
            xcopy /E /I /Y "web\dist" "%LOCALAPPDATA%\zeroclaw\web\dist" >nul
            echo   %GREEN%OK%RESET% Web dashboard installed to %LOCALAPPDATA%\zeroclaw\web\dist
        )
    ) else (
        echo   %YELLOW%WARNING: dashboard build failed; gateway runs in API-only mode.%RESET%
        echo   %YELLOW%Re-run setup.bat once the build issue is resolved.%RESET%
    )
) else (
    echo   %YELLOW%npm not found - skipping dashboard build. The gateway will run%RESET%
    echo   %YELLOW%in API-only mode. Install Node.js and re-run setup.bat to build%RESET%
    echo   %YELLOW%and install the dashboard.%RESET%
)

goto verify

:: ---- Post install ----
:verify
echo.
echo %BOLD%[5/5] Verifying installation...%RESET%

"%USERPROFILE%\.zeroclaw\bin\zeroclaw.exe" --version >nul 2>&1
if %ERRORLEVEL% EQU 0 (
    for /f "tokens=*" %%v in ('"%USERPROFILE%\.zeroclaw\bin\zeroclaw.exe" --version 2^>nul') do (
        echo   %GREEN%OK%RESET% %%v
    )
) else (
    zeroclaw --version >nul 2>&1
    if %ERRORLEVEL% EQU 0 (
        for /f "tokens=*" %%v in ('zeroclaw --version 2^>nul') do (
            echo   %GREEN%OK%RESET% %%v
        )
    ) else (
        echo   %YELLOW%Binary installed but not on PATH yet. Restart your terminal.%RESET%
    )
)

echo.
echo %BOLD%%GREEN%=========================================%RESET%
echo %BOLD%%GREEN%  ZeroClaw setup complete!%RESET%
echo %BOLD%%GREEN%=========================================%RESET%
echo.
echo   Next steps:
echo     1. Restart your terminal (for PATH changes)
if /I "%MODE%"=="minimal" (
echo     2. Minimal build excludes quickstart ^(zeroclaw quickstart is unavailable^)
echo     3. Configure model providers manually in %%USERPROFILE%%\.zeroclaw\config.toml
echo     4. Use reduced CLI path: zeroclaw agent --message "Hello"
) else (
echo     2. Run: zeroclaw quickstart
echo     3. Configure your API key in %%USERPROFILE%%\.zeroclaw\config.toml
echo     4. Launch the TUI: zerocode
)
echo.
echo   Alternative install via Scoop:
echo     scoop bucket add zeroclaw https://github.com/zeroclaw-labs/scoop-zeroclaw
echo     scoop install zeroclaw
echo.
echo   Documentation: https://github.com/zeroclaw-labs/zeroclaw
echo.
goto :end

:: ---- Help ----
:show_help
echo.
echo ZeroClaw Windows Setup Script
echo.
echo Usage: setup.bat [OPTIONS]
echo.
echo Options:
echo   --prebuilt    Download pre-compiled binary (fastest)
echo   --minimal     Build core only ^(--no-default-features^)
echo   --dist        Build all channels, no heavyweight extras (recommended)
echo   --default     Build the default feature set
echo   --all         Build every feature including hardware and browser
echo   --dry-run     Show what would happen without building or installing
echo   --help, -h    Show this help message
echo.
echo Without arguments, runs in interactive mode.
echo.
echo Prerequisites:
echo   - Git (required)
echo   - Rust 1.87+ (auto-installed if missing)
echo   - Visual Studio Build Tools with C++ workload (for source builds)
echo   - Node.js (optional, for web dashboard)
echo.
goto :end

:: ---- Error exit ----
:error_exit
echo.
echo %RED%Setup failed. See errors above.%RESET%
echo Need help? Open an issue at %REPO%/issues
echo.
endlocal
exit /b 1

:: ---- Dry-run summary ----
:dry_run_done
echo.
echo   %GREEN%Dry run complete.%RESET% No changes were made.
goto :end

:: ---- Clean exit ----
:end
endlocal
exit /b 0
