#!/bin/bash
# Qwen Latency Benchmark
# Measures average response time for 10 requests

set -euo pipefail

ITERATIONS=10
PROVIDER="qwen-code"
MODEL="qwen3-coder-plus"
MESSAGE="test latency"

echo "=== Qwen Latency Benchmark ==="
echo "Provider: $PROVIDER"
echo "Model: $MODEL"
echo "Iterations: $ITERATIONS"
echo ""

total_time=0

for i in $(seq 1 $ITERATIONS); do
  echo -n "Request $i ... "

  start=$(date +%s.%N)

  cargo run --release -- agent -p "$PROVIDER" --model "$MODEL" -m "$MESSAGE" > /dev/null 2>&1

  end=$(date +%s.%N)
  elapsed=$(echo "$end - $start" | bc)
  total_time=$(echo "$total_time + $elapsed" | bc)

  echo "${elapsed}s"

  # Small delay to avoid rate limiting
  sleep 0.5
done

echo ""
echo "=== Results ==="
avg_time=$(echo "scale=3; $total_time / $ITERATIONS" | bc)
echo "Total time: ${total_time}s"
echo "Average latency: ${avg_time}s"
echo ""

if (( $(echo "$avg_time < 5.0" | bc -l) )); then
  echo "✅ PASS: Average latency < 5s"
else
  echo "⚠️  WARNING: Average latency >= 5s"
fi
