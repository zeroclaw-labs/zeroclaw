#!/usr/bin/env bash

set -euo pipefail

BASE_SHA="${BASE_SHA:-}"
RUST_FILES_RAW="${RUST_FILES:-}"

if [ -z "$BASE_SHA" ] && git rev-parse --verify origin/main >/dev/null 2>&1; then
    BASE_SHA="$(git merge-base origin/main HEAD)"
fi

if [ -z "$BASE_SHA" ] && git rev-parse --verify HEAD~1 >/dev/null 2>&1; then
    BASE_SHA="$(git rev-parse HEAD~1)"
fi

if [ -z "$BASE_SHA" ] || ! git cat-file -e "$BASE_SHA^{commit}" 2>/dev/null; then
    echo "BASE_SHA is missing or invalid for strict delta gate."
    echo "Set BASE_SHA explicitly or ensure origin/main is available."
    exit 1
fi

if [ -z "$RUST_FILES_RAW" ]; then
    RUST_FILES_RAW="$(git diff --name-only "$BASE_SHA" HEAD | awk '/\.rs$/ { print }')"
fi

ALL_FILES=()
while IFS= read -r file; do
    if [ -n "$file" ]; then
        ALL_FILES+=("$file")
    fi
done < <(printf '%s\n' "$RUST_FILES_RAW")

if [ "${#ALL_FILES[@]}" -eq 0 ]; then
    echo "No Rust source files changed; skipping strict delta gate."
    exit 0
fi

EXISTING_FILES=()
for file in "${ALL_FILES[@]}"; do
    if [ -f "$file" ]; then
        EXISTING_FILES+=("$file")
    fi
done

if [ "${#EXISTING_FILES[@]}" -eq 0 ]; then
    echo "No existing changed Rust files to lint; skipping strict delta gate."
    exit 0
fi

echo "Strict delta linting changed Rust files: ${EXISTING_FILES[*]}"

CHANGED_LINES_JSON_FILE="$(mktemp)"
CLIPPY_JSON_FILE="$(mktemp)"
CLIPPY_STDERR_FILE="$(mktemp)"
FILTERED_OUTPUT_FILE="$(mktemp)"
trap 'rm -f "$CHANGED_LINES_JSON_FILE" "$CLIPPY_JSON_FILE" "$CLIPPY_STDERR_FILE" "$FILTERED_OUTPUT_FILE"' EXIT

python3 - "$BASE_SHA" "${EXISTING_FILES[@]}" >"$CHANGED_LINES_JSON_FILE" <<'PY'
import json
import re
import subprocess
import sys

base = sys.argv[1]
files = sys.argv[2:]
hunk = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")
changed = {}

for path in files:
    proc = subprocess.run(
        ["git", "diff", "--unified=0", base, "HEAD", "--", path],
        check=False,
        capture_output=True,
        text=True,
    )
    ranges = []
    for line in proc.stdout.splitlines():
        match = hunk.match(line)
        if not match:
            continue
        start = int(match.group(1))
        count = int(match.group(2) or "1")
        if count > 0:
            ranges.append([start, start + count - 1])
    changed[path] = ranges

print(json.dumps(changed))
PY

set +e
cargo clippy --quiet --locked --all-targets --message-format=json -- -D warnings >"$CLIPPY_JSON_FILE" 2>"$CLIPPY_STDERR_FILE"
CLIPPY_EXIT=$?
set -e

if [ "$CLIPPY_EXIT" -eq 0 ]; then
    echo "Strict delta gate passed: no strict warnings/errors." 
    exit 0
fi

set +e
python3 - "$CLIPPY_JSON_FILE" "$CHANGED_LINES_JSON_FILE" >"$FILTERED_OUTPUT_FILE" <<'PY'
import json
import sys
from pathlib import Path

messages_file = sys.argv[1]
changed_file = sys.argv[2]

with open(changed_file, "r", encoding="utf-8") as f:
    changed = json.load(f)

cwd = Path.cwd().resolve()


def normalize_path(path_value: str) -> str:
    path = Path(path_value)
    if path.is_absolute():
        try:
            return path.resolve().relative_to(cwd).as_posix()
        except Exception:
            return path.as_posix()
    return path.as_posix()


blocking = []
baseline = []
unclassified = []
classified_count = 0

with open(messages_file, "r", encoding="utf-8", errors="ignore") as f:
    for raw_line in f:
        line = raw_line.strip()
        if not line:
            continue

        try:
            payload = json.loads(line)
        except json.JSONDecodeError:
            continue

        if payload.get("reason") != "compiler-message":
            continue

        message = payload.get("message", {})
        level = message.get("level")
        if level not in {"warning", "error"}:
            continue

        code_obj = message.get("code") or {}
        code = code_obj.get("code") if isinstance(code_obj, dict) else None
        text = message.get("message", "")
        spans = message.get("spans") or []

        candidate_spans = [span for span in spans if span.get("is_primary")]
        if not candidate_spans:
            candidate_spans = spans

        span_entries = []
        for span in candidate_spans:
            file_name = span.get("file_name")
            line_start = span.get("line_start")
            line_end = span.get("line_end")
            if not file_name or line_start is None:
                continue
            norm_path = normalize_path(file_name)
            span_entries.append((norm_path, int(line_start), int(line_end or line_start)))

        if not span_entries:
            unclassified.append(f"{level.upper()} {code or '-'} {text}")
            continue

        is_changed_line = False
        best_path, best_line, _ = span_entries[0]
        for path, line_start, line_end in span_entries:
            ranges = changed.get(path)
            if ranges is None:
                continue

            for start, end in ranges:
                if line_end >= start and line_start <= end:
                    is_changed_line = True
                    best_path, best_line = path, line_start
                    break
            if is_changed_line:
                break

        entry = f"{best_path}:{best_line} {level.upper()} {code or '-'} {text}"
        classified_count += 1
        if is_changed_line:
            blocking.append(entry)
        else:
            baseline.append(entry)

if baseline:
    print("Existing strict lint issues outside changed Rust lines (non-blocking):")
    for entry in baseline:
        print(f"  - {entry}")

if blocking:
    print("Strict lint issues introduced on changed Rust lines (blocking):")
    for entry in blocking:
        print(f"  - {entry}")
    print(f"Blocking strict lint issues: {len(blocking)}")
    sys.exit(1)

if classified_count > 0:
    print("No blocking strict lint issues on changed Rust lines.")
    sys.exit(0)

if unclassified:
    print("Strict lint exited non-zero with unclassified diagnostics; failing safe:")
    for entry in unclassified[:20]:
        print(f"  - {entry}")
    sys.exit(2)

print("Strict lint exited non-zero without parsable diagnostics; failing safe.")
sys.exit(2)
PY
FILTER_EXIT=$?
set -e

cat "$FILTERED_OUTPUT_FILE"

if [ "$FILTER_EXIT" -eq 0 ]; then
    if [ -s "$CLIPPY_STDERR_FILE" ]; then
        echo "clippy stderr summary (informational):"
        cat "$CLIPPY_STDERR_FILE"
    fi
    exit 0
fi

if [ -s "$CLIPPY_STDERR_FILE" ]; then
    echo "clippy stderr summary:"
    cat "$CLIPPY_STDERR_FILE"
fi

exit "$FILTER_EXIT"
