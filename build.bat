@echo off
setlocal EnableDelayedExpansion

:: ---------------------------------------------------------------------------
:: build.bat — ZeroClaw build helper for Windows
::
:: Usage:
::   build.bat              — full release build (Rust + Web UI) -> Build\<timestamp>\
::   build.bat debug        — full debug  build  (Rust + Web UI) -> Build\<timestamp>\
::   build.bat rust         — Rust release only (no collect)
::   build.bat web          — Web UI only (no collect)
::   build.bat check        — cargo check (fast, no codegen)
::   build.bat test         — run all Rust tests
::   build.bat test <pat>   — run tests matching pattern
::   build.bat clippy       — lint with clippy -D warnings
::   build.bat fmt          — cargo fmt
::   build.bat clean        — remove target\ and web\dist\
:: ---------------------------------------------------------------------------

set "ROOT=%~dp0"
set "ROOT=%ROOT:~0,-1%"

echo ==========================================================
echo  ZeroClaw build  —  %ROOT%
echo ==========================================================
echo.

:: ── Locate cargo ────────────────────────────────────────────────────────────
set "CARGO="

:: 1) prefer explicit rustup path
if exist "%USERPROFILE%\.cargo\bin\cargo.exe" (
    set "CARGO=%USERPROFILE%\.cargo\bin\cargo.exe"
)

:: 2) fall back to whatever is on PATH
if not defined CARGO (
    where cargo >nul 2>&1
    if not errorlevel 1 set "CARGO=cargo"
)

if not defined CARGO (
    echo [ERROR] cargo not found.
    echo         Install Rust from https://rustup.rs then restart this terminal.
    goto :fail
)

:: Add stable toolchain bin so cargo can find rustc ─────────────────────────
set "TOOLCHAIN_BIN="
for /d %%T in ("%USERPROFILE%\.rustup\toolchains\stable-*-pc-windows-msvc") do (
    set "TOOLCHAIN_BIN=%%T\bin"
)
if defined TOOLCHAIN_BIN (
    set "PATH=!TOOLCHAIN_BIN!;%PATH%"
)

:: Verify rustc is reachable ─────────────────────────────────────────────────
"%CARGO%" --version >nul 2>&1
if errorlevel 1 (
    echo [ERROR] cargo found at "%CARGO%" but failed to run.
    echo         Try opening a new Developer Command Prompt.
    goto :fail
)

echo [info] cargo : !CARGO!

:: ── Locate npm via node.exe (avoids local node_modules\npm hijack) ─────────
set "NPM_CLI="
set "NODE_EXE="
for /f "delims=" %%N in ('where node 2^>nul') do (
    if not defined NODE_EXE set "NODE_EXE=%%N"
)
if defined NODE_EXE (
    :: node ships npm-cli.js next to node.exe  e.g. C:\Program Files\nodejs\
    for %%D in ("!NODE_EXE!") do set "NODE_DIR=%%~dpD"
    if exist "!NODE_DIR!node_modules\npm\bin\npm-cli.js" (
        set "NPM_CLI=!NODE_DIR!node_modules\npm\bin\npm-cli.js"
    )
)
:: Fallback: try well-known Program Files locations
if not defined NPM_CLI (
    for %%P in (
        "C:\Program Files\nodejs\node_modules\npm\bin\npm-cli.js"
        "C:\Program Files (x86)\nodejs\node_modules\npm\bin\npm-cli.js"
    ) do (
        if exist %%P set "NPM_CLI=%%~P"
    )
)

if defined NPM_CLI (
    echo [info] npm   : !NPM_CLI!
) else (
    echo [info] npm   : NOT FOUND — web build will be skipped
)
echo.

:: ── Dispatch ────────────────────────────────────────────────────────────────
set "CMD=%~1"
if "%CMD%"=="" set "CMD=release"

if /i "!CMD!"=="release" goto :full_release
if /i "!CMD!"=="debug"   goto :full_debug
if /i "!CMD!"=="rust"    goto :rust_only
if /i "!CMD!"=="web"     goto :web_only
if /i "!CMD!"=="check"   goto :do_check
if /i "!CMD!"=="test"    goto :do_test
if /i "!CMD!"=="clippy"  goto :do_clippy
if /i "!CMD!"=="fmt"     goto :do_fmt
if /i "!CMD!"=="clean"   goto :do_clean

echo [ERROR] Unknown command: !CMD!
echo Usage: build.bat [release^|debug^|rust^|web^|check^|test^|clippy^|fmt^|clean]
goto :fail

:: ============================================================================
:full_release
    call :web_build
    if errorlevel 1 goto :fail
    call :rust_build release
    if errorlevel 1 goto :fail
    call :collect release
    goto :ok

:full_debug
    call :web_build
    if errorlevel 1 goto :fail
    call :rust_build debug
    if errorlevel 1 goto :fail
    call :collect debug
    goto :ok

:rust_only
    call :rust_build release
    if errorlevel 1 goto :fail
    goto :ok

:web_only
    call :web_build
    if errorlevel 1 goto :fail
    goto :ok

:do_check
    echo [cargo] check
    "!CARGO!" check
    if errorlevel 1 goto :fail
    goto :ok

:do_test
    if "%~2"=="" (
        echo [cargo] test
        "!CARGO!" test
    ) else (
        echo [cargo] test %~2
        "!CARGO!" test %~2
    )
    if errorlevel 1 goto :fail
    goto :ok

:do_clippy
    echo [cargo] clippy
    "!CARGO!" clippy -- -D warnings
    if errorlevel 1 goto :fail
    goto :ok

:do_fmt
    echo [cargo] fmt
    "!CARGO!" fmt
    if errorlevel 1 goto :fail
    goto :ok

:do_clean
    echo [clean] cargo target\
    "!CARGO!" clean
    if exist "!ROOT!\web\dist" (
        echo [clean] web\dist\
        rmdir /s /q "!ROOT!\web\dist"
    )
    goto :ok

:: ============================================================================
:: Subroutines
:: ============================================================================

:rust_build
    set "_PROFILE=%~1"
    if "!_PROFILE!"=="release" (
        echo [cargo] build --release
        "!CARGO!" build --release --features plugins-wasm
    ) else (
        echo [cargo] build
        "!CARGO!" build --features plugins-wasm
    )
    exit /b !errorlevel!

:web_build
    if not defined NPM_CLI (
        echo [web] skipped — npm not found
        exit /b 0
    )
    if not exist "!ROOT!\web\node_modules" (
        echo [web] npm install
        "!NODE_EXE!" "!NPM_CLI!" install --prefix "!ROOT!\web"
        if errorlevel 1 exit /b 1
    )
    echo [web] npm run build
    "!NODE_EXE!" "!NPM_CLI!" run build --prefix "!ROOT!\web"
    exit /b !errorlevel!

:collect
    set "_PROFILE=%~1"

    :: Timestamp from PowerShell (locale-safe)
    for /f "delims=" %%D in ('powershell -NoProfile -Command "Get-Date -Format 'yyyy-MM-dd_HH-mm-ss'"') do set "TS=%%D"

    set "OUT=!ROOT!\Build\!TS!"
    echo.
    echo [collect] Output folder: !OUT!
    mkdir "!OUT!" 2>nul

    :: Copy binary
    if "!_PROFILE!"=="release" (
        set "_BIN=!ROOT!\target\release\zeroclaw.exe"
    ) else (
        set "_BIN=!ROOT!\target\debug\zeroclaw.exe"
    )
    if exist "!_BIN!" (
        echo [collect] zeroclaw.exe
        copy /y "!_BIN!" "!OUT!\zeroclaw.exe" >nul
    ) else (
        echo [WARN] Binary not found: !_BIN!
    )

    :: Copy web UI — must land at <binary_dir>\web\dist\ for auto-detect
    if exist "!ROOT!\web\dist" (
        echo [collect] web\dist\ -^> web\dist\
        xcopy /E /I /Q /Y "!ROOT!\web\dist" "!OUT!\web\dist" >nul
    ) else (
        echo [WARN] web\dist not found — web UI not included
    )

    echo.
    echo  Done: !OUT!
    exit /b 0

:: ============================================================================
:ok
    echo.
    echo ========== BUILD OK ==========
    pause
    endlocal
    exit /b 0

:fail
    echo.
    echo ========== BUILD FAILED ==========
    pause
    endlocal
    exit /b 1
