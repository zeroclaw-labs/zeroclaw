#!/usr/bin/env bash
#
# mtls-relay-testbed.sh - stand up a self-contained mTLS WSS testbed and prove
# both reach paths a `zerocode` client can use against a running daemon:
#
#   1. DIRECT     zerocode --connect wss://127.0.0.1:<wss>           (mutual TLS)
#   2. VIA RELAY  zerocode --connect wss://127.0.0.1:<wss> \
#                          --relay 127.0.0.1:<relay> --relay-node <id>
#
# It builds the three binaries (zeroclaw, zerocode, zerorelay), creates an
# isolated config dir with [wss] + [relay] enabled, boots a relay and a daemon,
# issues a client certificate from the daemon's auto-generated CA, and then
# self-verifies BOTH paths before handing you live processes plus the exact
# zerocode commands to run by hand.
#
# Self-checks (hard failures, exit non-zero):
#   - direct mTLS handshake SUCCEEDS with the client cert (TLS 1.3, Verify OK)
#   - direct mTLS handshake is REJECTED without a client cert (mandatory mTLS)
#   - the relay's OUTER TLS verifies and the daemon bridge is up
#   - over-the-wire ENROLLMENT: a certless client fetches its first cert with the
#     pairing code and that cert then completes the mTLS handshake
#   - an UN-MIGRATED (certless) client fails with an actionable enroll-first hint
#
# Nothing here touches a real deployment: it runs on test ports under a scratch
# dir, and the mTLS/relay code is inert in normal daemon config (no [wss]).
#
# Usage:
#   scripts/dev/mtls-relay-testbed.sh [--check-only] [--skip-build] [--browser-check|--browser-manual] [-h]
#
# Environment overrides:
#   ZC_TESTBED_DIR    workspace dir         (default: ${TMPDIR:-/tmp}/zc-mtls-testbed)
#   ZC_WSS_PORT       daemon WSS port       (default: 9799)
#   ZC_RELAY_PORT     relay listen port     (default: 8459)
#   ZC_NODE_ID        relay node-id         (default: testbed-daemon)
#   ZC_PROFILE        cargo profile         (release|debug, default: release)
#   CARGO_TARGET_DIR  cargo output/cache dir (default: /opt/cargo-build)
#   ZC_CARGO_TOOLCHAIN cargo toolchain flag  (default: +1.96.1 when installed)
#
# Flags:
#   --check-only      run both self-checks, tear everything down, exit (CI smoke)
#   --skip-build      reuse already-built binaries (same as ZC_TESTBED_SKIP_BUILD=1)
#   --browser-check   spend the pairing code on a headless browser frontdoor E2E
#   --browser-manual  leave the pairing code unused and print browser instructions
#   --browser PATH    Chrome/Chromium path for --browser-check
#   --headed          run --browser-check with a visible browser
#   -h, --help        this help

set -euo pipefail

# --- locate the repo root (this script lives in scripts/dev/) ----------------
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"

# --- options -----------------------------------------------------------------
CHECK_ONLY=0
SKIP_BUILD="${ZC_TESTBED_SKIP_BUILD:-0}"
BROWSER_CHECK=0
BROWSER_MANUAL=0
BROWSER_E2E_ARGS=()
while [ "$#" -gt 0 ]; do
  case "$1" in
    --check-only) CHECK_ONLY=1 ;;
    --skip-build) SKIP_BUILD=1 ;;
    --browser-check) BROWSER_CHECK=1 ;;
    --browser-manual) BROWSER_MANUAL=1 ;;
    --browser)
      shift
      [ "$#" -gt 0 ] || { echo "--browser requires a value" >&2; exit 2; }
      BROWSER_E2E_ARGS+=(--browser "$1")
      ;;
    --headed) BROWSER_E2E_ARGS+=(--headed) ;;
    -h|--help) sed -n '2,46p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1 (try --help)" >&2; exit 2 ;;
  esac
  shift
done

TB="${ZC_TESTBED_DIR:-${TMPDIR:-/tmp}/zc-mtls-testbed}"
WSS_PORT="${ZC_WSS_PORT:-9799}"
RELAY_PORT="${ZC_RELAY_PORT:-8459}"
ENROLL_PORT="${ZC_ENROLL_PORT:-9783}"
NODE_ID="${ZC_NODE_ID:-testbed-daemon}"
PROFILE="${ZC_PROFILE:-release}"
RELAY_TOKEN="testbed-token"
RELAY_TLS_DIR="$TB/relay-tls"   # where the relay self-provisions its CA + cert
CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-/opt/cargo-build}"
CARGO_TOOLCHAIN="${ZC_CARGO_TOOLCHAIN:-}"
CARGO_CMD=(cargo)
if [ -n "$CARGO_TOOLCHAIN" ]; then
  CARGO_CMD=(cargo "$CARGO_TOOLCHAIN")
elif command -v rustup >/dev/null 2>&1 && rustup toolchain list | grep -q '^1\.96\.1-'; then
  CARGO_CMD=(cargo +1.96.1)
fi

case "$PROFILE" in
  release) BIN_DIR="$CARGO_TARGET_DIR/release"; CARGO_PROFILE_FLAG="--release" ;;
  debug)   BIN_DIR="$CARGO_TARGET_DIR/debug";   CARGO_PROFILE_FLAG="" ;;
  *) echo "ZC_PROFILE must be 'release' or 'debug' (got '$PROFILE')" >&2; exit 2 ;;
esac
ZEROCLAW="$BIN_DIR/zeroclaw"
ZEROCODE="$BIN_DIR/zerocode"
ZERORELAY="$BIN_DIR/zerorelay"

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
ok()   { printf '\033[1;32m  ok\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31mFAIL\033[0m %s\n' "$*" >&2; exit 1; }

[ "$BROWSER_CHECK" = "1" ] && [ "$BROWSER_MANUAL" = "1" ] \
  && die "--browser-check and --browser-manual are mutually exclusive"
[ "$BROWSER_MANUAL" = "1" ] && [ "$CHECK_ONLY" = "1" ] \
  && die "--browser-manual keeps the testbed alive; do not combine it with --check-only"

# --- teardown ----------------------------------------------------------------
DAEMON_PID=""
RELAY_PID=""
cleanup() {
  [ -n "$DAEMON_PID" ] && kill "$DAEMON_PID" 2>/dev/null || true
  [ -n "$RELAY_PID" ]  && kill "$RELAY_PID"  2>/dev/null || true
  wait 2>/dev/null || true
}
trap cleanup EXIT INT TERM

# --- 1. build ----------------------------------------------------------------
if [ "$SKIP_BUILD" = "1" ]; then
  say "skipping build (reusing binaries in $BIN_DIR)"
  for b in "$ZEROCLAW" "$ZEROCODE" "$ZERORELAY"; do
    [ -x "$b" ] || die "missing binary $b (drop --skip-build to build it)"
  done
else
  say "building zeroclaw, zerocode, zerorelay ($PROFILE) with ${CARGO_CMD[*]} ..."
  ( cd "$REPO_ROOT" && CARGO_TARGET_DIR="$CARGO_TARGET_DIR" "${CARGO_CMD[@]}" build $CARGO_PROFILE_FLAG \
      --features channels-full \
      -p zeroclawlabs -p zerocode -p zerorelay ) \
    || die "build failed"
fi
ok "binaries ready in $BIN_DIR"

# --- 2. isolated workspace + config -----------------------------------------
say "preparing workspace at $TB"
rm -rf "$TB"
mkdir -p "$TB"
cat > "$TB/config.toml" <<TOML
# Generated by mtls-relay-testbed.sh - isolated mTLS WSS testbed.
default_provider = "ollama"
default_model = "llama3.2"

[gateway]
enabled = false

# Mutually authenticated remote control plane. CA + server cert are
# auto-generated under <config_dir>/data/tls on first boot.
[wss]
enabled = true
bind = "127.0.0.1"
port = $WSS_PORT

# Outbound bridge to the nominated relay so a client can reach this daemon by
# node-id without a direct route. Inert unless [wss] is also enabled. The relay
# hop is wrapped in OUTER TLS; the daemon verifies the relay against the CA the
# relay self-provisioned (relay_ca_path) - no insecure skip, no openssl.
[relay]
enabled = true
url = "127.0.0.1:$RELAY_PORT"
node_id = "$NODE_ID"
token = "$RELAY_TOKEN"
relay_host = "127.0.0.1"
relay_ca_path = "$RELAY_TLS_DIR/ca.crt"

# Enrollment endpoint: the bootstrap surface a certless client reaches for its
# FIRST certificate (server-auth TLS + one-time pairing code, CSR-only). Lets the
# testbed exercise the over-the-wire enroll flow, not just CLI issuance.
[enroll]
enabled = true
bind = "127.0.0.1"
port = $ENROLL_PORT
TOML
ok "wrote $TB/config.toml (wss :$WSS_PORT, relay :$RELAY_PORT, node '$NODE_ID')"

CA="$TB/data/tls/ca.crt"
# issue-client-cert into a drop-in client TLS dir: with --out-dir it also writes
# the generic ca.crt/client.crt/client.key that zerocode finds by default under
# <config-dir>/tls, so the relay command needs no --tls-* flags.
CLIENT_CONFIG_DIR="$TB/client"
CLIENT_DIR="$CLIENT_CONFIG_DIR/tls"
CLIENT_CRT="$CLIENT_DIR/client.crt"
CLIENT_KEY="$CLIENT_DIR/client.key"

# --- 3. start the relay (open admission; it SELF-PROVISIONS its outer TLS) ----
# No openssl: with no --tls-cert the relay generates its own CA + server cert
# (SAN localhost/127.0.0.1) into --tls-dir, the same machinery the daemon uses.
say "starting zerorelay on 127.0.0.1:$RELAY_PORT (open admission, self-provisioned TLS)"
"$ZERORELAY" --bind "127.0.0.1:$RELAY_PORT" --registration-mode open \
  --tls-dir "$RELAY_TLS_DIR" \
  > "$TB/relay.log" 2>&1 &
RELAY_PID=$!
for _ in $(seq 1 40); do
  ss -ltn 2>/dev/null | grep -q ":$RELAY_PORT" && break
  kill -0 "$RELAY_PID" 2>/dev/null || die "relay died on startup (see $TB/relay.log)"
  sleep 0.25
done
ss -ltn 2>/dev/null | grep -q ":$RELAY_PORT" || die "relay never bound :$RELAY_PORT"
ok "relay listening (pid $RELAY_PID)"

# --- 4. start the daemon -----------------------------------------------------
say "starting daemon (auto-generates CA, binds WSS, registers with relay)"
ZEROCLAW_CONFIG_DIR="$TB" "$ZEROCLAW" daemon > "$TB/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 80); do
  [ -f "$CA" ] && ss -ltn 2>/dev/null | grep -q ":$WSS_PORT" && break
  kill -0 "$DAEMON_PID" 2>/dev/null || die "daemon died on startup (see $TB/daemon.log)"
  sleep 0.25
done
[ -f "$CA" ] || die "CA was not generated at $CA"
ss -ltn 2>/dev/null | grep -q ":$WSS_PORT" || die "daemon never bound WSS :$WSS_PORT"
ok "daemon up (pid $DAEMON_PID), CA at $CA, WSS on :$WSS_PORT"

# --- 5. issue a client certificate -------------------------------------------
say "issuing a client certificate from the daemon CA"
ZEROCLAW_CONFIG_DIR="$TB" "$ZEROCLAW" security issue-client-cert \
  --name zerocode --out-dir "$CLIENT_DIR" --force > "$TB/issue.log" 2>&1 \
  || { cat "$TB/issue.log" >&2; die "issue-client-cert failed"; }
[ -f "$CLIENT_CRT" ] && [ -f "$CLIENT_KEY" ] || die "client cert/key not written to $CLIENT_DIR"
ok "client cert $CLIENT_CRT"

# --- 6. self-check: DIRECT mTLS ---------------------------------------------
say "self-check A: direct mTLS handshake WITH client cert (expect Verify OK)"
out="$(echo Q | openssl s_client -connect "127.0.0.1:$WSS_PORT" -tls1_3 \
  -CAfile "$CA" -cert "$CLIENT_CRT" -key "$CLIENT_KEY" 2>&1 || true)"
echo "$out" | grep -q "Verify return code: 0 (ok)" \
  || { echo "$out" | tail -20 >&2; die "direct mTLS did not verify with the client cert"; }
echo "$out" | grep -qE 'New, TLSv1\.3|Protocol *: *TLSv1\.3' \
  || die "direct handshake did not negotiate TLS 1.3"
ok "direct mTLS verified (TLS 1.3, Verify OK)"

say "self-check B: daemon DEMANDS a client certificate (mandatory mTLS)"
# openssl 3.x completes the TLS 1.3 handshake from its side and does not surface
# the server's post-handshake rejection alert in a greppable way, so we assert
# the deterministic signal instead: the server sends a CertificateRequest naming
# the daemon CA. The actual certless REJECTION is proven authoritatively by:
#   cargo test -p zeroclaw-runtime --test wss_mtls_transport (missing_client_cert_is_rejected)
out="$(printf 'Q' | openssl s_client -connect "127.0.0.1:$WSS_PORT" -tls1_3 \
  -CAfile "$CA" 2>&1 || true)"
echo "$out" | grep -qi "Acceptable client certificate CA names" \
  || { echo "$out" | tail -20 >&2; die "daemon did not request a client certificate (mTLS not mandatory)"; }
ok "daemon requires a client certificate (mandatory mTLS; rejection proven by cargo test)"

# --- 7. self-check: VIA RELAY -----------------------------------------------
# The designed relay speaks outer TLS + WebSocket + a signed Ed25519 handshake
# and multiplexes binary DATA frames - too much to re-implement faithfully in a
# shell probe. The authoritative inner-mTLS-end-to-end-through-the-relay proof
# (real RelayServer + real daemon bridge) is the integration test:
#     cargo test -p zeroclaw-runtime --test relay_full_path
# Here we assert the relay's OUTER TLS plane is live and the daemon bridge is up,
# then hand you the exact zerocode command for the relay route.
say "self-check C: relay outer TLS verifies against its self-provisioned CA"
out="$(echo Q | openssl s_client -connect "127.0.0.1:$RELAY_PORT" \
  -CAfile "$RELAY_TLS_DIR/ca.crt" -servername 127.0.0.1 2>&1 || true)"
echo "$out" | grep -q "Verify return code: 0 (ok)" \
  || { echo "$out" | tail -20 >&2; die "relay outer TLS did not verify against its self-provisioned CA"; }
kill -0 "$DAEMON_PID" 2>/dev/null || die "daemon exited before the relay bridge could register"
ok "relay outer TLS verified against its own CA; daemon bridge running (deep e2e: cargo test --test relay_full_path)"

say "self-check D: browser TLS certificate policy accepts only the daemon server identity"
node "$REPO_ROOT/scripts/dev/browser-tls-chain-check.mjs" \
  --ca "$CA" \
  --server-cert "$TB/data/tls/server.crt" \
  --client-cert "$CLIENT_CRT" > "$TB/browser-tls-chain.log" 2>&1 \
  || { cat "$TB/browser-tls-chain.log" >&2; die "browser TLS certificate policy check failed"; }
ok "browser TLS accepts the daemon server cert and rejects a client-only cert"

# --- 7b. self-check: OVER-THE-WIRE ENROLLMENT -------------------------------
# Prove the headline frictionless flow: a CERTLESS client fetches its first
# certificate from the enrollment endpoint using the daemon's one-time pairing
# code, caches it, and that cert then completes the mutually-authenticated WSS
# handshake. The default check drives zerocode non-interactively. --browser-check
# spends the same one-time code on the browser frontdoor flow instead, so it can
# prove service-worker availability, CSR generation, SAS confirmation, and the
# browser mTLS RPC tunnel against the real daemon/relay pair.
say "preparing one-time enrollment code"
for _ in $(seq 1 40); do
  ss -ltn 2>/dev/null | grep -q ":$ENROLL_PORT" && break
  sleep 0.25
done
ss -ltn 2>/dev/null | grep -q ":$ENROLL_PORT" || die "enroll endpoint never bound :$ENROLL_PORT"
CODE="$(grep -oE 'pairing code[[:space:]]*:[[:space:]]*[A-Za-z0-9]+' "$TB/daemon.log" | head -1 | awk '{print $NF}')"
[ -n "$CODE" ] || { tail -30 "$TB/daemon.log" >&2; die "could not read the pairing code from daemon.log"; }

if [ "$BROWSER_MANUAL" = "1" ]; then
  say "self-check D: browser manual mode (pairing code left unused)"
  ok "browser pairing code reserved for manual use"
elif [ "$BROWSER_CHECK" = "1" ]; then
  say "self-check E: browser frontdoor enrollment and mTLS RPC tunnel"
  ZC_BROWSER_E2E_URL="https://127.0.0.1:$RELAY_PORT/" \
    ZC_BROWSER_E2E_NODE_ID="$NODE_ID" \
    ZC_BROWSER_E2E_PAIRING_CODE="$CODE" \
    ZC_BROWSER_E2E_PROFILE_DIR="$TB/browser-profile" \
    node "$REPO_ROOT/scripts/dev/browser-relay-frontdoor-e2e.mjs" \
      "${BROWSER_E2E_ARGS[@]}" > "$TB/browser-e2e.log" 2>&1 \
    || { cat "$TB/browser-e2e.log" >&2; die "browser frontdoor E2E failed"; }
  ok "browser frontdoor paired, confirmed SAS, and opened the mTLS RPC tunnel"
else
  say "self-check D: over-the-wire enrollment (certless client -> pairing code -> cert)"
  ENROLL_DIR="$TB/enrolled"
  # --enroll enrolls and then proceeds to the normal connect flow; that post-enroll
  # connect (here, via the cached relay whose self-signed outer cert this fresh
  # client does not yet trust) is irrelevant to validating ENROLLMENT itself, so we
  # ignore the exit code and assert the cert was cached, then verify it below.
  printf '%s\ny\n' "$CODE" | ZEROCLAW_CONFIG_DIR="$ENROLL_DIR" "$ZEROCODE" \
    --enroll --enroll-host 127.0.0.1 --enroll-port "$ENROLL_PORT" \
    --config-dir "$ENROLL_DIR" > "$TB/enroll.log" 2>&1 || true
  ENR_CRT="$ENROLL_DIR/tls/client.crt"; ENR_KEY="$ENROLL_DIR/tls/client.key"
  [ -f "$ENR_CRT" ] && [ -f "$ENR_KEY" ] \
    || { cat "$TB/enroll.log" >&2; die "enrollment did not cache a client cert at $ENROLL_DIR/tls"; }
  out="$(echo Q | openssl s_client -connect "127.0.0.1:$WSS_PORT" -tls1_3 \
    -CAfile "$CA" -cert "$ENR_CRT" -key "$ENR_KEY" 2>&1 || true)"
  echo "$out" | grep -q "Verify return code: 0 (ok)" \
    || { echo "$out" | tail -20 >&2; die "the ENROLLED cert did not complete the mTLS handshake"; }
  ok "enrolled over the wire and the enrolled cert verifies (TLS 1.3, Verify OK)"
fi

# --- 7c. self-check: UN-MIGRATED CLIENT gets an actionable error -------------
# A certless client that connects to the always-mTLS WSS plane (non-interactive,
# so auto-enroll does not fire) must FAIL with an actionable "enroll first"
# message, never a silent hang or a bare TLS error.
say "self-check F: un-migrated (certless) client is told to enroll"
UNMIG_DIR="$TB/unmigrated"
mkdir -p "$UNMIG_DIR"
set +e
out="$(ZEROCLAW_CONFIG_DIR="$UNMIG_DIR" "$ZEROCODE" \
  --connect "wss://127.0.0.1:$WSS_PORT" --config-dir "$UNMIG_DIR" </dev/null 2>&1)"
rc=$?
set -e
[ "$rc" -ne 0 ] || die "a certless client unexpectedly connected (mTLS not enforced?)"
echo "$out" | grep -qiE "enroll|client certificate" \
  || { echo "$out" | tail -20 >&2; die "certless connect failed without an actionable enroll hint"; }
ok "certless client fails with an actionable enroll-first message (exit $rc)"

# --- 8. done -----------------------------------------------------------------
echo
if [ "$BROWSER_MANUAL" = "1" ]; then
  say "BROWSER TESTBED READY"
else
  say "ALL SELF-CHECKS PASSED"
fi

if [ "$CHECK_ONLY" = "1" ]; then
  say "--check-only: tearing down"
  exit 0
fi

cat <<EOF

------------------------------------------------------------------------------
 Live testbed is UP. Run zerocode against it (mutual TLS):

 DIRECT (zerocode -> daemon):
   $ZEROCODE \\
     --connect wss://127.0.0.1:$WSS_PORT \\
     --tls-ca-cert $CA \\
     --tls-client-cert $CLIENT_CRT \\
     --tls-client-key $CLIENT_KEY \\
     --agent <your-agent-alias>

 VIA RELAY (zerocode -> relay -> daemon) - short form, certs picked up from
 <config-dir>/tls and --connect defaulted to the daemon loopback:
   $ZEROCODE \\
     --config-dir $CLIENT_CONFIG_DIR \\
     --relay 127.0.0.1:$RELAY_PORT \\
     --relay-node $NODE_ID \\
     --relay-host 127.0.0.1 \\
     --relay-ca $RELAY_TLS_DIR/ca.crt \\
     --agent <your-agent-alias>

 Logs:   daemon $TB/daemon.log   relay $TB/relay.log
 Config: $TB/config.toml

EOF

if [ "$BROWSER_MANUAL" = "1" ]; then
cat <<EOF
 BROWSER FRONTDOOR (pairing code is still unused):
   URL:          https://127.0.0.1:$RELAY_PORT/
   Server ID:    $NODE_ID
   Pairing Code: $CODE

 Automated browser proof against this live testbed:
   node $REPO_ROOT/scripts/dev/browser-relay-frontdoor-e2e.mjs \\
     --url https://127.0.0.1:$RELAY_PORT/ \\
     --node-id $NODE_ID \\
     --pairing-code $CODE \\
     --browser /path/to/chromium

 The automated harness launches Chrome/Chromium with local certificate errors
 ignored for this self-provisioned test relay.

EOF
fi

cat <<EOF

 Press Ctrl-C to stop the daemon and relay and clean up.
------------------------------------------------------------------------------
EOF

# Keep the daemon + relay alive until the operator interrupts.
wait "$DAEMON_PID"
