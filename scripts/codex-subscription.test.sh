#!/usr/bin/env bash
set -euo pipefail

script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
subject=${1:-$script_dir/codex-subscription.sh}
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

cat > "$fixture/zeroclaw" <<'FAKE'
#!/usr/bin/env bash
set -euo pipefail

state=${FAKE_CODEX_STATE:?}
active=$(cat "$state")
shifted=("$@")

if [[ "${shifted[*]}" == 'auth list' ]]; then
  for profile in default back backup spare; do
    if [[ "$profile" == "$active" ]]; then
      echo "* openai-codex:$profile"
    else
      echo "  openai-codex:$profile"
    fi
  done
  exit 0
fi

if [[ "${shifted[*]}" == 'auth status' ]]; then
  echo "  openai-codex:default kind=OAuth expires=expires in 60m"
  echo "  openai-codex:back kind=OAuth expires=expired at 2026-01-01T00:00:00Z"
  if [[ "${FAKE_ALT_EXPIRED:-0}" == 1 ]]; then
    echo "  openai-codex:backup kind=OAuth expires=expired at 2026-01-01T00:00:00Z"
  else
    echo "  openai-codex:backup kind=OAuth expires=expires in 60m"
  fi
  echo "  openai-codex:spare kind=OAuth expires=expires in 60m"
  exit 0
fi

if [[ "${shifted[0]:-} ${shifted[1]:-}" == 'auth use' ]]; then
  target=''
  for ((i = 0; i < ${#shifted[@]}; i++)); do
    if [[ "${shifted[$i]}" == --profile ]]; then
      target=${shifted[$((i + 1))]}
      break
    fi
  done
  printf '%s\n' "$target" > "$state"
  echo "Active profile for openai-codex: $target"
  exit 0
fi

echo "unexpected fake invocation: ${shifted[*]}" >&2
exit 2
FAKE
chmod +x "$fixture/zeroclaw"
printf 'default\n' > "$fixture/state"

run() {
  FAKE_CODEX_STATE="$fixture/state" ZEROCLAW_BIN="$fixture/zeroclaw" \
    CODEX_ALT_PROFILE=backup "$subject" "$@"
}

[[ "$(run current)" == default ]]
run use alt | grep -q 'Active Codex subscription: backup'
[[ "$(run current)" == backup ]]
run use alt | grep -q 'Active Codex subscription: backup'
run toggle | grep -q 'Active Codex subscription: default'
[[ "$(run current)" == default ]]
run use spare | grep -q 'Active Codex subscription: spare'

if FAKE_ALT_EXPIRED=1 run use alt >"$fixture/expired.out" 2>&1; then
  echo 'FAIL: expired alternate profile was activated' >&2
  exit 1
fi
grep -q 'is expired; sign in before switching' "$fixture/expired.out"

if run use missing >"$fixture/missing.out" 2>&1; then
  echo 'FAIL: missing profile was activated' >&2
  exit 1
fi
grep -q 'does not exist' "$fixture/missing.out"

if run toggle >"$fixture/unexpected.out" 2>&1; then
  echo 'FAIL: toggle from an unconfigured endpoint succeeded' >&2
  exit 1
fi
grep -q 'is neither configured toggle endpoint' "$fixture/unexpected.out"

echo 'codex-subscription tests: pass'
