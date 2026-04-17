#!/bin/sh
# hotswap-entrypoint.sh — respawning PID-1 supervisor for zeroclaw.
#
# Purpose
# -------
# Allow `videoclaw-ops/tooling/hot-swap.sh` to replace the zeroclaw binary
# inside a running pod without re-pulling the container image. Works by
# preferring a "hot-swap" copy on the PVC over the image's baked binary.
#
# Layout inside the pod
#   Stock image binary    : /usr/local/bin/zeroclaw
#   Hot-swap binary (PVC) : /zeroclaw-data/.hotswap/zeroclaw
#   Child PID file (PVC)  : /zeroclaw-data/.hotswap/child.pid
#
# /zeroclaw-data is the PVC mount (survives pod restart), so a hot-swap
# survives process crashes; it does NOT survive PVC deletion (intentional).
#
# Signal handling
#   - SIGTERM/SIGINT from kubelet is forwarded to the current child so
#     graceful shutdown works during rolling updates.
#   - When the child exits on its own (or is killed by hot-swap.sh), the
#     loop re-reads the hot-swap location and respawns, picking up any
#     newly-copied binary.
#
# Failure semantics
#   - If the child refuses to die on SIGTERM, the supervisor still exits
#     after the shell's wait returns; kubelet will force-kill the pod
#     after terminationGracePeriodSeconds.
#   - If the hot-swap binary is broken, each iteration restarts in 2s.
#     Kubelet's liveness probe (10s period, 3 failures) will restart the
#     pod after ~30s of /health being unreachable.
#   - CIRCUIT BREAKER (default on): if the child exits <10s N times in
#     a row (N=5 by default, configurable via HOTSWAP_MAX_FAST_FAILS),
#     supervisor exits PID 1 so kubelet escalates to CrashLoopBackoff —
#     otherwise a truly-broken binary stays hidden behind Ready=1/1. Any
#     successful >=10s run resets the counter. A value of 0 disables.

set -u

HOTSWAP_DIR="/zeroclaw-data/.hotswap"
HOTSWAP_BIN="${HOTSWAP_DIR}/zeroclaw"
STOCK_BIN="/usr/local/bin/zeroclaw"
PID_FILE="${HOTSWAP_DIR}/child.pid"

# Circuit breaker tunables (overridable via env for tests / ops override).
MAX_FAST_FAILS="${HOTSWAP_MAX_FAST_FAILS:-5}"
FAST_FAIL_SECS="${HOTSWAP_FAST_FAIL_SECS:-10}"

mkdir -p "${HOTSWAP_DIR}" 2>/dev/null || true

log() { printf '[hotswap-supervisor] %s\n' "$*" ; }

CHILD_PID=""
FAST_FAIL_STREAK=0

shutdown() {
    sig="$1"
    if [ -n "${CHILD_PID}" ] ; then
        log "received SIG${sig}; forwarding to child PID=${CHILD_PID}"
        kill -TERM "${CHILD_PID}" 2>/dev/null || true
        # Wait up to grace period; kubelet kills us after SIGKILL anyway.
        wait "${CHILD_PID}" 2>/dev/null || true
    fi
    log "shutting down"
    exit 0
}
trap 'shutdown TERM' TERM
trap 'shutdown INT'  INT

log "starting; PID=$$"
log "stock binary   = ${STOCK_BIN}"
log "hot-swap path  = ${HOTSWAP_BIN}"
if [ "${MAX_FAST_FAILS}" -gt 0 ] ; then
    log "circuit breaker: ${MAX_FAST_FAILS} consecutive runs shorter than ${FAST_FAIL_SECS}s will crash the pod"
else
    log "circuit breaker: DISABLED (HOTSWAP_MAX_FAST_FAILS=0)"
fi

while true ; do
    if [ -x "${HOTSWAP_BIN}" ] ; then
        BIN="${HOTSWAP_BIN}"
        log "using HOT-SWAPPED binary (${BIN})"
    else
        BIN="${STOCK_BIN}"
        log "using stock image binary (${BIN})"
    fi

    # Record start time so we can detect fast-fail.
    START_EPOCH=$(date +%s 2>/dev/null || echo 0)

    "${BIN}" "$@" &
    CHILD_PID=$!
    printf '%s\n' "${CHILD_PID}" > "${PID_FILE}" 2>/dev/null || true
    log "child PID=${CHILD_PID} (args: $*)"

    wait "${CHILD_PID}"
    RC=$?
    END_EPOCH=$(date +%s 2>/dev/null || echo 0)
    RAN_FOR=$(( END_EPOCH - START_EPOCH ))
    log "child PID=${CHILD_PID} exited rc=${RC} ran_for=${RAN_FOR}s; restarting in 2s"
    CHILD_PID=""
    rm -f "${PID_FILE}" 2>/dev/null || true

    # Circuit breaker: count consecutive fast fails; reset on any long run.
    if [ "${MAX_FAST_FAILS}" -gt 0 ] ; then
        if [ "${RAN_FOR}" -lt "${FAST_FAIL_SECS}" ] ; then
            FAST_FAIL_STREAK=$(( FAST_FAIL_STREAK + 1 ))
            log "fast-fail streak = ${FAST_FAIL_STREAK}/${MAX_FAST_FAILS}"
            if [ "${FAST_FAIL_STREAK}" -ge "${MAX_FAST_FAILS}" ] ; then
                log "CIRCUIT BREAKER TRIPPED: ${FAST_FAIL_STREAK} consecutive fast-fails (<${FAST_FAIL_SECS}s each)"
                log "exiting PID 1 so kubelet can surface CrashLoopBackoff and fire alerts"
                exit 1
            fi
        else
            if [ "${FAST_FAIL_STREAK}" -gt 0 ] ; then
                log "run stayed up ${RAN_FOR}s; resetting fast-fail streak (was ${FAST_FAIL_STREAK})"
            fi
            FAST_FAIL_STREAK=0
        fi
    fi

    sleep 2
done
