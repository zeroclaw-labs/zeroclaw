#!/bin/bash
# Qwen Model Discovery Script
# Tests all potential Qwen models through OAuth API

set -euo pipefail

# Configuration
ACCESS_TOKEN=$(jq -r .access_token ~/.qwen/oauth_creds.json)
ENDPOINT="https://portal.qwen.ai/v1/chat/completions"
OUTPUT_FILE="qwen_model_test_results.csv"

# Models to test
MODELS=(
  # Qwen 3.x Series
  "qwen3-coder-plus"
  "qwen3-coder"
  "qwen3-plus"
  "qwen3-turbo"
  "qwen3-14b"
  "qwen3-7b"

  # Qwen 2.x Series
  "qwen2.5-coder-32b"
  "qwen2.5-plus"
  "qwen2.5-turbo"
  "qwq-32b-preview"

  # Generic Names
  "qwen-max"
  "qwen-plus"
  "qwen-turbo"
  "qwen-coder"
)

echo "=== Qwen Model Discovery ==="
echo "Testing ${#MODELS[@]} models..."
echo ""

# Initialize CSV
echo "Model,Status,Response" > "$OUTPUT_FILE"

# Test each model
for model in "${MODELS[@]}"; do
  echo -n "Testing: $model ... "

  response=$(curl -s -w "\n%{http_code}" \
    -H "Authorization: Bearer $ACCESS_TOKEN" \
    -H "Content-Type: application/json" \
    -d "{\"model\":\"$model\",\"messages\":[{\"role\":\"user\",\"content\":\"Hi\"}],\"max_tokens\":5}" \
    "$ENDPOINT")

  # Split response and HTTP code
  http_code=$(echo "$response" | tail -n1)
  body=$(echo "$response" | head -n-1)

  if [ "$http_code" -eq 200 ]; then
    if echo "$body" | jq -e '.choices[0].message.content' > /dev/null 2>&1; then
      content=$(echo "$body" | jq -r '.choices[0].message.content' | tr '\n' ' ' | head -c 100)
      echo "SUCCESS"
      echo "$model,SUCCESS,\"$content\"" >> "$OUTPUT_FILE"
    else
      error=$(echo "$body" | jq -r '.error.message' 2>/dev/null || echo "Parse error")
      echo "FAILED: $error"
      echo "$model,FAILED,\"$error\"" >> "$OUTPUT_FILE"
    fi
  else
    error=$(echo "$body" | jq -r '.error.message' 2>/dev/null || echo "HTTP $http_code")
    echo "FAILED: $error"
    echo "$model,FAILED,\"$error\"" >> "$OUTPUT_FILE"
  fi

  # Rate limit courtesy
  sleep 0.5
done

echo ""
echo "=== Results Summary ==="
echo ""
column -t -s, "$OUTPUT_FILE"

echo ""
echo "=== Statistics ==="
success_count=$(grep -c ",SUCCESS," "$OUTPUT_FILE" || echo 0)
total_count=${#MODELS[@]}
echo "Successful: $success_count / $total_count"
echo ""
echo "Full results saved to: $OUTPUT_FILE"
