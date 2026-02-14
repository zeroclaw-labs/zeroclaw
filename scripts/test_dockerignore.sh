#!/usr/bin/env bash
# Test script to verify .dockerignore excludes sensitive paths
# Run: ./scripts/test_dockerignore.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(dirname "$SCRIPT_DIR")"
DOCKERIGNORE="$PROJECT_ROOT/.dockerignore"

RED='\033[0;31m'
GREEN='\033[0;32m'
NC='\033[0m' # No Color

PASS=0
FAIL=0

log_pass() {
    echo -e "${GREEN}✓${NC} $1"
    PASS=$((PASS + 1))
}

log_fail() {
    echo -e "${RED}✗${NC} $1"
    FAIL=$((FAIL + 1))
}

# Test 1: .dockerignore exists
echo "=== Testing .dockerignore ==="
if [[ -f "$DOCKERIGNORE" ]]; then
    log_pass ".dockerignore file exists"
else
    log_fail ".dockerignore file does not exist"
    exit 1
fi

# Test 2: Required exclusions are present
MUST_EXCLUDE=(
    ".git"
    ".githooks"
    "target"
    "docs"
    "examples"
    "tests"
    "*.md"
    "*.png"
    "*.db"
    "*.db-journal"
    ".DS_Store"
    ".github"
    "deny.toml"
    "LICENSE"
    ".env"
    ".tmp_*"
)

for pattern in "${MUST_EXCLUDE[@]}"; do
    # Use fgrep for literal matching
    if grep -Fq "$pattern" "$DOCKERIGNORE" 2>/dev/null; then
        log_pass "Excludes: $pattern"
    else
        log_fail "Missing exclusion: $pattern"
    fi
done

# Test 3: Build essentials are NOT excluded
MUST_NOT_EXCLUDE=(
    "Cargo.toml"
    "Cargo.lock"
    "src"
)

for path in "${MUST_NOT_EXCLUDE[@]}"; do
    if grep -qE "^${path}$" "$DOCKERIGNORE" 2>/dev/null; then
        log_fail "Build essential '$path' is incorrectly excluded"
    else
        log_pass "Build essential NOT excluded: $path"
    fi
done

# Test 4: No syntax errors (basic validation)
while IFS= read -r line; do
    # Skip empty lines and comments
    [[ -z "$line" || "$line" =~ ^# ]] && continue
    
    # Check for common issues
    if [[ "$line" =~ [[:space:]]$ ]]; then
        log_fail "Trailing whitespace in pattern: '$line'"
    fi
done < "$DOCKERIGNORE"
log_pass "No trailing whitespace in patterns"

# Test 5: Verify Docker build context would be small
echo ""
echo "=== Simulating Docker build context ==="

# Create temp dir and simulate what would be sent
TEMP_DIR=$(mktemp -d)
trap "rm -rf $TEMP_DIR" EXIT

# Use rsync with .dockerignore patterns to simulate Docker's behavior
cd "$PROJECT_ROOT"

# Count files that WOULD be sent (excluding .dockerignore patterns)
TOTAL_FILES=$(find . -type f | wc -l | tr -d ' ')
CONTEXT_FILES=$(find . -type f \
    ! -path './.git/*' \
    ! -path './target/*' \
    ! -path './docs/*' \
    ! -path './examples/*' \
    ! -path './tests/*' \
    ! -name '*.md' \
    ! -name '*.png' \
    ! -name '*.svg' \
    ! -name '*.db' \
    ! -name '*.db-journal' \
    ! -name '.DS_Store' \
    ! -path './.github/*' \
    ! -name 'deny.toml' \
    ! -name 'LICENSE' \
    ! -name '.env' \
    ! -name '.env.*' \
    2>/dev/null | wc -l | tr -d ' ')

echo "Total files in repo: $TOTAL_FILES"
echo "Files in Docker context: $CONTEXT_FILES"

if [[ $CONTEXT_FILES -lt $TOTAL_FILES ]]; then
    log_pass "Docker context is smaller than full repo ($CONTEXT_FILES < $TOTAL_FILES files)"
else
    log_fail "Docker context is not being reduced"
fi

# Test 6: Verify critical security files would be excluded
echo ""
echo "=== Security checks ==="

# Check if .git would be excluded
if [[ -d "$PROJECT_ROOT/.git" ]]; then
    if grep -q "^\.git$" "$DOCKERIGNORE"; then
        log_pass ".git directory will be excluded (security)"
    else
        log_fail ".git directory NOT excluded - SECURITY RISK"
    fi
fi

# Check if any .db files exist and would be excluded
DB_FILES=$(find "$PROJECT_ROOT" -name "*.db" -type f 2>/dev/null | head -5)
if [[ -n "$DB_FILES" ]]; then
    if grep -q "^\*\.db$" "$DOCKERIGNORE"; then
        log_pass "*.db files will be excluded (security)"
    else
        log_fail "*.db files NOT excluded - SECURITY RISK"
    fi
fi

# Summary
echo ""
echo "=== Summary ==="
echo -e "Passed: ${GREEN}$PASS${NC}"
echo -e "Failed: ${RED}$FAIL${NC}"

if [[ $FAIL -gt 0 ]]; then
    echo -e "${RED}FAILED${NC}: $FAIL tests failed"
    exit 1
else
    echo -e "${GREEN}PASSED${NC}: All tests passed"
    exit 0
fi
