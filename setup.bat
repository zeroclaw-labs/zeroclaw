@echo off
setlocal enabledelayedexpansion

:: ============================================================================
:: ZeroClaw Windows Setup Script
:: Simplifies building and installing ZeroClaw on Windows.
:: Usage: setup.bat [--prebuilt | --minimal | --standard | --full | --help]
:: ============================================================================

set "VERSION=0.6.2"
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
if "%~1"=="--help"     goto :show_help
if "%~1"=="-h"         goto :show_help
if "%~1"=="--prebuilt" set "MODE=prebuilt" & goto :start
if "%~1"=="--minimal"  set "MODE=minimal"  & goto :start
if "%~1"=="--standard" set "MODE=standard" & goto :start
if "%~1"=="--full"     set "MODE=full"     & goto :start

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
for /f "tokens=3" %%a in ('dir /-C "%~dp0" 2^>nul ^| findstr /C:"bytes free"') do (
    set /a "FREE_DISK_GB=%%a / 1073741824"
)

:: Check Rust
where cargo >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo   %YELLOW%Rust not found.%RESET%
    goto :install_rust
) else (
    for /f "tokens=2" %%v in ('rustc --version 2^>nul') do set "RUST_VER=%%v"
    echo   %GREEN%OK%RESET% Rust !RUST_VER! found
)

:: Check Node.js (optional)
where node >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    echo   %YELLOW%Node.js not found (optional - web dashboard will use stub).%RESET%
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
:choose_mode
echo.

if "%MODE%"=="prebuilt" goto :install_prebuilt
if "%MODE%"=="minimal"  goto :build_minimal
if "%MODE%"=="standard" goto :build_standard
if "%MODE%"=="full"     goto :build_full

:: Interactive mode
echo %BOLD%[2/5] Choose installation method:%RESET%
echo.
echo   1) Prebuilt binary   - Download pre-compiled release (fastest, ~2 min)
echo   2) Minimal build     - Default features only (~15 min)
echo   3) Standard build    - Default + Lark/Feishu + Matrix + Postgres (~20 min)
echo   4) Full build        - All features including hardware + browser (~30 min)
echo.
set /p "CHOICE=  Select [1-4] (default: 1): "

if "%CHOICE%"=="" set "CHOICE=1"
if "%CHOICE%"=="1" goto :install_prebuilt
if "%CHOICE%"=="2" goto :build_minimal
if "%CHOICE%"=="3" goto :build_standard
if "%CHOICE%"=="4" goto :build_full

echo   %RED%Invalid choice. Please enter 1-4.%RESET%
goto :choose_mode

:: ---- Prebuilt binary ----
:install_prebuilt
echo.
echo %BOLD%[3/5] Downloading prebuilt binary...%RESET%

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
    echo   %YELLOW%Prebuilt binary not available. Falling back to source build (standard).%RESET%
    goto :build_standard
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
goto :post_install

:: ---- Minimal build ----
:build_minimal
set "FEATURES="
set "BUILD_DESC=minimal (default features)"
goto :do_build

:: ---- Standard build ----
:build_standard
set "FEATURES=--features channel-matrix,channel-lark,memory-postgres"
set "BUILD_DESC=standard (Matrix + Lark/Feishu + Postgres)"
goto :do_build

:: ---- Full build ----
:build_full
set "FEATURES=--features channel-matrix,channel-lark,memory-postgres,browser-native,hardware,rag-pdf,observability-otel"
set "BUILD_DESC=full (all features)"
goto :do_build

:: ---- Build from source ----
:do_build
echo.
echo %BOLD%[3/5] Building ZeroClaw (%BUILD_DESC%)...%RESET%
echo   Target: %TARGET%

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

cargo build --release --locked %FEATURES% --target %TARGET%
if %ERRORLEVEL% NEQ 0 (
    echo.
    echo   %RED%ERROR: Build failed.%RESET%
    echo   Common fixes:
    echo   - Ensure Visual Studio Build Tools are installed (C++ workload)
    echo   - Run: rustup update
    echo   - Check disk space (6 GB needed)
    goto :error_exit
)

echo   %GREEN%OK%RESET% Build succeeded.

:: Copy binary to a convenient location
echo.
echo %BOLD%[4/5] Installing binary...%RESET%
mkdir "%USERPROFILE%\.zeroclaw\bin" 2>nul
copy /Y "target\%TARGET%\release\zeroclaw.exe" "%USERPROFILE%\.zeroclaw\bin\zeroclaw.exe" >nul
echo   %GREEN%OK%RESET% Installed to %USERPROFILE%\.zeroclaw\bin\zeroclaw.exe

:: Add to PATH if not already there
echo %PATH% | findstr /I /C:".zeroclaw\bin" >nul 2>&1
if %ERRORLEVEL% NEQ 0 (
    setx PATH "%PATH%;%USERPROFILE%\.zeroclaw\bin" >nul 2>&1
    set "PATH=%PATH%;%USERPROFILE%\.zeroclaw\bin"
    echo   %GREEN%OK%RESET% Added to PATH
)

goto :post_install

:: ---- Post install ----
:post_install
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
echo     2. Run: zeroclaw init
echo     3. Configure your API key in %%USERPROFILE%%\.zeroclaw\config.toml
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
echo   --minimal     Build with default features only
echo   --standard    Build with Matrix + Lark/Feishu + Postgres
echo   --full        Build with all features
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

:: ---- Clean exit ----
:end
endlocal
exit /b 0
