#!/usr/bin/env bash
# Notion integration diagnostics — verifies API key, capabilities, and access.
# Usage: ./scripts/notion-check.sh <api_key>
#        ./scripts/notion-check.sh              # reads from NOTION_API_KEY env

set -euo pipefail

# ── 0. Input validation ──────────────────────────────────────

if [[ -z "${NOTION_API_KEY:-}" && -z "${NOTION_TOKEN:-}" ]]; then
  echo "Error: Notion API key not found in environment."
  echo "Usage: export NOTION_API_KEY=ntn_... && $0"
  exit 1
fi

API_KEY="${NOTION_API_KEY:-${NOTION_TOKEN:-}}"

BASE="https://api.notion.com/v1"
VERSION="2025-09-03"

call() {
  local method="$1" endpoint="$2" data="${3:-}"
  local args=(
    -s -w "\n%{http_code}"
    -H "Authorization: Bearer $API_KEY"
    -H "Notion-Version: $VERSION"
    -H "Content-Type: application/json"
    -X "$method"
  )
  [[ -n "$data" ]] && args+=(-d "$data")
  curl "${args[@]}" "${BASE}${endpoint}"
}

parse() {
  # split body and status code
  local raw="$1"
  HTTP_CODE=$(echo "$raw" | tail -1)
  BODY=$(echo "$raw" | sed '$d')
}

section() {
  echo ""
  echo "━━━ $1 ━━━"
}

# ── 1. Token validation ──────────────────────────────────────

section "1. Token validation (GET /users/me)"
parse "$(call GET /users/me)"

if [[ "$HTTP_CODE" == "200" ]]; then
  echo "✓ API key is valid (HTTP $HTTP_CODE)"
  bot_name=$(echo "$BODY" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('name','?'))" 2>/dev/null || echo "?")
  bot_type=$(echo "$BODY" | python3 -c "import sys,json; d=json.load(sys.stdin); print(d.get('type','?'))" 2>/dev/null || echo "?")
  echo "  Bot name: $bot_name"
  echo "  Type:     $bot_type"
else
  echo "✗ API key is INVALID (HTTP $HTTP_CODE)"
  echo "$BODY" | python3 -m json.tool 2>/dev/null || echo "$BODY"
  echo ""
  echo "Fix: Check the key at https://notion.so/my-integrations"
  exit 1
fi

# ── 2. Search — what can the integration see? ────────────────

section "2. Accessible content (POST /search)"
parse "$(call POST /search '{}')"

if [[ "$HTTP_CODE" == "200" ]]; then
  total=$(echo "$BODY" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('results',[])))" 2>/dev/null || echo "?")
  echo "✓ Search returned $total result(s) (HTTP $HTTP_CODE)"

  if [[ "$total" == "0" ]]; then
    echo ""
    echo "  ⚠ No pages or databases are shared with this integration."
    echo "  Fix: In Notion, open a page → click '...' → 'Connect to' → select your integration."
  else
    echo ""
    echo "  Objects found:"
    echo "$BODY" | python3 -c "
import sys, json
data = json.load(sys.stdin)
for r in data.get('results', []):
    obj = r.get('object', '?')
    rid = r.get('id', '?')
    title = '?'
    if obj == 'page':
        props = r.get('properties', {})
        for v in props.values():
            if v.get('type') == 'title':
                arr = v.get('title', [])
                if arr:
                    title = arr[0].get('plain_text', arr[0].get('text', {}).get('content', '?'))
                break
    elif obj == 'data_source':
        tarr = r.get('title', [])
        if tarr:
            title = tarr[0].get('plain_text', '?')
    print(f'    [{obj:12s}] {rid}  \"{title}\"')
has_more = data.get('has_more', False)
if has_more:
    print(f'    ... more results available (has_more=true)')
" 2>/dev/null || echo "  (could not parse results)"
  fi
else
  echo "✗ Search failed (HTTP $HTTP_CODE)"
  echo "$BODY" | python3 -m json.tool 2>/dev/null || echo "$BODY"
fi

# ── 3. Pages only ────────────────────────────────────────────

section "3. Pages (POST /search with page filter)"
parse "$(call POST /search '{"filter":{"property":"object","value":"page"}}')"

if [[ "$HTTP_CODE" == "200" ]]; then
  count=$(echo "$BODY" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('results',[])))" 2>/dev/null || echo "?")
  echo "✓ $count page(s) accessible"
else
  echo "✗ Page search failed (HTTP $HTTP_CODE)"
fi

# ── 4. Data sources (databases) ──────────────────────────────

section "4. Data sources (POST /search with data_source filter)"
parse "$(call POST /search '{"filter":{"property":"object","value":"data_source"}}')"

if [[ "$HTTP_CODE" == "200" ]]; then
  count=$(echo "$BODY" | python3 -c "import sys,json; print(len(json.load(sys.stdin).get('results',[])))" 2>/dev/null || echo "?")
  echo "✓ $count data source(s) accessible"
  if [[ "$count" != "0" ]]; then
    echo ""
    echo "  Data sources:"
    echo "$BODY" | python3 -c "
import sys, json
data = json.load(sys.stdin)
for r in data.get('results', []):
    rid = r.get('id', '?')
    tarr = r.get('title', [])
    title = tarr[0].get('plain_text', '?') if tarr else '?'
    print(f'    {rid}  \"{title}\"')
" 2>/dev/null || echo "  (could not parse)"
  fi
else
  echo "✗ Data source search failed (HTTP $HTTP_CODE)"
fi

# ── 5. Read capability test ──────────────────────────────────

section "5. Read capability test"
# grab first page ID from search
first_page_id=$(call POST /search '{"filter":{"property":"object","value":"page"},"page_size":1}' \
  | sed '$d' \
  | python3 -c "import sys,json; r=json.load(sys.stdin).get('results',[]); print(r[0]['id'] if r else '')" 2>/dev/null || echo "")

if [[ -n "$first_page_id" ]]; then
  parse "$(call GET "/pages/$first_page_id")"
  if [[ "$HTTP_CODE" == "200" ]]; then
    echo "✓ Read content capability works (read page $first_page_id)"
  elif [[ "$HTTP_CODE" == "403" ]]; then
    echo "✗ Read content capability MISSING (HTTP 403)"
    echo "  Fix: https://notion.so/my-integrations → select integration → enable 'Read content'"
  else
    echo "? Unexpected response (HTTP $HTTP_CODE)"
  fi
else
  echo "⚠ No pages found to test read capability"
fi

# ── 6. Markdown read test ────────────────────────────────────

section "6. Markdown read test"
if [[ -n "$first_page_id" ]]; then
  parse "$(call GET "/pages/$first_page_id/markdown")"
  if [[ "$HTTP_CODE" == "200" ]]; then
    truncated=$(echo "$BODY" | python3 -c "import sys,json; print(json.load(sys.stdin).get('truncated', '?'))" 2>/dev/null || echo "?")
    echo "✓ GET /pages/{id}/markdown works (truncated=$truncated)"
  else
    echo "✗ Markdown read failed (HTTP $HTTP_CODE)"
    echo "$BODY" | python3 -c "import sys,json; d=json.load(sys.stdin); print(f\"  {d.get('code','?')}: {d.get('message','?')}\")" 2>/dev/null || true
  fi
else
  echo "⚠ No pages found to test"
fi

# ── Summary ──────────────────────────────────────────────────

section "Summary"
echo "API version:  $VERSION"
echo "Bot:          $bot_name"
echo ""
echo "If you see ✗ on capabilities, go to https://notion.so/my-integrations"
echo "and enable the required capabilities for your integration."
echo ""
echo "If searches return 0 results, share pages/databases with the integration:"
echo "  Open page in Notion → '...' → 'Connect to' → select your integration."
