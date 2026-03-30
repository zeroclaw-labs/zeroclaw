#!/usr/bin/env bash
# dev/test.sh — Local build & test harness for ZeroClaw
#
# Usage:
#   ./dev/test.sh                # Full: fmt, clippy, build, all tests
#   ./dev/test.sh plugins        # Build + plugin tests only
#   ./dev/test.sh build          # Build only
#   ./dev/test.sh test           # All test suites
#   ./dev/test.sh clippy         # Clippy only
#   ./dev/test.sh fmt            # Format check only
#   ./dev/test.sh quick          # Fmt + clippy + unit tests
#
# Flags:
#   --verbose, -v    Show full cargo output (default: quiet, summary only)
#   --release        Use release profile
#   --features FEAT  Override features (default: plugins-wasm)
#   --help, -h       Show help

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# ── Defaults ────────────────────────────────────────────────────────────────
VERBOSE=false
FEATURES="plugins-wasm"
PROFILE=""
CMD=""

# ── Parse args ──────────────────────────────────────────────────────────────
while [ $# -gt 0 ]; do
  case "$1" in
    -v|--verbose)  VERBOSE=true ;;
    --release)     PROFILE="--release" ;;
    --features)    shift; FEATURES="$1" ;;
    -h|--help)
      cat <<'EOF'
ZeroClaw Test Harness

Usage: ./dev/test.sh [command] [flags]

Commands:
  (none)    Full validation: fmt, clippy, build, all tests
  plugins   Build + plugin integration tests only
  build     Build only
  test      All test suites (unit, component, integration, system)
  clippy    Clippy only
  fmt       Format check only
  quick     Fmt + clippy + unit tests

Flags:
  -v, --verbose    Show full cargo output
  --release        Build in release mode
  --features FEAT  Override feature flags (default: plugins-wasm)
  -h, --help       Show this help
EOF
      exit 0
      ;;
    -*)
      echo "Unknown flag: $1" >&2; exit 1 ;;
    *)
      if [ -z "$CMD" ]; then CMD="$1"
      else echo "Unexpected argument: $1" >&2; exit 1; fi
      ;;
  esac
  shift
done

CMD="${CMD:-all}"

# ── Colors ──────────────────────────────────────────────────────────────────
if [ -t 1 ]; then
  B='\033[1m' R='\033[31m' G='\033[32m' Y='\033[33m' C='\033[36m' D='\033[2m' Z='\033[0m'
else
  B='' R='' G='' Y='' C='' D='' Z=''
fi

# ── State ───────────────────────────────────────────────────────────────────
declare -a S_NAME=() S_RESULT=() S_DUR=() S_DETAIL=()
FAILS=0
T_START=$(date +%s)

# ── Helpers ─────────────────────────────────────────────────────────────────
feat() { [ -n "$FEATURES" ] && echo "--features $FEATURES"; }

# run_step "Name" cmd args...
run_step() {
  local name="$1"; shift
  local t0 rc tmpf t1 elapsed
  t0=$(date +%s)
  S_NAME+=("$name")
  tmpf=$(mktemp)

  printf "  %-28s " "$name"

  rc=0
  if $VERBOSE; then
    "$@" 2>&1 | tee "$tmpf" || rc=${PIPESTATUS[0]:-1}
  else
    "$@" >"$tmpf" 2>&1 || rc=$?
  fi

  t1=$(date +%s)
  elapsed=$(( t1 - t0 ))
  S_DUR+=("${elapsed}s")

  if [ "$rc" -eq 0 ]; then
    S_RESULT+=("pass")
    S_DETAIL+=("")
    echo -e "${G}PASS${Z}  ${D}${elapsed}s${Z}"
  else
    S_RESULT+=("FAIL")
    S_DETAIL+=("$(tail -30 "$tmpf")")
    FAILS=$(( FAILS + 1 ))
    echo -e "${R}FAIL${Z}  ${D}${elapsed}s${Z}"
  fi
  rm -f "$tmpf"
}

# ── Steps ───────────────────────────────────────────────────────────────────
# shellcheck disable=SC2086
step_fmt()      { run_step "Format"           cargo fmt --all -- --check; }
step_clippy()   { run_step "Clippy"           cargo clippy --all-targets $(feat) $PROFILE -- -D warnings; }
step_build()    { run_step "Build"            cargo build $(feat) $PROFILE; }
step_unit()     { run_step "Unit Tests"       cargo test --lib --bins $(feat) $PROFILE; }
step_component(){ run_step "Component Tests"  cargo test --test component $(feat) $PROFILE; }
step_integ()    { run_step "Integration Tests" cargo test --test integration $(feat) $PROFILE; }
step_system()   { run_step "System Tests"     cargo test --test system $(feat) $PROFILE; }
step_plugins()  { run_step "Plugin Tests"     cargo test --test integration plugin_ $(feat) $PROFILE; }

# ── Summary ─────────────────────────────────────────────────────────────────
print_summary() {
  local t_end elapsed
  t_end=$(date +%s)
  elapsed=$(( t_end - T_START ))

  echo ""
  echo -e "${B}═══════════════════════════════════════════${Z}"
  echo -e "${B}  Summary${Z}  features=${C}${FEATURES:-default}${Z}  total=${B}${elapsed}s${Z}"
  echo -e "${B}═══════════════════════════════════════════${Z}"

  if [ "$FAILS" -gt 0 ]; then
    echo ""
    echo -e "  ${R}${B}$FAILS step(s) failed:${Z}"
    for i in "${!S_NAME[@]}"; do
      if [ "${S_RESULT[$i]}" = "FAIL" ] && [ -n "${S_DETAIL[$i]}" ]; then
        echo ""
        echo -e "  ${R}── ${S_NAME[$i]} ──${Z}"
        echo "${S_DETAIL[$i]}" | sed 's/^/    /'
      fi
    done
    echo ""
    return 1
  else
    echo -e "  ${G}${B}All steps passed.${Z}"
    echo ""
    return 0
  fi
}

# ── Main ────────────────────────────────────────────────────────────────────
echo -e "${B}ZeroClaw Test Harness${Z}  [${C}${CMD}${Z}]"
echo ""

case "$CMD" in
  all)
    step_fmt
    step_clippy
    step_build
    step_unit
    step_component
    step_integ
    step_system
    ;;
  plugins)
    step_build
    step_plugins
    ;;
  build)
    step_build
    ;;
  test)
    step_unit
    step_component
    step_integ
    step_system
    ;;
  clippy)
    step_clippy
    ;;
  fmt)
    step_fmt
    ;;
  quick)
    step_fmt
    step_clippy
    step_unit
    ;;
  *)
    echo "Unknown command: $CMD" >&2
    exit 1
    ;;
esac

print_summary
