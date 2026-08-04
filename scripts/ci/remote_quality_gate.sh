#!/usr/bin/env bash
# Run the repo's quality gate on pve-compute instead of on the box that serves
# the agent.
#
# WHY
#     `.githooks/pre-push` runs `scripts/ci/rust_quality_gate.sh` locally. On the
#     OCI host that is the worst possible place for it: every process in this
#     user session shares one cgroup capped at MemoryHigh=6G, so a full rebuild
#     both takes ~1h45m of wall clock for ~9m of CPU (the kernel throttles every
#     allocation past the cap) and squeezes the live agent sharing the cgroup.
#     It also needs ~21 GB in target/, which has filled this disk to 100% twice
#     today — the second time surfacing as a truncated `rustc-LLVM E`, which is
#     "no space left on device", not a code error.
#
#     pve-compute has 28 cores, 80 GB and no cap. The gate belongs there.
#
# WHAT IT IS NOT
#     Not a way to skip the gate. The same fmt/clippy/test commands run, against
#     the same working tree, on the pinned toolchain; only the machine changes.
#     If the remote is unreachable it fails closed rather than waving the push
#     through, because a gate that silently disappears is worse than no gate.
set -euo pipefail

readonly HOST="${ZC_BUILD_HOST:-pve-compute}"
readonly REMOTE_SRC="${ZC_REMOTE_SRC:-/root/zc-src}"
readonly FEATURES="${ZC_FEATURES:-whatsapp-web}"

log() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31mFAIL:\033[0m %s\n' "$*" >&2; exit 1; }

ssh -o ConnectTimeout=10 -o BatchMode=yes "$HOST" true 2>/dev/null \
  || die "cannot reach $HOST — refusing to push unverified (set ZC_SKIP_REMOTE_GATE=1 only if you have run the gate yourself)"

log "syncing working tree to $HOST"
rsync -az --delete \
  --exclude 'target/' --exclude '.git/' --exclude 'node_modules/' \
  --exclude '*.log' --exclude 'web/dist/' \
  ./ "$HOST:$REMOTE_SRC/"

log "running quality gate on $HOST (28 cores)"
ssh "$HOST" bash -euo pipefail <<REMOTE
export PATH="\$HOME/.cargo/bin:/usr/local/bin:\$PATH"
export CARGO_INCREMENTAL=0
cd "$REMOTE_SRC"

# The repo pins rustc 1.96.1. A different compiler makes the whole run
# meaningless, so stop rather than report a green that means nothing.
have="\$(rustc --version | awk '{print \$2}')"
[ "\$have" = "1.96.1" ] || { echo "rustc \$have != pinned 1.96.1" >&2; exit 1; }

echo "--- fmt ---"
cargo fmt --all -- --check

# `zeroclaw-desktop` is excluded the way every upstream gate excludes it
# (rust_quality_gate.sh, .githooks/pre-push): it pulls tao -> dbus ->
# libdbus-sys, which needs the dbus-1 system library. That is a desktop GUI
# dependency with no bearing on the daemon this fork ships, and requiring it
# would mean installing a display stack on a headless build box.
echo "--- clippy ---"
cargo clippy --workspace --exclude zeroclaw-desktop --all-targets \
  --features "$FEATURES" -- -D warnings

# Serial: several tests in this workspace share global logging state and flake
# under the parallel runner, which produces failures that have nothing to do
# with the change being pushed.
echo "--- tests (serial) ---"
cargo test --locked --workspace --exclude zeroclaw-desktop \
  --features "$FEATURES" -- --test-threads=1
REMOTE

log "quality gate passed on $HOST"
