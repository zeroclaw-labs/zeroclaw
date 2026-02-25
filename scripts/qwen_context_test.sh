#!/bin/bash
# Qwen Context Window Testing Script
# Tests maximum context length for qwen3-coder-plus

set -euo pipefail

ACCESS_TOKEN=$(jq -r .access_token ~/.qwen/oauth_creds.json)
ENDPOINT="https://portal.qwen.ai/v1/chat/completions"
MODEL="qwen3-coder-plus"

# Context sizes to test (in tokens, approximate)
CONTEXT_SIZES=(1024 2048 4096 8192 16384 32768 65536 131072)

echo "=== Qwen Context Window Test ==="
echo "Model: $MODEL"
echo ""

for context_size in "${CONTEXT_SIZES[@]}"; do
  echo -n "Testing ${context_size} tokens ... "

  # Generate dummy text (~4 chars per token)
  char_count=$((context_size * 4))
  dummy_text=$(python3 -c "print('test ' * $context_size)" | head -c "$char_count")

  response=$(curl -s -w "\n%{http_code}" \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "Content-Type: application/json" \
    --data-binary @- \
    "$ENDPOINT" <<EOF
{
  "model": "$MODEL",
  "messages": [
    {"role": "user", "content": "$dummy_text"}
  ],
  "max_tokens": 10
}
EOF
)

  http_code=$(echo "$response" | tail -n1)
  body=$(echo "$response" | head -n-1)

  if [ "$http_code" -eq 200 ]; then
    if echo "$body" | jq -e '.choices[0].message.content' > /dev/null 2>&1; then
      # Check if we got usage stats
      prompt_tokens=$(echo "$body" | jq -r '.usage.prompt_tokens' 2>/dev/null || echo "N/A")
      completion_tokens=$(echo "$body" | jq -r '.usage.completion_tokens' 2>/dev/null || echo "N/A")
      echo "✅ OK (actual: ${prompt_tokens} prompt + ${completion_tokens} completion tokens)"
    else
      echo "❌ Response parse error"
      break
    fi
  else
    error=$(echo "$body" | jq -r '.error.message' 2>/dev/null || echo "HTTP $http_code")
    error_code=$(echo "$body" | jq -r '.error.code' 2>/dev/null || echo "unknown")
    echo "❌ FAILED - $error_code: $error"

    # If we hit a limit, this is likely the max
    if [[ "$error" == *"context"* ]] || [[ "$error" == *"length"* ]] || [[ "$error" == *"token"* ]]; then
      echo ""
      echo "⚠️  Maximum context window reached at ~$context_size tokens"
      break
    fi
    break
  fi

  sleep 1  # Rate limiting
done

echo ""
echo "=== Test Complete ==="
