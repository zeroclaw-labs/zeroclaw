#!/usr/bin/env bash
set -euo pipefail

# publish-crates.sh: publish the ZeroClaw workspace to crates.io.
#
# Usage:
#   scripts/release/publish-crates.sh                 # dry run (default, safe)
#   scripts/release/publish-crates.sh --execute       # real publish
#   scripts/release/publish-crates.sh --execute 0.8.4 # real publish, assert version
#
# Publishing is IRREVERSIBLE: a crates.io version can be yanked but never
# replaced or deleted. The default is therefore a dry run; the real publish
# requires an explicit --execute.
#
# The dry run selects every manifest-enabled crate at the coordinated workspace
# version. Cargo builds an ephemeral local registry, packages each crate, and
# compiles it from its tarball without uploading. The real publish walks the
# same set one crate at a time in dependency order so the run can be paced
# against crates.io's rate limits and demonstrably resume after a partial
# failure. See the comment above the publish loop.
#
# What this adds over cargo alone:
#
#   1. independent-version workspace crates are excluded from this release
#   2. which crates are already on crates.io, queried before anything uploads,
#      so a re-run skips them without relying on cargo's behaviour
#   3. no publishable crate depends on an unpublishable workspace crate
#   4. CARGO_REGISTRY_TOKEN is present before anything irreversible starts
#
# The set of crates and their order are derived from `cargo metadata` — never
# hardcoded here, so they cannot drift from the manifests.
# tests/architecture/publish_contract.rs asserts the same invariants in CI.

REPO_ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO_ROOT"

EXECUTE=0
EXPECTED_VERSION=""
for arg in "$@"; do
  case "$arg" in
    --execute) EXECUTE=1 ;;
    --dry-run) EXECUTE=0 ;;
    -h | --help)
      sed -n '3,28p' "$0"
      exit 0
      ;;
    -*)
      echo "error: unknown flag: $arg" >&2
      exit 2
      ;;
    *) EXPECTED_VERSION="$arg" ;;
  esac
done

need() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: $1 is required" >&2
    exit 1
  }
}
need cargo
need jq
need curl
need python3

VERSION="$(sed -n 's/^version = "\([^"]*\)"/\1/p' Cargo.toml | head -1)"
if [[ ! "$VERSION" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "error: [workspace.package] version is not a stable semver: '$VERSION'" >&2
  exit 1
fi
if [[ -n "$EXPECTED_VERSION" && "$VERSION" != "$EXPECTED_VERSION" ]]; then
  echo "error: workspace version is $VERSION but $EXPECTED_VERSION was requested." >&2
  echo "       Run scripts/release/bump-version.sh first." >&2
  exit 1
fi

META="$(cargo metadata --format-version 1 --no-deps)"

# `publish` is null when unrestricted (publishable) and [] when publish = false.
# Only packages at the coordinated workspace version belong to this release;
# independent-version workspace members are reported but never uploaded merely
# because they share the repository.
# Built with a read loop rather than `mapfile`, which macOS's bash 3.2 lacks —
# maintainers dry-run this locally before triggering the release workflow.
PUBLISHABLE=()
PRIVATE=()
INDEPENDENT=()
while IFS= read -r name; do
  [[ -n "$name" ]] && PUBLISHABLE+=("$name")
done < <(jq -r --arg version "$VERSION" \
  '.packages[] | select(.publish == null and .version == $version) | .name' \
  <<<"$META" | sort)
while IFS= read -r name; do
  [[ -n "$name" ]] && PRIVATE+=("$name")
done < <(jq -r --arg version "$VERSION" \
  '.packages[] | select(.publish != null and .version == $version) | .name' \
  <<<"$META" | sort)
while IFS= read -r item; do
  [[ -n "$item" ]] && INDEPENDENT+=("$item")
done < <(jq -r --arg version "$VERSION" \
  '.packages[] | select(.version != $version) | "\(.name)@\(.version)"' \
  <<<"$META" | sort)

if [[ ${#PUBLISHABLE[@]} -eq 0 ]]; then
  echo "error: no publishable crates found — every manifest has publish = false." >&2
  exit 1
fi

echo "ZeroClaw crates.io publish"
echo "  version:      $VERSION"
echo "  mode:         $([[ $EXECUTE -eq 1 ]] && echo 'EXECUTE (irreversible)' || echo 'dry run')"
echo "  publishable:  ${#PUBLISHABLE[@]} crates"
echo "  private:      ${#PRIVATE[@]} crates (${PRIVATE[*]})"
echo "  independent:  ${#INDEPENDENT[@]} crates (${INDEPENDENT[*]})"
echo

# ── Preflight 1: no release crate depends on a private one ─────────────────
# cargo only reports this once it reaches the offending crate, which can be
# after several irreversible uploads have already succeeded.
leaks="$(jq -r --arg version "$VERSION" \
  --argjson priv "$(printf '%s\n' "${PRIVATE[@]}" | jq -R . | jq -s .)" '
  [ .packages[]
    | select(.publish == null and .version == $version) as $p
    | $p.dependencies[]
    | select(.kind == null or .kind == "build")
    | select(.name as $n | $priv | index($n))
    | "\($p.name) -> \(.name)"
  ] | unique | .[]' <<<"$META")"
if [[ -n "$leaks" ]]; then
  echo "error: publishable crates depend on unpublishable workspace crates:" >&2
  while IFS= read -r leak; do
    echo "       $leak" >&2
  done <<<"$leaks"
  echo "       Either publish the dependency or drop the edge." >&2
  exit 1
fi

# ── Preflight 2: what is already on crates.io, and what would be created ────
# Two distinct questions, and the difference decides how fast the loop may run:
#
#   does <crate>@<version> exist?  -> whether to skip it (resumability)
#   does <crate> exist at all?     -> whether publishing it CREATES a crate
#
# crates.io rate-limits creation far harder than a new version, so the loop
# below paces creations slowly and moves through version bumps quickly. Asking
# only the first question, as this script originally did, cannot tell them apart
# and paces everything at the fast cadence, which 429s partway through a
# first-time workspace publish.
#
# A yanked version still answers 200 here, and that is the behaviour we want:
# yanking does not free a version number, so cargo would reject a re-publish.
crates_io_status() {
  local code
  if ! code="$(curl -sS --connect-timeout 15 --max-time 60 \
    --retry 3 --retry-delay 2 --retry-all-errors \
    -o /dev/null -w '%{http_code}' \
    -H "User-Agent: zeroclaw-release (https://github.com/zeroclaw-labs/zeroclaw)" \
    "https://crates.io/api/v1/crates/$1")"; then
    printf '000\n'
    return
  fi
  printf '%s\n' "$code"
}

wait_for_registry_version() {
  local crate="$1" attempt code
  local max_polls="${PUBLISH_INDEX_MAX_POLLS:-60}"
  for ((attempt = 1; attempt <= max_polls; attempt++)); do
    code="$(crates_io_status "${crate}/${VERSION}")"
    case "$code" in
      200) return 0 ;;
      404) sleep "${PUBLISH_INDEX_POLL_SECONDS:-5}" ;;
      *)
        echo "error: crates.io returned HTTP $code while waiting for ${crate}/${VERSION}." >&2
        return 1
        ;;
    esac
  done
  echo "error: ${crate}/${VERSION} was uploaded but did not appear in the crates.io API." >&2
  echo "       Re-run after the index catches up; the script will skip the existing version." >&2
  return 1
}

already=()
todo=()
creations=()
for crate in "${PUBLISHABLE[@]}"; do
  code="$(crates_io_status "${crate}/${VERSION}")"
  case "$code" in
    200)
      already+=("${crate}@${VERSION}")
      continue
      ;;
    404) todo+=("${crate}@${VERSION}") ;;
    *)
      echo "error: crates.io returned HTTP $code for ${crate}/${VERSION}; refusing to guess." >&2
      exit 1
      ;;
  esac

  crate_code="$(crates_io_status "${crate}")"
  case "$crate_code" in
    200) ;;
    404) creations+=("$crate") ;;
    *)
      echo "error: crates.io returned HTTP $crate_code for ${crate}; refusing to guess." >&2
      exit 1
      ;;
  esac
done

if [[ ${#already[@]} -gt 0 ]]; then
  echo "Already on crates.io, will be skipped (a yanked version also counts as present):"
  printf '  %s\n' "${already[@]}"
  echo
fi
if [[ ${#todo[@]} -eq 0 ]]; then
  echo "Nothing to publish — every crate is already at $VERSION on crates.io."
  exit 0
fi
echo "To publish:"
printf '  %s\n' "${todo[@]}"
if [[ ${#creations[@]} -gt 0 ]]; then
  echo
  echo "Of those, ${#creations[@]} do not exist on crates.io yet and will be CREATED:"
  printf '  %s\n' "${creations[@]}"
  echo "Creation is the rate-limited operation; this run will pace itself accordingly."
fi
echo

# ── Preflight 4: token present before anything irreversible starts ──────────
# Scope matters: a crate that does not yet exist needs `publish-new`.
# `publish-update` alone is enough only once every crate has been created.
if [[ $EXECUTE -eq 1 && -z "${CARGO_REGISTRY_TOKEN:-}" ]]; then
  echo "error: CARGO_REGISTRY_TOKEN is required for --execute." >&2
  echo "       Mint one at https://crates.io/settings/tokens." >&2
  echo "       First publish of a new crate needs the 'publish-new' scope;" >&2
  echo "       subsequent version bumps need 'publish-update'." >&2
  exit 1
fi

if [[ $EXECUTE -eq 0 ]]; then
  echo "── Dry run: packaging and verifying every crate (no upload) ──"
  # Cargo builds an ephemeral local registry for a multi-package dry run, so
  # unpublished workspace dependencies resolve without any upload. Select only
  # the coordinated release set; `--workspace` would also package independent
  # 0.1.x crates that merely share this repository.
  publish_args=(cargo publish --dry-run --locked)
  for crate in "${PUBLISHABLE[@]}"; do
    publish_args+=(-p "$crate")
  done
  # Default-feature verification cannot exercise every feature-gated include.
  # tests/architecture/publish_contract.rs statically guards that wider class.
  "${publish_args[@]}"
  echo
  echo "Dry run OK. Re-run with --execute to publish for real."
  exit 0
fi

# ── Publish, one crate at a time, in dependency order ───────────────────────
# Deliberately NOT `cargo publish --workspace`. Two reasons, both about what
# happens when a run dies partway through an irreversible sequence:
#
#   * Resumability must not rest on an assumption about how cargo treats a
#     version already in the index. Skipping is decided here, from the crates.io
#     query above, so a re-run demonstrably continues instead of restarting.
#   * crates.io rate-limits the creation of *new* crates far harder than new
#     versions (a small burst, then roughly one per 10 minutes). Publishing
#     sequentially lets the run pace itself and report exactly where it stopped.
#
# Pacing is REACTIVE, not pre-emptive, and the difference decides whether the
# job finishes. crates.io allows a burst of new crates before it starts
# throttling, so sleeping the full throttle interval after every creation pays
# the worst-case cost even when we are nowhere near the limit: 16 creations at
# ~10 minutes each is 165 minutes of pure sleep, which alone exceeds a sensible
# job timeout before a single byte is uploaded.
#
# So: publish immediately, and back off only when crates.io actually says 429.
# Fast when we have headroom, patient exactly when we do not.
DELAY="${PUBLISH_DELAY_SECONDS:-5}"
# How long to wait after being throttled. crates.io documents new-crate creation
# at roughly one per 10 minutes; 620s clears that with a margin.
THROTTLE_BACKOFF="${PUBLISH_THROTTLE_BACKOFF_SECONDS:-620}"
# Retries are the mechanism that gets a 16-crate first publish through, so this
# has to be generous enough to cover every creation past the burst allowance.
MAX_RETRIES="${PUBLISH_MAX_RETRIES:-20}"

is_creation() {
  local needle="$1" c
  for c in ${creations[@]+"${creations[@]}"}; do
    [[ "$c" == "$needle" ]] && return 0
  done
  return 1
}

# Topological order over the publishable set, so each crate's dependencies are
# already on the registry when it uploads. Derived from cargo metadata, never
# hand-maintained.
ORDER="$(python3 - "$META" "$VERSION" <<'PY'
import json, sys
meta = json.loads(sys.argv[1])
version = sys.argv[2]
pkgs = {p["name"]: p for p in meta["packages"]}
pub = {
    n for n, p in pkgs.items()
    if p["publish"] is None and p["version"] == version
}
deps = {
    n: sorted({d["name"] for d in p["dependencies"]
               if d["name"] in pub and d["kind"] in (None, "build")})
    for n, p in pkgs.items()
}
order, state = [], {}
def visit(n, trail=()):
    if state.get(n) == "done":
        return
    if state.get(n) == "visiting":
        sys.exit("dependency cycle: " + " -> ".join(trail + (n,)))
    state[n] = "visiting"
    for d in deps[n]:
        visit(d, trail + (n,))
    state[n] = "done"
    order.append(n)
for n in sorted(pub):
    visit(n)
print("\n".join(order))
PY
)" || {
  echo "error: could not compute publish order." >&2
  exit 1
}

published=0
skipped=0
echo "── Publishing to crates.io (IRREVERSIBLE) ──"
echo "   ${#todo[@]} to publish, ${#creations[@]} of them new crates"
echo "   pacing: ${DELAY}s between uploads, ${THROTTLE_BACKOFF}s backoff when throttled"
echo
for crate in $ORDER; do
  # `${arr[@]+...}` guards the empty-array expansion, which is an unbound-variable
  # error under `set -u` on macOS's bash 3.2.
  if printf '%s\n' ${already[@]+"${already[@]}"} | grep -qx "${crate}@${VERSION}"; then
    echo "skip  ${crate} ${VERSION} (already on crates.io)"
    skipped=$((skipped + 1))
    continue
  fi

  creating=0
  is_creation "$crate" && creating=1
  echo "publish ${crate} ${VERSION}$([[ $creating -eq 1 ]] && echo ' (new crate)')"

  # `--no-verify`: the preflight job already ran a full verifying dry run over
  # this exact tree, building every crate from its own tarball. Re-verifying all
  # 18 here would add ~40 minutes inside a rate-limited window, and a timeout
  # midway through an irreversible sequence is the worst outcome available.
  attempt=1
  log="$(mktemp)"
  while true; do
    if cargo publish -p "$crate" --locked --no-verify 2>&1 | tee "$log"; then
      break
    fi
    # Throttling is not failure. Back off and retry rather than abandoning a
    # sequence that cannot be rolled back.
    if grep -qiE '429|too many requests|rate limit' "$log" && [[ $attempt -lt $MAX_RETRIES ]]; then
      echo "  rate-limited by crates.io; waiting ${THROTTLE_BACKOFF}s then retrying (${attempt}/${MAX_RETRIES})"
      sleep "$THROTTLE_BACKOFF"
      attempt=$((attempt + 1))
      continue
    fi
    rm -f "$log"
    echo >&2
    echo "error: publishing ${crate} failed." >&2
    echo "       ${published} crate(s) uploaded before this point and CANNOT be undone." >&2
    echo "       Fix the cause and re-run this script; it will skip what already landed." >&2
    exit 1
  done
  rm -f "$log"

  # A successful upload can precede registry/index visibility. The next crate
  # may depend on this exact version, so do not race its `cargo publish` against
  # propagation and misreport a dependency-resolution failure.
  wait_for_registry_version "$crate"

  published=$((published + 1))
  sleep "$DELAY"
done

echo
echo "Published ${published} crate(s) at ${VERSION}; skipped ${skipped} already present."
echo "Verify: cargo install zeroclaw --version $VERSION --locked"
