#!/usr/bin/env bash
# check-parity.sh — 检查上游是否已实现我们的自定义功能
#
# 用法：
#   ./dev/check-parity.sh              # 检查所有功能
#   ./dev/check-parity.sh --fetch      # 先 fetch upstream 再检查
#   ./dev/check-parity.sh F-01 F-03    # 只检查指定功能
#
# 退出码：
#   0 — 所有功能均为 KEEP（上游无覆盖，无需操作）
#   1 — 至少一个功能需要 REVIEW（上游可能已实现，需人工比较）
#
# 依赖：git（需要 upstream remote 已配置）
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ── Colors ────────────────────────────────────────────────────────
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
RED='\033[0;31m'
CYAN='\033[0;36m'
BOLD='\033[1m'
RESET='\033[0m'

# ── Args ──────────────────────────────────────────────────────────
DO_FETCH=false
FILTER_IDS=()
for arg in "$@"; do
    case "$arg" in
        --fetch) DO_FETCH=true ;;
        F-*) FILTER_IDS+=("$arg") ;;
        *) echo "Unknown argument: $arg" >&2; exit 2 ;;
    esac
done

# ── Helpers ───────────────────────────────────────────────────────
NEEDS_REVIEW=0

# grep_upstream FILE PATTERN [PATTERN...]
# Returns count of matches in upstream/master:FILE
grep_upstream_file() {
    local file="$1"; shift
    local count=0
    for pattern in "$@"; do
        local n
        n=$(git show "upstream/master:$file" 2>/dev/null | grep -ciE "$pattern" || true)
        count=$((count + n))
    done
    echo "$count"
}

# grep_upstream_tree PATTERN [PATTERN...]
# Searches all of upstream/master's tracked files
grep_upstream_tree() {
    local count=0
    for pattern in "$@"; do
        local n
        n=$(git grep -ciE "$pattern" upstream/master -- 2>/dev/null | wc -l | tr -d ' ' || true)
        count=$((count + n))
    done
    echo "$count"
}

print_header() {
    echo ""
    echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
    echo -e "${BOLD} ZeroClaw One2X — Feature Parity Check${RESET}"
    echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"
}

check_feature() {
    local id="$1"
    local name="$2"
    local verdict="KEEP"
    local detail=""

    # Skip if filter specified and this ID not in list
    if [ ${#FILTER_IDS[@]} -gt 0 ]; then
        local matched=false
        for fid in "${FILTER_IDS[@]}"; do
            [[ "$fid" == "$id" ]] && matched=true
        done
        $matched || return
    fi

    # ── Feature-specific checks ───────────────────────────────────
    case "$id" in

    F-01) # Session Hygiene (session JSONL persistence trim, not in-memory trim)
        # upstream has fast_trim_tool_results (in-memory) — we're looking for JSONL file persistence trimming
        local n
        n=0
        for pattern in "trim.*before.*persist\|persist.*trim" "truncate.*session.*file\|session.*file.*truncat" "repair.*session.*message\|session.*message.*repair" "jsonl.*trim\|trim.*jsonl"; do
            local m
            m=$(git grep -ciE "$pattern" upstream/master -- '*.rs' 2>/dev/null | wc -l | tr -d ' ' || true)
            n=$((n + m))
        done
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in *.rs — upstream may have session-persistence trimming. Compare with F-01 criteria in custom-features.md"
        fi
        ;;

    F-02) # Multi-Stage Compaction (chunked summarization, not single-pass)
        local n
        n=0
        for pattern in "multi.stage.*compress" "chunk.*compress" "chunked.*compact" "compress.*chunk" "stage.*summar"; do
            local m
            m=$(git grep -ciE "$pattern" upstream/master -- 'src/*.rs' 'src/**/*.rs' 2>/dev/null | wc -l | tr -d ' ' || true)
            n=$((n + m))
        done
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in src/ — upstream may have chunked compression. Compare with F-02 criteria."
        fi
        ;;

    F-03) # Planning Detection + Fast Approval
        local n
        n=$(grep_upstream_tree \
            "planning.*without.*execution" \
            "planning.*detect" \
            "execution.*nudge" \
            "fast.*approval" \
            "short.*confirm.*inject")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) — upstream may have planning/approval detection. Compare with F-03 criteria."
        fi
        ;;

    F-04) # Web Channel (HTTP/WebSocket channel for direct browser connection)
        # exclude existing DingTalk/Lark/QQ websocket channels — looking for a generic "web" channel
        local n
        n=0
        for pattern in "web_channel\b" "WebChannel\b" "channel.*web\b"; do
            local m
            m=$(git grep -ciE "$pattern" upstream/master -- 'src/channels/*.rs' 'src/config/*.rs' 2>/dev/null | wc -l | tr -d ' ' || true)
            n=$((n + m))
        done
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in src/channels/ — upstream may have added a generic web/browser channel."
        fi
        ;;

    F-05) # Agent SSE Endpoint (HTTP SSE streaming for programmatic agent execution)
        local n
        n=0
        for pattern in "/sse/agent\b" "agent_sse\b" "AgentSse\b" "sse.*run.*agent\|agent.*run.*sse"; do
            local m
            m=$(git grep -ciE "$pattern" upstream/master -- 'src/gateway/*.rs' 'src/agent/*.rs' 2>/dev/null | wc -l | tr -d ' ' || true)
            n=$((n + m))
        done
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in src/gateway/ or src/agent/ — upstream may have SSE agent endpoint."
        fi
        ;;

    F-06) # Memory list_by_prefix
        local n
        n=$(grep_upstream_file "src/memory/traits.rs" \
            "list_by_prefix" \
            "fn list.*prefix" \
            "prefix.*list")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in memory/traits.rs — upstream may have added list_by_prefix."
        fi
        ;;

    F-07) # Shell SESSION_ID
        local n
        n=$(grep_upstream_file "src/tools/shell.rs" \
            "SESSION_ID" \
            "ZEROCLAW_SESSION" \
            "session.*id.*env")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in tools/shell.rs — upstream may inject SESSION_ID."
        fi
        ;;

    F-08) # Heartbeat Lark validation
        local n
        n=$(grep_upstream_file "src/daemon/mod.rs" \
            "lark.*heartbeat\|heartbeat.*lark" \
            "feishu.*heartbeat\|heartbeat.*feishu")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in daemon/mod.rs — upstream may have lark heartbeat validation."
        fi
        ;;

    F-09) # Stream Idle Timeout (N1) — in upstream file, no feature flag
        # Looking for ANY per-token timeout in the ReliableProvider stream spawn tasks
        local n
        n=$(grep_upstream_file "src/providers/reliable.rs" \
            "STREAM_IDLE_TIMEOUT\|idle.*timeout.*stream\|stream.*idle.*timeout" \
            "tokio::time::timeout.*stream\.next\|timeout.*stream\.next")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in providers/reliable.rs — upstream may have stream idle timeout. Check F-09 criteria."
        fi
        ;;

    F-10) # Compaction Context Window Floor (N2) — in upstream file, no feature flag
        local n
        n=$(grep_upstream_file "src/agent/context_compressor.rs" \
            "MIN_CONTEXT_WINDOW_FLOOR\|context_window.*floor\|context_window.*min\|min.*context_window")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in agent/context_compressor.rs — upstream may have context window floor. Check F-10 criteria."
        fi
        ;;

    F-11) # Case-Insensitive Tool Name Lookup (N3) — in upstream file, no feature flag
        local n
        n=$(grep_upstream_file "src/agent/tool_execution.rs" \
            "to_ascii_lowercase\|case.insensitive.*tool\|tool.*case.insensitive\|toLowerCase.*tool")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in agent/tool_execution.rs — upstream may have case-insensitive tool lookup. Check F-11 criteria."
        fi
        ;;

    F-12) # Full Mid-History Tool Pairing Repair
        local n
        n=$(grep_upstream_tree \
            "repair.*tool.*pair.*full\|full.*tool.*pair\|mid.*history.*orphan\|synthetic.*tool.*result\|insert.*synthetic.*tool")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) — upstream may have full mid-history tool pairing repair. Check F-12 criteria."
        fi
        ;;

    F-13) # Pre-LLM Tool Result Size Guard
        local n
        n=$(grep_upstream_tree \
            "limit.*tool.*result.*size\|pre.*llm.*tool.*trim\|unconditional.*tool.*trim\|tool.*result.*guard.*llm")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) — upstream may have unconditional pre-LLM tool result capping. Check F-13 criteria."
        fi
        ;;

    F-14) # Pre-Compaction Key-Facts Memory Flush
        local n
        n=$(grep_upstream_tree \
            "pre.*compact.*memory\|memory.*flush.*compress\|key.*facts.*extract\|before.*compaction.*store\|key_facts_")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) — upstream may have pre-compaction key-facts memory flush. Check F-14 criteria."
        fi
        ;;

    F-15) # Retry Jitter in ReliableProvider Backoff (upstream file, no flag)
        local n
        n=$(grep_upstream_file "src/providers/reliable.rs" \
            "jitter.*backoff\|backoff.*jitter\|gen_range.*backoff\|random.*backoff\|jitter_factor")
        if [ "$n" -gt 0 ]; then
            verdict="REVIEW"
            detail="Found $n match(es) in providers/reliable.rs — upstream may have retry jitter. Check F-15 criteria."
        fi
        ;;

    *)
        echo -e "  ${RED}Unknown feature ID: $id${RESET}"
        return
        ;;
    esac

    # ── Print result ──────────────────────────────────────────────
    if [ "$verdict" = "KEEP" ]; then
        echo -e "  ${GREEN}✓ KEEP${RESET}   ${BOLD}$id${RESET} — $name"
    else
        echo -e "  ${YELLOW}⚠ REVIEW${RESET} ${BOLD}$id${RESET} — $name"
        echo -e "           ${YELLOW}$detail${RESET}"
        echo -e "           ${CYAN}→ See dev/custom-features.md#$id for removal criteria${RESET}"
        NEEDS_REVIEW=$((NEEDS_REVIEW + 1))
    fi
}

# ── Main ──────────────────────────────────────────────────────────
print_header

# Fetch upstream if requested
if $DO_FETCH; then
    echo ""
    echo -e "${CYAN}Fetching upstream/master...${RESET}"
    if ! git fetch upstream master 2>&1; then
        echo -e "${RED}ERROR: 'git fetch upstream master' failed.${RESET}"
        echo -e "${RED}Run: git remote add upstream https://github.com/zeroclaw-labs/zeroclaw.git${RESET}"
        exit 2
    fi
fi

# Verify upstream/master is accessible
if ! git rev-parse upstream/master >/dev/null 2>&1; then
    echo ""
    echo -e "${RED}ERROR: upstream/master not found. Run with --fetch or:${RESET}"
    echo -e "${RED}  git remote add upstream https://github.com/zeroclaw-labs/zeroclaw.git${RESET}"
    echo -e "${RED}  git fetch upstream master${RESET}"
    exit 2
fi

UPSTREAM_HASH=$(git rev-parse --short upstream/master)
echo ""
echo -e "Checking against upstream/master @ ${CYAN}$UPSTREAM_HASH${RESET}"
echo ""

check_feature "F-01" "Session Hygiene (trim/truncate/repair)"
check_feature "F-02" "Multi-Stage Compaction"
check_feature "F-03" "Planning Detection + Fast Approval"
check_feature "F-04" "Web Channel (WebSocket)"
check_feature "F-05" "Agent SSE Endpoint"
check_feature "F-06" "Memory list_by_prefix"
check_feature "F-07" "Shell SESSION_ID env"
check_feature "F-08" "Heartbeat Lark/Feishu validation"
check_feature "F-09" "Stream Idle Timeout (upstream file, no flag)"
check_feature "F-10" "Compaction Context Window Floor (upstream file, no flag)"
check_feature "F-11" "Case-Insensitive Tool Lookup (upstream file, no flag)"
check_feature "F-12" "Full Mid-History Tool Pairing Repair"
check_feature "F-13" "Pre-LLM Tool Result Size Guard"
check_feature "F-14" "Pre-Compaction Key-Facts Memory Flush"
check_feature "F-15" "Retry Jitter in ReliableProvider Backoff (upstream file, no flag)"

echo ""
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${RESET}"

if [ "$NEEDS_REVIEW" -eq 0 ]; then
    echo -e "${GREEN}All features: KEEP. No upstream adoption detected.${RESET}"
    echo -e "${GREEN}Proceed to Step 2 (merge-upstream.sh).${RESET}"
    exit 0
else
    echo -e "${YELLOW}$NEEDS_REVIEW feature(s) need manual review before merging.${RESET}"
    echo ""
    echo -e "For each ⚠ REVIEW item:"
    echo -e "  1. Read the equivalence criteria in ${CYAN}dev/custom-features.md${RESET}"
    echo -e "  2. Compare upstream implementation vs our implementation"
    echo -e "  3. If equivalent: follow the deletion steps, then skip our commit during cherry-pick"
    echo -e "  4. If not equivalent: keep our code, update Feature Parity Tracking table in SOP"
    echo ""
    exit 1
fi
