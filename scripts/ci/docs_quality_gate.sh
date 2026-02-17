#!/usr/bin/env bash

set -euo pipefail

BASE_SHA="${BASE_SHA:-}"
DOCS_FILES_RAW="${DOCS_FILES:-}"

if [ -z "$BASE_SHA" ] && git rev-parse --verify origin/main >/dev/null 2>&1; then
    BASE_SHA="$(git merge-base origin/main HEAD)"
fi

if [ -z "$DOCS_FILES_RAW" ] && [ -n "$BASE_SHA" ] && git cat-file -e "$BASE_SHA^{commit}" 2>/dev/null; then
    DOCS_FILES_RAW="$(git diff --name-only "$BASE_SHA" HEAD | awk '
        /\.md$/ || /\.mdx$/ || $0 == "LICENSE" || $0 == ".github/pull_request_template.md" {
            print
        }
    ')"
fi

if [ -z "$DOCS_FILES_RAW" ]; then
    echo "No docs files detected; skipping docs quality gate."
    exit 0
fi

if [ -z "$BASE_SHA" ] || ! git cat-file -e "$BASE_SHA^{commit}" 2>/dev/null; then
    echo "BASE_SHA is missing or invalid; falling back to full-file markdown lint."
    BASE_SHA=""
fi

ALL_FILES=()
while IFS= read -r file; do
    if [ -n "$file" ]; then
        ALL_FILES+=("$file")
    fi
done < <(printf '%s\n' "$DOCS_FILES_RAW")

if [ "${#ALL_FILES[@]}" -eq 0 ]; then
    echo "No docs files detected after normalization; skipping docs quality gate."
    exit 0
fi

EXISTING_FILES=()
for file in "${ALL_FILES[@]}"; do
    if [ -f "$file" ]; then
        EXISTING_FILES+=("$file")
    fi
done

if [ "${#EXISTING_FILES[@]}" -eq 0 ]; then
    echo "No existing docs files to lint; skipping docs quality gate."
    exit 0
fi

if command -v npx >/dev/null 2>&1; then
    MD_CMD=(npx --yes markdownlint-cli2@0.20.0)
elif command -v markdownlint-cli2 >/dev/null 2>&1; then
    MD_CMD=(markdownlint-cli2)
else
    echo "markdownlint-cli2 is required (via npx or local binary)."
    exit 1
fi

echo "Linting docs files: ${EXISTING_FILES[*]}"

LINT_OUTPUT_FILE="$(mktemp)"
set +e
"${MD_CMD[@]}" "${EXISTING_FILES[@]}" >"$LINT_OUTPUT_FILE" 2>&1
LINT_EXIT=$?
set -e

if [ "$LINT_EXIT" -eq 0 ]; then
    cat "$LINT_OUTPUT_FILE"
    rm -f "$LINT_OUTPUT_FILE"
    exit 0
fi

if [ -z "$BASE_SHA" ]; then
    cat "$LINT_OUTPUT_FILE"
    rm -f "$LINT_OUTPUT_FILE"
    exit "$LINT_EXIT"
fi

CHANGED_LINES_JSON_FILE="$(mktemp)"
python3 - "$BASE_SHA" "${EXISTING_FILES[@]}" >"$CHANGED_LINES_JSON_FILE" <<'PY'
import json
import re
import subprocess
import sys

base = sys.argv[1]
files = sys.argv[2:]

changed = {}
hunk = re.compile(r"^@@ -\d+(?:,\d+)? \+(\d+)(?:,(\d+))? @@")

for path in files:
    proc = subprocess.run(
        ["git", "diff", "--unified=0", base, "HEAD", "--", path],
        check=False,
        capture_output=True,
        text=True,
    )
    ranges = []
    for line in proc.stdout.splitlines():
        m = hunk.match(line)
        if not m:
            continue
        start = int(m.group(1))
        count = int(m.group(2) or "1")
        if count > 0:
            ranges.append([start, start + count - 1])
    changed[path] = ranges

print(json.dumps(changed))
PY

FILTERED_OUTPUT_FILE="$(mktemp)"
set +e
python3 - "$LINT_OUTPUT_FILE" "$CHANGED_LINES_JSON_FILE" >"$FILTERED_OUTPUT_FILE" <<'PY'
import json
import re
import sys

lint_file = sys.argv[1]
changed_file = sys.argv[2]

with open(changed_file, "r", encoding="utf-8") as f:
    changed = json.load(f)

line_re = re.compile(r"^(.+?):(\d+)\s+error\s+(MD\d+(?:/[^\s]+)?)\s+(.*)$")

blocking = []
baseline = []
other_lines = []

with open(lint_file, "r", encoding="utf-8") as f:
    for raw_line in f:
        line = raw_line.rstrip("\n")
        m = line_re.match(line)
        if not m:
            other_lines.append(line)
            continue

        path, line_no_s, rule, msg = m.groups()
        line_no = int(line_no_s)
        ranges = changed.get(path, [])

        is_changed_line = any(start <= line_no <= end for start, end in ranges)
        entry = f"{path}:{line_no} {rule} {msg}"
        if is_changed_line:
            blocking.append(entry)
        else:
            baseline.append(entry)

if baseline:
    print("Existing markdown issues outside changed lines (non-blocking):")
    for entry in baseline:
        print(f"  - {entry}")

if blocking:
    print("Markdown issues introduced on changed lines (blocking):")
    for entry in blocking:
        print(f"  - {entry}")
    print(f"Blocking markdown issues: {len(blocking)}")
    sys.exit(1)

if baseline:
    print("No blocking markdown issues on changed lines.")
    sys.exit(0)

for line in other_lines:
    print(line)

if any(line.strip() for line in other_lines):
    print("markdownlint exited non-zero with unclassified output; failing safe.")
    sys.exit(2)

print("No blocking markdown issues on changed lines.")
PY
SCRIPT_EXIT=$?
set -e

cat "$FILTERED_OUTPUT_FILE"

rm -f "$LINT_OUTPUT_FILE" "$CHANGED_LINES_JSON_FILE" "$FILTERED_OUTPUT_FILE"
exit "$SCRIPT_EXIT"
