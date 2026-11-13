#!/usr/bin/env bash
# CLI-only artifact ratchet. The CLI-only build (`cargo generate features
# --selection cli-only`) must not link the web gateway, the dashboard assets
# or an HTTP server other than the channel listeners the core keeps as
# documented exceptions (ADR-019 D8: webhook, Lark, LINE and voice-call).
#
#   cli_only_ratchet.sh run <target-triple> <unstripped-zeroclaw-binary>
#   cli_only_ratchet.sh check <tree.txt> <axum-dependents.txt> <symbols.txt>
#
# `check` decides over saved `cargo tree` and `nm` output, so the test can
# drive it with fixtures. Release builds are stripped; pass a dev build.
set -euo pipefail

forbidden_crates=(zeroclaw-gateway include_dir)
allowed_axum_dependents=(zeroclaw-channels)
# `run`'s scratch dir. Script-level, not local: the EXIT trap reads it after
# `run` has returned, and `set -u` would fail the script on an unset local.
work=""
trap '[ -z "$work" ] || rm -rf "$work"' EXIT

check() {
  local tree="$1" axum_dependents="$2" symbols="$3" failed=0 crate dependent allowed ok
  for crate in "${forbidden_crates[@]}"; do
    if grep -q "^${crate} " "$tree"; then
      echo "::error::the CLI-only build links ${crate}"
      failed=1
    fi
  done
  while read -r dependent _; do
    [ -n "$dependent" ] || continue
    allowed=0
    for ok in "${allowed_axum_dependents[@]}"; do
      if [ "$dependent" = "$ok" ]; then
        allowed=1
      fi
    done
    if [ "$allowed" = 0 ]; then
      echo "::error::axum is linked through ${dependent}, which is not a documented listener exception"
      failed=1
    fi
  done < "$axum_dependents"
  if [ ! -s "$symbols" ]; then
    echo "::error::the binary has no symbols to inspect; pass an unstripped build"
    failed=1
  elif grep -q 'zeroclaw_gateway::' "$symbols"; then
    echo "::error::the CLI-only binary contains zeroclaw_gateway symbols"
    failed=1
  fi
  if [ "$failed" = 0 ]; then
    echo "CLI-only ratchet passed."
  fi
  return "$failed"
}

run() {
  local target="$1" binary="$2" features
  features="$(cargo run --quiet --locked -p xtask --bin generate -- features --selection cli-only --target "$target")"
  work="$(mktemp -d)"
  cargo tree --locked --target "$target" -e normal --prefix none -p zeroclaw \
    --no-default-features --features "$features" | sort -u > "$work/tree.txt"
  if grep -q '^axum ' "$work/tree.txt"; then
    cargo tree --locked --target "$target" -e normal --prefix none -p zeroclaw \
      --no-default-features --features "$features" -i axum --depth 1 \
      | tail -n +2 | sort -u > "$work/axum-dependents.txt"
  else
    : > "$work/axum-dependents.txt"
  fi
  nm -C --defined-only "$binary" > "$work/symbols.txt" 2>/dev/null || true
  check "$work/tree.txt" "$work/axum-dependents.txt" "$work/symbols.txt"
}

case "${1:-}" in
  run)
    shift
    run "$@"
    ;;
  check)
    shift
    check "$@"
    ;;
  *)
    echo "usage: $0 run <target-triple> <binary> | check <tree> <axum-dependents> <symbols>" >&2
    exit 2
    ;;
esac
