#!/usr/bin/env bash
# Build the production aarch64 binary on pve-compute instead of on the box that
# serves the agent.
#
# WHY THIS EXISTS
#     The OCI host runs the daemon *and* the Hermes gateway, and every process in
#     that user session shares one cgroup capped at MemoryHigh=6G. The final LTO
#     link of the `zeroclaw` binary peaks near 6 GB on its own, so a local
#     release build pushes the cgroup over its limit and the kernel throttles
#     every allocation: one observed run logged 952,664 `memory.events high`
#     entries and spent 76 minutes wall clock to accumulate 8 minutes of CPU,
#     with the system otherwise 90%+ idle. It is not slow because compiling is
#     slow. It is slow because the kernel is deliberately pausing it, while the
#     same throttling squeezes the live agent sharing that cgroup.
#
#     pve-compute has 28 cores and 80 GB and no such cap.
#
# THE GLIBC TRAP
#     pve-compute runs Debian 13 (glibc 2.41); the OCI host runs Ubuntu 24.04
#     (glibc 2.39). A plain cross-build links against the *builder's* glibc, so
#     the binary loads fine on the builder and dies on the target with
#     `version 'GLIBC_2.41' not found`. `cargo-zigbuild` accepts the glibc
#     version inside the target triple and pins it, which is the whole reason
#     zig is installed there rather than gcc-aarch64-linux-gnu.
#
#     The check at the end is not decoration: it reads the highest GLIBC_* symbol
#     the artifact actually requires and refuses to hand over a binary that the
#     production host cannot load. A build that "succeeded" and then fails at
#     exec is worse than one that never finished.
set -euo pipefail

readonly HOST="${ZC_BUILD_HOST:-pve-compute}"
readonly REMOTE_SRC="${ZC_REMOTE_SRC:-/root/zc-src}"
readonly TARGET_GLIBC="${ZC_TARGET_GLIBC:-2.39}"
readonly TRIPLE="aarch64-unknown-linux-gnu"
readonly FEATURES="${ZC_FEATURES:-whatsapp-web}"
readonly LOCAL_OUT="${1:-target/release/zeroclaw-remote}"

log() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
die() { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

command -v rsync >/dev/null || die "rsync is required"
ssh -o ConnectTimeout=10 -o BatchMode=yes "$HOST" true 2>/dev/null \
  || die "cannot reach $HOST over ssh"

# The source of truth for what gets built is this working tree, so ship it
# rather than trusting whatever the remote happens to have checked out. Excludes
# keep the transfer to source: target/ alone is tens of gigabytes.
log "syncing working tree to $HOST:$REMOTE_SRC"
rsync -az --delete \
  --exclude 'target/' --exclude '.git/' --exclude 'node_modules/' \
  --exclude '*.log' --exclude 'web/dist/' \
  ./ "$HOST:$REMOTE_SRC/"

log "building $TRIPLE (glibc pinned to $TARGET_GLIBC) with $(nproc 2>/dev/null || echo '?') local / 28 remote cores"
ssh "$HOST" bash -euo pipefail <<REMOTE
export PATH="\$HOME/.cargo/bin:/usr/local/bin:\$PATH"
export CARGO_INCREMENTAL=0
cd "$REMOTE_SRC"

# The repo pins rustc 1.96.1; a mismatch invalidates the build the same way it
# invalidates local validation.
have="\$(rustc --version | awk '{print \$2}')"
[ "\$have" = "1.96.1" ] || { echo "rustc \$have != 1.96.1" >&2; exit 1; }

cargo zigbuild --release --bin zeroclaw \
  --features "$FEATURES" \
  --target "$TRIPLE.$TARGET_GLIBC"
REMOTE

readonly REMOTE_BIN="$REMOTE_SRC/target/$TRIPLE/release/zeroclaw"

# Verify on the builder, before transfer: if the artifact needs a newer glibc
# than production has, stop here instead of shipping a binary that cannot exec.
log "checking glibc requirements against target ($TARGET_GLIBC)"
highest="$(ssh "$HOST" "objdump -T '$REMOTE_BIN' | grep -oE 'GLIBC_2\.[0-9]+' | sort -uV | tail -1")"
[ -n "$highest" ] || die "could not read glibc symbols from $REMOTE_BIN"
need="${highest#GLIBC_}"
if [ "$(printf '%s\n%s\n' "$need" "$TARGET_GLIBC" | sort -V | tail -1)" != "$TARGET_GLIBC" ]; then
  die "binary requires glibc $need but production has $TARGET_GLIBC — it would fail to exec"
fi
log "highest required: $highest (ok)"

mkdir -p "$(dirname "$LOCAL_OUT")"
log "fetching binary"
rsync -z --progress "$HOST:$REMOTE_BIN" "$LOCAL_OUT"
chmod +x "$LOCAL_OUT"

file "$LOCAL_OUT" | grep -q 'ARM aarch64' || die "fetched artifact is not aarch64"
log "built $(du -h "$LOCAL_OUT" | cut -f1)  md5=$(md5sum "$LOCAL_OUT" | cut -c1-12)"
log "install with: sudo install -m755 $LOCAL_OUT /usr/local/lib/zeroclaw/zeroclaw-bin"
