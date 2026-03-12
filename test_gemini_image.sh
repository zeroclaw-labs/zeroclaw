#!/bin/bash
set -a && source .env && set +a

PROMPT="An astronaut elephant on Mars, cinematic style"
MODEL="imagen-4.0-fast-generate-001"

# Construction du payload
PAYLOAD=$(cat <<JSON
{
  "instances": [
    {
      "prompt": "$PROMPT"
    }
  ],
  "parameters": {
    "sampleCount": 1
  }
}
JSON
)

echo "Testing Gemini Imagen Prediction with model: $MODEL"
curl -X POST "https://generativelanguage.googleapis.com/v1beta/models/$MODEL:predict?key=$GEMINI_API_KEY" \
     -H "Content-Type: application/json" \
     -d "$PAYLOAD" | jq '.' | head -n 50
