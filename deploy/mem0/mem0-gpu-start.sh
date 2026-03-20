#!/bin/bash
# Start mem0 + reranker GPU container for ZeroClaw memory backend.
#
# Required env vars:
#   MEM0_LLM_API_KEY or ZAI_API_KEY  — API key for the LLM used in fact extraction
#
# Optional env vars (with defaults):
#   MEM0_LLM_PROVIDER    — mem0 LLM provider (default: "openai" i.e. OpenAI-compatible)
#   MEM0_LLM_MODEL       — LLM model for fact extraction (default: "glm-5-turbo")
#   MEM0_LLM_BASE_URL    — LLM API base URL (default: "https://api.z.ai/api/coding/paas/v4")
#   MEM0_EMBEDDER_MODEL  — embedding model (default: "BAAI/bge-m3")
#   MEM0_EMBEDDER_DIMS   — embedding dimensions (default: "1024")
#   MEM0_EMBEDDER_DEVICE — "cuda", "cpu", or "auto" (default: "cuda")
#   MEM0_VECTOR_COLLECTION — Qdrant collection name (default: "zeroclaw_mem0")
#   RERANKER_MODEL       — reranker model (default: "BAAI/bge-reranker-v2-m3")
#   RERANKER_DEVICE      — "cuda" or "cpu" (default: "cuda")
#   MEM0_PORT            — mem0 server port (default: 8765)
#   RERANKER_PORT        — reranker server port (default: 8678)
#   CONTAINER_IMAGE      — base container image (default: docker.io/kyuz0/amd-strix-halo-comfyui:latest)
#   CONTAINER_NAME       — container name (default: mem0-gpu)
#   DATA_DIR             — host path for Qdrant data (default: ~/mem0-data)
#   SCRIPT_DIR           — host path for server scripts (default: directory of this script)
set -e

# Resolve script directory for mounting server scripts
SCRIPT_DIR="${SCRIPT_DIR:-$(cd "$(dirname "$0")" && pwd)}"

# API key — accept either name
export MEM0_LLM_API_KEY="${MEM0_LLM_API_KEY:-${ZAI_API_KEY:?MEM0_LLM_API_KEY or ZAI_API_KEY must be set}}"

# Defaults
MEM0_LLM_MODEL="${MEM0_LLM_MODEL:-glm-5-turbo}"
MEM0_LLM_BASE_URL="${MEM0_LLM_BASE_URL:-https://api.z.ai/api/coding/paas/v4}"
MEM0_PORT="${MEM0_PORT:-8765}"
RERANKER_PORT="${RERANKER_PORT:-8678}"
CONTAINER_IMAGE="${CONTAINER_IMAGE:-docker.io/kyuz0/amd-strix-halo-comfyui:latest}"
CONTAINER_NAME="${CONTAINER_NAME:-mem0-gpu}"
DATA_DIR="${DATA_DIR:-$HOME/mem0-data}"

# Stop existing CPU services (if any)
kill -9 $(pgrep -f "mem0-server.py") 2>/dev/null || true
kill -9 $(pgrep -f "reranker-server.py") 2>/dev/null || true

# Stop existing container
podman stop "$CONTAINER_NAME" 2>/dev/null || true
podman rm "$CONTAINER_NAME" 2>/dev/null || true

podman run -d --name "$CONTAINER_NAME" \
  --device /dev/dri --device /dev/kfd \
  --group-add video --group-add render \
  --restart unless-stopped \
  -p "$MEM0_PORT:$MEM0_PORT" -p "$RERANKER_PORT:$RERANKER_PORT" \
  -v "$DATA_DIR":/root/mem0-data:Z \
  -v "$SCRIPT_DIR/mem0-server.py":/app/mem0-server.py:ro,Z \
  -v "$SCRIPT_DIR/reranker-server.py":/app/reranker-server.py:ro,Z \
  -v "$HOME/.cache/huggingface":/root/.cache/huggingface:Z \
  -e MEM0_LLM_API_KEY="$MEM0_LLM_API_KEY" \
  -e ZAI_API_KEY="$MEM0_LLM_API_KEY" \
  -e MEM0_LLM_MODEL="$MEM0_LLM_MODEL" \
  -e MEM0_LLM_BASE_URL="$MEM0_LLM_BASE_URL" \
  ${MEM0_LLM_PROVIDER:+-e MEM0_LLM_PROVIDER="$MEM0_LLM_PROVIDER"} \
  ${MEM0_EMBEDDER_MODEL:+-e MEM0_EMBEDDER_MODEL="$MEM0_EMBEDDER_MODEL"} \
  ${MEM0_EMBEDDER_DIMS:+-e MEM0_EMBEDDER_DIMS="$MEM0_EMBEDDER_DIMS"} \
  ${MEM0_EMBEDDER_DEVICE:+-e MEM0_EMBEDDER_DEVICE="$MEM0_EMBEDDER_DEVICE"} \
  ${MEM0_VECTOR_COLLECTION:+-e MEM0_VECTOR_COLLECTION="$MEM0_VECTOR_COLLECTION"} \
  ${RERANKER_MODEL:+-e RERANKER_MODEL="$RERANKER_MODEL"} \
  ${RERANKER_DEVICE:+-e RERANKER_DEVICE="$RERANKER_DEVICE"} \
  -e RERANKER_PORT="$RERANKER_PORT" \
  -e RERANKER_URL="http://127.0.0.1:$RERANKER_PORT/rerank" \
  -e TORCH_ROCM_AOTRITON_ENABLE_EXPERIMENTAL=1 \
  -e HOME=/root \
  "$CONTAINER_IMAGE" \
  bash -c "pip install -q FlagEmbedding mem0ai flask httpx qdrant-client 2>&1 | tail -3; echo '=== Starting reranker (GPU) on :$RERANKER_PORT ==='; python3 /app/reranker-server.py & sleep 3; echo '=== Starting mem0 (GPU) on :$MEM0_PORT ==='; exec python3 /app/mem0-server.py"

echo "Container started, waiting for init..."
sleep 15
echo "=== Container logs ==="
podman logs "$CONTAINER_NAME" 2>&1 | tail -25
echo "=== Port check ==="
ss -tlnp | grep "$MEM0_PORT\|$RERANKER_PORT" || echo "Ports not yet ready, check: podman logs $CONTAINER_NAME"
