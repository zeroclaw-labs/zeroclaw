@echo off
setlocal enabledelayedexpansion

:: ============================================================================
:: ZeroClaw Windows Setup Script
:: Simplifies building and installing ZeroClaw on Windows.
:: Usage: setup.bat [--source | --minimal | --dist | --default | --all | --dry-run | --help]
:: ============================================================================

set "VERSION=0.8.3"
set "TARGET=x86_64-pc-windows-msvc"
set "REPO=https://github.com/kckylechen1/zeroclaw"

:: Colors via ANSI (Windows 10+ Terminal)
set "GREEN=[32m"
set "YELLOW=[33m"
set "RED=[31m"
set "BLUE=[34m"
set "BOLD=[1m"
set "RESET=[0m"

:: Parse arguments. Reject retired prebuilt mode before prerequisites or writes.
set "MODE="
set "DRY_RUN=false"
:parse_args
if "%~1"=="" goto :start
if "%~1"=="--help" goto :show_help
if "%~1"=="-h" goto :show_help
if "%~1"=="--prebuilt" goto :prebuilt_unavailable
if "%~1"=="--source" (
    shift
    goto :parse_args
)
if "%~1"=="--dry-run" (
    set "DRY_RUN=true"
    shift
    goto :parse_args
)
if "%~1"=="--minimal" (
    set "MODE=minimal"
    shift
    goto :parse_args
)
if "%~1"=="--dist" (
    set "MODE=dist"
    shift
    goto :parse_args
)
if "%~1"=="--default" (
    set "MODE=default"
    shift
    goto :parse_args
)
if "%~1"=="--all" (
    set "MODE=all"
    shift
    goto :parse_args
)
echo Unknown option. Run setup.bat --help for supported options.
goto :error_exit

:prebuilt_unavailable
echo This fork has no verified prebuilt binary channel.
echo Clone %REPO%.git into a new directory and run setup.bat --source there.
goto :error_exit

:start
echo.
echo %BOLD%%BLUE%=========================================%RESET%
echo %BOLD%%BLUE%  ZeroClaw Windows Setup  v%VERSION%%RESET%
echo %BOLD%%BLUE%=========================================%RESET%
echo.

:: Verify source identity before toolchain bootstrap; do not echo a remote URL
:: because a configured URL may contain credentials.
if not exist "Cargo.toml" (
    echo   %RED%ERROR: Run this script from the fork repository root.%RESET%
    echo   git clone %REPO%.git
    echo   cd zeroclaw
    echo   setup.bat --source
    goto :error_exit
)
where git >nul 2>&1
if errorlevel 1 (
    echo   %RED%ERROR: Git is required to verify the fork source checkout.%RESET%
    echo   Install Git from https://git-scm.com/download/win
    goto :error_exit
)
:: Read Git output as data: exact one-line membership must not depend on the
:: byte-level LF/CRLF behavior of Git's stdout and FINDSTR /X. PowerShell's
:: default string membership is case-insensitive, matching the former /I policy.
powershell -NoProfile -NonInteractive -Command "$origin = @(git remote get-url origin 2>$null); if ($LASTEXITCODE -ne 0 -or $origin.Count -ne 1) { exit 1 }; $allowed = @('%REPO%', '%REPO%.git', 'git@github.com:kckylechen1/zeroclaw', 'git@github.com:kckylechen1/zeroclaw.git', 'ssh://git@github.com/kckylechen1/zeroclaw', 'ssh://git@github.com/kckylechen1/zeroclaw.git'); if ($allowed -notcontains $origin[0]) { exit 1 }; exit 0" >nul 2>nul
if errorlevel 1 (
    echo   %RED%ERROR: Source origin is not this fork or could not be verified.%RESET%
    echo   Clone %REPO%.git into a new directory and run setup.bat --source there.
    echo   Existing checkout was not changed.
    goto :error_exit
)

:: ---- Step 1: Check prerequisites ----
echo %BOLD%[1/5] Checking prerequisites...%RESET%

:: Check available RAM (rough estimate via wmic)
for /f "tokens=2 delims==" %%a in ('wmic os get FreePhysicalMemory /value 2^>nul ^| find "="') do (
    set /a "FREE_RAM_MB=%%a / 1024"
)
if defined FREE_RAM_MB (
    if !FREE_RAM_MB! LSS 2048 (
        echo   %YELLOW%WARNING: Only !FREE_RAM_MB! MB free RAM detected. 2048 MB recommended for source builds.%RESET%
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
if "%MODE%"=="" goto :build_dist
if "%MODE%"=="minimal" goto :build_minimal
if "%MODE%"=="dist" goto :build_dist
if "%MODE%"=="default" goto :build_default
if "%MODE%"=="all" goto :build_all
echo   %RED%Unknown source build mode.%RESET%
goto :error_exit
:: >>> end generated:menu <<<

:: ---- Source build presets ----
:: >>> generated:presets by `cargo generate installers` - do not edit <<<
:build_minimal
set "FEATURES=--no-default-features"
set "BUILD_DESC=minimal (core only, no default features)"
goto :do_build

:build_dist
set "FEATURES=--no-default-features --features agent-runtime,channel-acp-server,channel-discord,channel-email,channel-telegram,channel-webhook,gateway,memory-tachi,observability-prometheus,schema-export"
set "BUILD_DESC=dist (lean standard distribution (recommended))"
goto :do_build

:build_default
set "FEATURES="
set "BUILD_DESC=default (default feature set)"
goto :do_build

:build_all
set "FEATURES=--no-default-features --features agent-runtime,channel-acp-server,channel-discord,channel-email,channel-notion,channel-telegram,channel-webhook,gateway,memory-tachi,nodes,observability-prometheus,provider-gitea,provider-github,sandbox-bubblewrap,sandbox-landlock,schema-export"
set "BUILD_DESC=all (every optional feature (kitchen sink))"
goto :do_build
:: >>> end generated:presets <<<

:: ---- Build from source ----
:do_build
echo.
echo %BOLD%[3/5] Building ZeroClaw (%BUILD_DESC%)...%RESET%
echo   Target: %TARGET%

if "%DRY_RUN%"=="true" (
    echo   [dry-run] Would run: cargo build --release --locked %FEATURES% --target %TARGET%
    echo   [dry-run] Would install to %USERPROFILE%\.zeroclaw\bin
    echo   [dry-run] Would build web dashboard ^(cargo web build^) and install to %LOCALAPPDATA%\zeroclaw\web\dist
    echo   [dry-run] Would add %USERPROFILE%\.zeroclaw\bin to PATH
    goto :dry_run_done
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

:: Copy binary to a convenient location
echo.
echo %BOLD%[4/5] Installing binary...%RESET%
mkdir "%USERPROFILE%\.zeroclaw\bin" 2>nul
copy /Y "target\%TARGET%\release\zeroclaw.exe" "%USERPROFILE%\.zeroclaw\bin\zeroclaw.exe" >nul
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
            robocopy "web\dist" "%LOCALAPPDATA%\zeroclaw\web\dist" /MIR /NFL /NDL /NJH /NJS >nul
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
echo     4. Start chatting: zeroclaw agent
)
echo.
echo   Documentation: %REPO%
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
echo   --source      Build this fork from source ^(default^)
echo   --prebuilt    Unsupported: this fork has no verified binary channel
echo   --minimal     Build core only ^(--no-default-features^)
echo   --dist        Build lean standard distribution (recommended)
echo   --default     Build the default feature set
echo   --all         Build every feature
echo   --dry-run     Show what would happen without building or installing
echo   --help, -h    Show this help message
echo.
echo Without arguments, builds the recommended dist preset from this fork checkout.
echo.
echo Prerequisites:
echo   - Git (required)
echo   - Rust via rustup ^(uses the repository toolchain; bootstrapped if missing^)
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
