#!/usr/bin/env bash
# scripts/refresh_model_priorities.sh
# Uses ZeroClaw to research and update the model priority list.

set -e

# 1. Get current available free models from opencode
echo "Fetching available free models..."
ALL_MODELS=$(opencode models || true)
AVAILABLE_MODELS=$(echo "$ALL_MODELS" | grep ":free" || true)
MINIMAX=$(echo "$ALL_MODELS" | grep "minimax" || true)
AUTO_FREE=$(echo "$ALL_MODELS" | grep "openrouter/free" || true)
ALL_FREE="$AVAILABLE_MODELS"$'\n'"$MINIMAX"$'\n'"$AUTO_FREE"

# 2. Use ZeroClaw agent to research and rank
echo "Analyzing benchmarks and ranking models..."
./bazel-bin/zeroclaw agent -m "Research the latest LMSYS Chatbot Arena rankings for coding and general reasoning. 
Specifically look for the top-performing 'free' or 'open-weights' models.
Cross-reference this with the following list of models available in our current CLI:
$ALL_FREE

Based on performance and recent stability (avoid DeepSeek and Gemini if they are having outages, and highly prioritize 'openrouter/openrouter/free' as a stable auto-router), output a JSON array of model IDs in ranked order of preference.
Output ONLY the JSON array, e.g. [\"provider/model:free\", ...]" > ranked_models.txt

# 3. Extract and Verify JSON
# Extract the last valid-looking array from the file (handles trailing text or multiple blocks)
JSON_CONTENT=$(grep -o '\[.*\]' ranked_models.txt | tail -n 1)

if [ -n "$JSON_CONTENT" ]; then
    echo "Verifying model IDs against local registry..."
    
    # Convert local model list to JSON array for jq comparison
    REGISTRY_JSON=$(echo "$ALL_MODELS" | jq -R . | jq -s .)
    
    # Filter the agent's output to only include models that actually exist locally
    # We use -c to ensure compact output and wrap in a check to handle jq errors
    VERIFIED_JSON=$(echo "$JSON_CONTENT" | jq -c --argjson reg "$REGISTRY_JSON" '
        if type == "array" then
            map(select(. as $m | any($reg[]; . == $m)))
        else
            []
        end
    ' 2>/dev/null || echo "[]")
    
    if [ "$VERIFIED_JSON" != "[]" ]; then
        mkdir -p ~/.zeroclaw
        echo "$VERIFIED_JSON" > ~/.zeroclaw/model_priorities.json
        echo "Successfully updated and verified model priorities in ~/.zeroclaw/model_priorities.json"
        cat ~/.zeroclaw/model_priorities.json
    else
        echo "Error: All models returned by agent were rejected (did not match local registry)."
        echo "Agent suggested: $JSON_CONTENT"
        exit 1
    fi
else
    echo "Error: Could not extract ranked model list from agent output."
    cat ranked_models.txt
    exit 1
fi

rm ranked_models.txt
