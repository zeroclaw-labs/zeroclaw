#!/usr/bin/env bash
# deploy-local.sh — build, verify and install ZeroClaw on the machine it runs on.
#
# WHY THIS EXISTS
#
# Deploying by hand is how a live agent goes mute. The failure that motivated
# this script: `cargo build --release --bin zeroclaw` (no `--features`) produces
# a binary that starts *cleanly* — service `active`, `zeroclaw doctor` reporting
# zero errors, config untouched — but with NO WhatsApp channel compiled in. The
# daemon then logs:
#
#     No active channels to supervise (none configured or all disabled).
#
# which blames the config while the real cause is a missing compile-time
# feature. Nothing in the normal health surface catches it.
#
# So this script refuses to install a binary until it has proven, against the
# artifact itself, that every channel the config enables is actually present.
#
# WHAT IT GUARANTEES
#
#   1. Features are derived FROM THE CONFIG, not from memory or a habit.
#   2. The built binary is inspected for those channels before it is installed.
#   3. The running binary and the paired session are backed up first.
#   4. After restart it verifies the channel really came up — and if it did
#      not, it rolls back automatically and restarts the previous binary.
#
# A deploy that ends "ok" here means the channel was observed alive, not that
# a command exited 0.
#
# Usage:
#   ./scripts/deploy-local.sh                 # build, verify, install, restart
#   ./scripts/deploy-local.sh --dry-run       # build + verify only, no install
#   ./scripts/deploy-local.sh --skip-build    # verify/install an existing binary
#   ./scripts/deploy-local.sh --skip-tests    # skip the test gate (validated elsewhere)
#
# Env vars:
#   ZC_CONFIG      — config file            (default: ~/.zeroclaw/config.toml)
#   ZC_TARGET      — installed binary path  (default: /usr/local/lib/zeroclaw/zeroclaw-bin)
#   ZC_SERVICE     — systemd --user unit    (default: zeroclaw)
#   ZC_FEATURES    — override feature list  (default: derived from config)
#   ZC_HEALTH_WAIT — seconds to wait for the channel (default: 60)
#   ZC_MIN_DISK_GB — minimum free space required   (default: 6)
#   ZC_REMOTE_HOST — cross-build host for --remote (default: pve)
#   ZC_REMOTE_CT   — LXC id on that host           (default: 104)
#   ZC_GLIBC       — target glibc version          (default: auto-detected)

set -euo pipefail

ZC_CONFIG="${ZC_CONFIG:-$HOME/.zeroclaw/config.toml}"
ZC_TARGET="${ZC_TARGET:-/usr/local/lib/zeroclaw/zeroclaw-bin}"
ZC_SERVICE="${ZC_SERVICE:-zeroclaw}"
ZC_HEALTH_WAIT="${ZC_HEALTH_WAIT:-60}"
# A release build with lto=fat needs several GB of scratch space.
ZC_MIN_DISK_GB="${ZC_MIN_DISK_GB:-6}"
ZC_REMOTE_HOST="${ZC_REMOTE_HOST:-pve}"
ZC_REMOTE_CT="${ZC_REMOTE_CT:-104}"
# Pin the produced binary to the glibc THIS machine has. The cross-build host
# runs a newer glibc; without the pin the binary links against the build host's
# version and dies here at load time with "GLIBC_2.xx not found".
ZC_GLIBC="${ZC_GLIBC:-$(ldd --version 2>/dev/null | head -1 | grep -oE '[0-9]+\.[0-9]+$' || echo 2.39)}"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

DRY_RUN=0
SKIP_BUILD=0
SKIP_TESTS=0
REMOTE=0
for arg in "$@"; do
  case "$arg" in
    --dry-run)    DRY_RUN=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --skip-tests) SKIP_TESTS=1 ;;
    --remote)     REMOTE=1 ;;
    -h|--help)    sed -n '2,50p' "${BASH_SOURCE[0]}"; exit 0 ;;
    *) echo "unknown argument: $arg" >&2; exit 2 ;;
  esac
done

say()  { printf '\n\033[1m%s\033[0m\n' "$*"; }
ok()   { printf '  \033[32m✓\033[0m %s\n' "$*"; }
bad()  { printf '  \033[31m✗\033[0m %s\n' "$*" >&2; }
info() { printf '    %s\n' "$*"; }

say "1/8  Preflight: disk space"
# A release build needs several GB. When the disk fills mid-build, cargo fails
# at the LINK step with "No space left on device" — and any wrapper that greps
# for a success marker reports it as a *test* or *build* failure with no hint of
# the real cause. Measured on this host: a full disk produced a 5/5 "test
# failure" rate that vanished entirely once space was freed. Check first.
AVAIL_MB=$(df -Pm "$REPO_ROOT" | awk 'NR==2{print $4}')
MIN_MB=$((ZC_MIN_DISK_GB * 1024))
if (( AVAIL_MB < MIN_MB )); then
  bad "only $((AVAIL_MB / 1024))G free at $REPO_ROOT; need at least ${ZC_MIN_DISK_GB}G"
  info "free space with: cargo clean --profile dev   (keeps target/release)"
  info "override the threshold with ZC_MIN_DISK_GB=<n>"
  exit 1
fi
ok "$((AVAIL_MB / 1024))G free (need ${ZC_MIN_DISK_GB}G)"

# ---------------------------------------------------------------------------
# 2. Derive required features FROM THE CONFIG
#
# This is the step whose absence caused the outage. Each entry maps a config
# predicate to the cargo feature that compiles the corresponding channel, plus
# a symbol that must appear in the finished binary. Deriving from config means
# enabling a channel in TOML can never silently outrun the build.
# ---------------------------------------------------------------------------
declare -a REQUIRED_FEATURES=()
declare -a REQUIRED_CHANNELS=()

config_enables() {
  # $1 = TOML table path fragment, e.g. "channels.whatsapp"
  # True when the table exists AND is not explicitly disabled.
  python3 - "$ZC_CONFIG" "$1" <<'PY'
import sys, tomllib
try:
    cfg = tomllib.load(open(sys.argv[1], "rb"))
except Exception:
    sys.exit(1)
node = cfg
for part in sys.argv[2].split("."):
    if not isinstance(node, dict) or part not in node:
        sys.exit(1)
    node = node[part]
# A channel table holds named instances: enabled when any instance is on.
if isinstance(node, dict):
    for inst in node.values():
        if isinstance(inst, dict) and inst.get("enabled", True):
            sys.exit(0)
sys.exit(1)
PY
}

say "2/8  Deriving features from $ZC_CONFIG"
if [[ ! -f "$ZC_CONFIG" ]]; then
  bad "config not found: $ZC_CONFIG"; exit 1
fi

# channel table            cargo feature        symbol expected in the binary
CHANNEL_MAP=(
  "channels.whatsapp|whatsapp-web|whatsapp_web"
  "channels.telegram|channel-telegram|telegram"
  "channels.discord|channel-discord|discord"
  "channels.slack|channel-slack|slack"
  "channels.matrix|channel-matrix|matrix"
  "channels.signal|channel-signal|signal"
  "channels.irc|channel-irc|irc"
  "channels.email|channel-email|email"
)

for entry in "${CHANNEL_MAP[@]}"; do
  IFS='|' read -r table feature symbol <<<"$entry"
  if config_enables "$table"; then
    REQUIRED_FEATURES+=("$feature")
    REQUIRED_CHANNELS+=("$symbol")
    ok "$table enabled → needs feature '$feature'"
  fi
done

if [[ -n "${ZC_FEATURES:-}" ]]; then
  FEATURES="$ZC_FEATURES"
  info "feature list overridden by ZC_FEATURES"
elif ((${#REQUIRED_FEATURES[@]})); then
  FEATURES="$(IFS=,; echo "${REQUIRED_FEATURES[*]}")"
else
  FEATURES=""
  info "no non-default channels enabled; building with defaults"
fi
[[ -n "$FEATURES" ]] && info "features: $FEATURES"

# ---------------------------------------------------------------------------
# 3. Build
# ---------------------------------------------------------------------------
BUILT="$REPO_ROOT/target/release/zeroclaw"
if ((SKIP_BUILD)); then
  say "3/8  Skipping build (--skip-build)"
  [[ -f "$BUILT" ]] || { bad "no binary at $BUILT"; exit 1; }
elif ((REMOTE)); then
  # Cross-build on a much larger x86 host. Measured on this infra: 14m there
  # versus ~28m natively on 4 ARM cores, and it leaves the local disk alone.
  say "3/8  Cross-building on $ZC_REMOTE_HOST (LXC $ZC_REMOTE_CT) for glibc $ZC_GLIBC"
  TRIPLE="aarch64-unknown-linux-gnu"
  cd "$REPO_ROOT"

  info "syncing source"
  rsync -az --delete --exclude=target --exclude=.git \
    -e "ssh -o ConnectTimeout=10" ./ "${ZC_REMOTE_HOST}:/tmp/zc-src/" >/dev/null
  # The container is not directly reachable over SSH; stream through the host.
  ssh "$ZC_REMOTE_HOST" "tar czf - -C /tmp zc-src | pct exec $ZC_REMOTE_CT -- bash -c 'cd /root && tar xzf -'" >/dev/null

  info "building (this takes ~15 min)"
  REMOTE_CMD="export PATH=/root/.cargo/bin:/usr/local/bin:\$PATH CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS=24; \
cd /root/zc-src && cargo zigbuild --release --bin zeroclaw --target ${TRIPLE}.${ZC_GLIBC}"
  [[ -n "$FEATURES" ]] && REMOTE_CMD="$REMOTE_CMD --features '$FEATURES'"
  if ! ssh "$ZC_REMOTE_HOST" "pct exec $ZC_REMOTE_CT -- bash -lc \"$REMOTE_CMD\"" 2>&1 | tail -3; then
    bad "remote cross-build failed"; exit 1
  fi

  info "retrieving artifact"
  mkdir -p "$(dirname "$BUILT")"
  ssh "$ZC_REMOTE_HOST" "pct pull $ZC_REMOTE_CT /root/zc-src/target/${TRIPLE}/release/zeroclaw /tmp/zc-cross" >/dev/null
  scp -q "${ZC_REMOTE_HOST}:/tmp/zc-cross" "$BUILT"
  chmod +x "$BUILT"
  ok "cross-build retrieved ($(stat -c%s "$BUILT") bytes)"

  # A cross-built binary can be perfectly valid ELF and still refuse to start
  # here if it was linked against a newer glibc. Prove the ABI before trusting
  # any later step.
  MAXG=$(objdump -T "$BUILT" 2>/dev/null | grep -o 'GLIBC_[0-9.]*' | sort -Vu | tail -1)
  if [[ -n "$MAXG" ]]; then
    if printf '%s\n%s\n' "${MAXG#GLIBC_}" "$ZC_GLIBC" | sort -VC; then
      ok "glibc requirement $MAXG <= host $ZC_GLIBC"
    else
      bad "binary demands $MAXG but this host has glibc $ZC_GLIBC — it would not start"
      exit 1
    fi
  fi
  if ! "$BUILT" --version >/dev/null 2>&1; then
    bad "cross-built binary does not execute on this host"
    exit 1
  fi
  ok "binary executes here: $("$BUILT" --version 2>&1 | head -1)"
else
  say "3/8  Building release binary (this is slow with lto=fat)"
  cd "$REPO_ROOT"
  if [[ -n "$FEATURES" ]]; then
    cargo build --release --bin zeroclaw --features "$FEATURES"
  else
    cargo build --release --bin zeroclaw
  fi
  ok "build finished"
fi

# ---------------------------------------------------------------------------
# 4. Quality gate — do not ship code that fails its own tests
#
# Verifying the artifact contains the right channels says nothing about whether
# the code inside works. Skipped with --skip-tests when deploying a build that
# was already validated elsewhere (e.g. on a faster machine).
# ---------------------------------------------------------------------------
if ((SKIP_TESTS)); then
  say "4/8  Skipping tests (--skip-tests)"
else
  say "4/8  Running tests for the channels crate"
  cd "$REPO_ROOT"
  TEST_FEATS=()
  [[ -n "$FEATURES" ]] && TEST_FEATS=(--features "$FEATURES")
  if cargo test -p zeroclaw-channels --lib "${TEST_FEATS[@]}" 2>&1 | tee /tmp/zc-deploy-tests.log | tail -3; then
    if grep -q "test result: FAILED" /tmp/zc-deploy-tests.log; then
      bad "tests FAILED — refusing to install"
      grep -E "^test .*FAILED" /tmp/zc-deploy-tests.log | head -5
      info "re-run with --skip-tests only if these failures are known-unrelated"
      exit 1
    fi
    ok "tests passed"
  else
    bad "test run could not complete (build error? disk?) — refusing to install"
    tail -5 /tmp/zc-deploy-tests.log
    exit 1
  fi
fi

# ---------------------------------------------------------------------------
# 5. VERIFY THE ARTIFACT — the gate that would have caught the outage
#
# Counting `strings | grep -c <symbol>` hits is NOT reliable: the linker packs
# string literals into large blobs, so one "line" of output can hold hundreds of
# distinct literals. Two binaries with identical functionality can report wildly
# different counts (measured: 4 vs 12 for the same working channel).
#
# The dependable signal is a literal only the compiled channel can produce —
# a runtime message from inside its own source file. When the feature is off the
# module is never compiled, so its internal strings cannot exist at all.
# ---------------------------------------------------------------------------
say "5/8  Verifying channels are present in the artifact"
FAILED=0
for symbol in "${REQUIRED_CHANNELS[@]}"; do
  # A log/error literal emitted from inside the channel's own module. Present
  # only when that module was actually built.
  case "$symbol" in
    whatsapp_web) probe="found existing session, loading device" ;;
    telegram)     probe="Telegram channel requires" ;;
    discord)      probe="Discord channel requires" ;;
    slack)        probe="Slack channel requires" ;;
    matrix)       probe="Matrix channel requires" ;;
    signal)       probe="Signal channel requires" ;;
    irc)          probe="IRC channel requires" ;;
    email)        probe="IMAP login successful" ;;
    *)            probe="" ;;
  esac

  if [[ -z "$probe" ]]; then
    info "$symbol: no probe defined, skipping deep check"
    continue
  fi

  # NOTE: `strings ... | grep -q` is a trap under `set -o pipefail`. `grep -q`
  # exits at the first match and closes the pipe, so `strings` dies with
  # SIGPIPE (141) and pipefail propagates that as the pipeline's status —
  # turning a successful find into a reported failure. Materialise the output
  # first, then match, so the exit status reflects the match and nothing else.
  if strings "$BUILT" 2>/dev/null | grep -cF "$probe" | grep -qv '^0$'; then
    ok "$symbol compiled in (found its runtime strings)"
  else
    bad "$symbol: NOT compiled in — missing probe literal \"$probe\""
    FAILED=1
  fi
done

if ((FAILED)); then
  bad "artifact verification FAILED — refusing to install"
  info "the binary would start cleanly and leave the channel dead"
  info "check that --features covers every enabled channel"
  exit 1
fi

if ((DRY_RUN)); then
  say "Dry run complete — verified, not installed"
  exit 0
fi

# ---------------------------------------------------------------------------
# 6. Back up the running binary and the paired session
# ---------------------------------------------------------------------------
say "6/8  Backing up current binary and session"
STAMP="$(date +%Y%m%d-%H%M%S)"
BACKUP=""
if [[ -f "$ZC_TARGET" ]]; then
  BACKUP="${ZC_TARGET}.pre-deploy-${STAMP}"
  sudo cp -a "$ZC_TARGET" "$BACKUP"
  ok "binary → $(basename "$BACKUP")"
fi

SESSION_DIR="$HOME/.zeroclaw/state"
BACKUP_DIR="$SESSION_DIR/backups"
mkdir -p "$BACKUP_DIR"
while IFS= read -r db; do
  [[ -f "$db" ]] || continue
  out="$BACKUP_DIR/$(basename "$(dirname "$db")")-predeploy-${STAMP}.db"
  # VACUUM INTO takes a consistent snapshot without stopping the daemon.
  if sqlite3 "file:${db}?mode=ro" "VACUUM INTO '$out';" 2>/dev/null; then
    ok "session → $(basename "$out")"
  fi
done < <(find "$SESSION_DIR" -name 'session.db' -maxdepth 3 2>/dev/null)

# ---------------------------------------------------------------------------
# 7. Install — and decide whether restarting is safe
#
# The health check in step 8 proves the deploy by waiting for the channel to
# connect, and rolls back when it doesn't. That is the right behaviour when a
# connection is *possible*. It is actively harmful when it is not.
#
# A WhatsApp session whose device row has no phone number has been revoked
# server-side: the credentials are gone and no amount of restarting will bring
# the channel up until a human re-pairs the device. Run the normal path against
# that state and every deploy costs four device reconnections — start the new
# binary, fail the health check, reinstall the old one, start it again — each
# one more evidence to Meta that this account behaves like a bot. That is not
# hypothetical: 23 restarts inside ten hours preceded the revocation this check
# now detects, and the automatic rollback supplied a share of them.
#
# So when the session is already revoked, install the binary and stop. The
# deploy still happens; the code is in place for whenever pairing is restored.
# What is skipped is the part that cannot succeed and is not free to attempt.
# ---------------------------------------------------------------------------
say "7/8  Installing"

# Returns 0 when a paired session exists, 1 when every session is revoked.
# Absence of any session file is NOT revocation — a first-ever deploy has no
# session yet and must be allowed to start and present a QR code.
session_is_paired() {
  local found_any=0 db pn
  while IFS= read -r db; do
    [[ -f "$db" ]] || continue
    found_any=1
    # `pn` is the device's own phone number, written at pairing and cleared
    # when the server revokes. Query read-only so a live daemon is undisturbed.
    pn=$(sqlite3 "file:${db}?mode=ro" \
           "SELECT COALESCE(pn, '') FROM device LIMIT 1;" 2>/dev/null || true)
    [[ -n "$pn" ]] && return 0
  done < <(find "$HOME/.zeroclaw/state" -name 'session.db' -maxdepth 3 2>/dev/null)
  (( found_any )) && return 1 || return 0
}

sudo install -m 755 "$BUILT" "$ZC_TARGET"
ok "installed to $ZC_TARGET"

if ! session_is_paired; then
  bad "session is revoked — installed, NOT restarting"
  info "every session on this host has an empty device.pn, which means the"
  info "server dropped the pairing. Restarting cannot reconnect and each"
  info "attempt makes the account look more automated."
  info ""
  info "re-pair first, then:  systemctl --user start $ZC_SERVICE"
  exit 0
fi

systemctl --user restart "$ZC_SERVICE"
ok "restarted $ZC_SERVICE"

# ---------------------------------------------------------------------------
# 8. Prove the channel actually came up — else roll back
#
# "systemctl is-active" is NOT proof: the broken build reported active while
# serving nothing. The real evidence is the channel banner in the journal plus
# an established socket.
# ---------------------------------------------------------------------------
say "8/8  Verifying the channel came up (waiting up to ${ZC_HEALTH_WAIT}s)"
# The startup banner only proves the daemon *tried* to start the channel — it
# prints before any network handshake. A binary can print "Channels: whatsapp"
# and then never reach WhatsApp at all (observed: a durability hook registered
# against a backend lacking the pending-inbound methods wedges the receive path
# while the banner still looks perfect). So the banner is necessary, not
# sufficient: also require a live socket to the messaging backend before
# calling the deploy good.
DEADLINE=$((SECONDS + ZC_HEALTH_WAIT))
HEALTHY=0
BANNER_SEEN=""
while (( SECONDS < DEADLINE )); do
  sleep 5
  banner=$(journalctl --user -u "$ZC_SERVICE" --since "-2 min" --no-pager 2>/dev/null \
             | grep -o 'Channels: .*' | tail -1 || true)
  no_chan=$(journalctl --user -u "$ZC_SERVICE" --since "-2 min" --no-pager 2>/dev/null \
             | grep -c 'No active channels to supervise' || true)

  if (( no_chan > 0 )); then
    bad "daemon reports 'No active channels to supervise'"
    break
  fi
  if [[ -n "$banner" ]]; then
    [[ -z "$BANNER_SEEN" ]] && { BANNER_SEEN="$banner"; ok "$banner"; }
    # Count sockets owned by the daemon, not by this script: match the process
    # name column first, then require an established TLS port. Deliberately NOT
    # matched on a peer IP prefix — Meta serves WhatsApp from several ranges
    # (157.240.x and 57.144.x both observed on this host within one hour), so a
    # prefix filter reports "0 connections" for a perfectly healthy channel and
    # triggers a false rollback.
    estab=$(ss -tnp 2>/dev/null | grep 'zeroclaw' | grep ':443' | grep -c ESTAB || true)
    if (( estab > 0 )); then
      ok "channel connected ($estab established connection(s))"
      HEALTHY=1
      break
    fi
    info "banner up, waiting for the channel to connect..."
  fi
done

if (( HEALTHY )); then
  say "Deploy OK — channel verified live"
  exit 0
fi

if [[ -n "$BANNER_SEEN" ]]; then
  bad "channel announced itself but never established a connection"
fi

# --- rollback ---
bad "channel did NOT come up — rolling back"
if [[ -n "$BACKUP" && -f "$BACKUP" ]]; then
  sudo install -m 755 "$BACKUP" "$ZC_TARGET"
  systemctl --user restart "$ZC_SERVICE"
  sleep 20
  banner=$(journalctl --user -u "$ZC_SERVICE" --since "-1 min" --no-pager 2>/dev/null \
             | grep -o 'Channels: .*' | tail -1 || true)
  if [[ -n "$banner" ]]; then
    ok "rolled back and healthy again — $banner"
  else
    bad "ROLLBACK DID NOT RECOVER THE CHANNEL — needs a human"
  fi
else
  bad "no backup available to roll back to"
fi
exit 1
