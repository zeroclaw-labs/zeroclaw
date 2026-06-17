#!/usr/bin/env bash

set -euo pipefail

MODE="correctness"
if [ "${1:-}" = "--strict" ]; then
    MODE="strict"
fi

echo "==> rust quality: cargo fmt --all -- --check"
cargo fmt --all -- --check

if [ "$MODE" = "strict" ]; then
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D warnings"
    cargo clippy --locked --all-targets -- -D warnings
else
    echo "==> rust quality: cargo clippy --locked --all-targets -- -D clippy::correctness"
    cargo clippy --locked --all-targets -- -D clippy::correctness
fi

echo "==> rust quality: provider dispatch gate (no direct ModelProvider method calls outside ProviderDispatch)"

# Methods covered by ProviderDispatch / ProviderDispatchRef. Adding a new
# method to the dispatcher requires extending this list in lockstep with
# the dispatch.rs file.
PROTECTED_METHODS='\.(chat|stream_chat|simple_chat|chat_with_system|chat_with_history|chat_with_tools|list_models|list_models_with_pricing|warmup)\('

# We allow:
#   - dispatch.rs and its integration tests (the implementation + its
#     dedicated test fakes).
#   - Any code inside a `tests/` directory (`tests/live/` integration
#     tests, `crates/*/tests/` integration tests).
#   - Any line inside a `#[cfg(test)]` module: the gate uses the
#     first `^#[cfg(test)]` line in each file as a boundary and drops
#     matches at or below it.
#   - `self.<method>(...)` self-calls (same Attributable instance;
#     dispatcher wrap would be a redundant attribution layer).
#   - `self.as_ref().<method>(...)` blanket-impl forwarders (used by
#     `impl ModelProvider for Arc<T>` in zeroclaw-api — must call
#     inner directly to avoid infinite recursion through the dispatcher).
#   - Method calls that follow a ProviderDispatch construction within
#     the prior 3 lines. The dispatcher's borrowed/owned variants
#     produce the call shape `ProviderDispatch::from_ref(p).<method>(`
#     or `ProviderDispatch::new(arc).<method>(` or
#     `let dispatcher = ProviderDispatch::from_ref(...); dispatcher.<method>(`.
#     When rustfmt wraps these onto separate lines, the `.method(` line
#     loses the dispatcher token; we look at the surrounding context.

set +e
RG_OUTPUT=$(rg --vimgrep --type rust "$PROTECTED_METHODS" \
    crates/ src/ xtask/ tools/ tests/ 2>/dev/null)
RG_STATUS=$?
set -e
if [ "$RG_STATUS" -ne 0 ] && [ "$RG_STATUS" -ne 1 ]; then
    echo "❌ ripgrep failed during dispatch gate (status $RG_STATUS)"
    exit 1
fi

VIOLATIONS=$(printf '%s\n' "$RG_OUTPUT" | awk -F: '
    BEGIN {
        allowed["crates/zeroclaw-providers/src/dispatch.rs"] = 1
        allowed["crates/zeroclaw-providers/tests/dispatch_integration.rs"] = 1
    }
    function read_file_lines(file,    cmd, raw_line, lineno) {
        cmd = "cat " file " 2>/dev/null"
        lineno = 1
        while ((cmd | getline raw_line) > 0) {
            file_lines[file, lineno] = raw_line
            lineno++
        }
        close(cmd)
        file_line_count[file] = lineno - 1
        # Test boundary: first ^#[cfg(test)] line in the file.
        test_boundary[file] = 999999999
        for (i = 1; i <= file_line_count[file]; i++) {
            if (file_lines[file, i] ~ /^#\[cfg\(test\)\]/) {
                test_boundary[file] = i
                break
            }
        }
        file_loaded[file] = 1
    }
    function context_is_cfg_test_block(file, lineno,    i, ln) {
        # Look back up to 200 lines for an indented `#[cfg(test)]`
        # attribute followed by a `{` block start. If found and we are
        # still inside the brace-balance window, treat the match as
        # test code. (The 200-line window is a heuristic ceiling that
        # covers realistic in-function #[cfg(test)] blocks while
        # bounding cost; trait-method test stubs longer than that are
        # rare in this codebase.)
        for (i = lineno - 1; i >= lineno - 200 && i >= 1; i--) {
            ln = file_lines[file, i]
            if (ln ~ /^[[:space:]]+#\[cfg\(test\)\]/) {
                # Found an indented cfg(test). Count braces between
                # that line and the match line; if positive, we are
                # still inside the cfg(test) block.
                braces = 0
                for (j = i + 1; j <= lineno; j++) {
                    lnb = file_lines[file, j]
                    n = gsub(/\{/, "{", lnb); braces += n
                    n = gsub(/\}/, "}", lnb); braces -= n
                }
                if (braces > 0) return 1
                # If braces are balanced or negative, the block closed
                # before our match — keep looking back for an outer cfg.
            }
        }
        return 0
    }
    function context_is_self_call(file, lineno,    i, ln) {
        # Look back up to 3 lines for the receiver of this chained call.
        # rustfmt commonly splits `self.method()` into `self\n.method()`
        # — the gate must treat that as a self-call.
        for (i = lineno - 1; i >= lineno - 3 && i >= 1; i--) {
            ln = file_lines[file, i]
            # Receiver is `self`, `self.as_ref()`, or any identifier
            # ending in `self` (e.g. `(*self.inner)` is NOT a self-call
            # but the inner is captured separately).
            if (ln ~ /(^|[^a-zA-Z0-9_])self[ ]*$/) return 1
            if (ln ~ /self\.as_ref\(\)[ ]*$/) return 1
            # Non-empty non-whitespace continuation breaks the lookback —
            # the previous line is a complete statement boundary.
            if (ln ~ /[^[:space:]]/ && ln !~ /[\.\(\),][ ]*$/) return 0
        }
        return 0
    }
    function context_has_dispatcher(file, lineno,    start, i, ln) {
        # Look back up to 5 lines for a ProviderDispatch construction
        # or a dispatcher.<method> chain start.
        start = lineno - 5
        if (start < 1) start = 1
        for (i = start; i <= lineno; i++) {
            ln = file_lines[file, i]
            if (ln ~ /ProviderDispatch::(new|from_ref)\(/) return 1
            if (ln ~ /dispatcher[ ]*\.[a-z_]+\(/) return 1
            if (ln ~ /dispatcher[ ]*$/) return 1
        }
        return 0
    }
    {
        file = $1
        line = $2
        if (file == "") next
        if (file in allowed) next
        # Skip live/integration tests in /tests/ directories.
        if (file ~ /(^|\/)tests?\//) next
        if (!(file in file_loaded)) read_file_lines(file)
        if (line + 0 >= test_boundary[file] + 0) next
        # Drop matches inside indented #[cfg(test)] blocks (test
        # helpers nested in production functions).
        if (context_is_cfg_test_block(file, line + 0)) next
        # Reconstruct content (rest of rg vimgrep after `file:line:col:`).
        content = $4
        for (i = 5; i <= NF; i++) content = content ":" $i
        # Skip self-calls.
        if (content ~ /self\.(chat|stream_chat|simple_chat|chat_with_system|chat_with_history|chat_with_tools|list_models|list_models_with_pricing|warmup)\(/) next
        # Skip blanket Arc<T> forwarders.
        if (content ~ /self\.as_ref\(\)\.(chat|stream_chat|simple_chat|chat_with_system|chat_with_history|chat_with_tools|list_models|list_models_with_pricing|warmup)\(/) next
        # Skip rustfmt-split self.method() chains (receiver on prior line).
        if (context_is_self_call(file, line + 0)) next
        # Skip doc/comment lines.
        if (content ~ /^[[:space:]]*\/\//) next
        # Skip calls in the dispatcher-construction context.
        if (context_has_dispatcher(file, line + 0)) next
        print file ":" line ":" content
    }
')

if [ -n "$VIOLATIONS" ]; then
    echo "❌ Direct ModelProvider method calls found outside the dispatcher:"
    echo "$VIOLATIONS"
    echo
    echo "Route the call through zeroclaw_providers::ProviderDispatch:"
    echo "    ProviderDispatch::new(provider.clone()).<method>(...)        // Arc<dyn ModelProvider>"
    echo "    ProviderDispatch::from_ref(&*provider).<method>(...)         // &dyn ModelProvider"
    echo
    echo "If this is a false positive (e.g. .chat() on a non-ModelProvider type),"
    echo "extend the awk filter in scripts/ci/rust_quality_gate.sh."
    exit 1
fi

echo "==> rust quality: provider dispatch gate clean"
