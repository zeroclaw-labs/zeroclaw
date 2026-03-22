#!/usr/bin/env python3
"""
ZeroClaw Reranker Server
========================

Lightweight FastAPI server wrapping a cross-encoder reranker model.

Accepts a query + list of candidate documents, returns them reranked
with relevance scores. Designed for ZeroClaw's retrieval pipeline.

Usage:
    pip install fastapi uvicorn sentence-transformers torch
    python reranker-server.py

    # or with a custom model:
    RERANKER_MODEL=BAAI/bge-reranker-v2-m3 python reranker-server.py

Config env vars:
    RERANKER_MODEL   - HuggingFace model name (default: BAAI/bge-reranker-v2-m3)
    RERANKER_HOST    - Listen host (default: 0.0.0.0)
    RERANKER_PORT    - Listen port (default: 8787)
    RERANKER_DEVICE  - torch device: "cpu", "cuda", "mps" (default: auto-detect)
    RERANKER_MAX_LEN - Max sequence length (default: 512)
"""

import os
import time
import logging
from typing import Optional

import uvicorn
from fastapi import FastAPI, HTTPException
from pydantic import BaseModel, Field

logging.basicConfig(level=logging.INFO, format="%(asctime)s %(levelname)s %(message)s")
logger = logging.getLogger("reranker")

# ── Config ───────────────────────────────────────────────────
MODEL_NAME = os.getenv("RERANKER_MODEL", "BAAI/bge-reranker-v2-m3")
HOST = os.getenv("RERANKER_HOST", "0.0.0.0")
PORT = int(os.getenv("RERANKER_PORT", "8787"))
MAX_LEN = int(os.getenv("RERANKER_MAX_LEN", "512"))

def detect_device() -> str:
    """Auto-detect best available device."""
    import torch
    explicit = os.getenv("RERANKER_DEVICE", "").strip()
    if explicit:
        return explicit
    if torch.cuda.is_available():
        return "cuda"
    if hasattr(torch.backends, "mps") and torch.backends.mps.is_available():
        return "mps"
    return "cpu"

# ── Models ───────────────────────────────────────────────────
class RerankRequest(BaseModel):
    query: str = Field(..., description="The search query")
    documents: list[str] = Field(..., description="Candidate documents to rerank")
    top_k: Optional[int] = Field(None, description="Return only top K results (default: all)")

class RerankResult(BaseModel):
    index: int
    document: str
    score: float

class RerankResponse(BaseModel):
    results: list[RerankResult]
    model: str
    elapsed_ms: float

# ── App ──────────────────────────────────────────────────────
app = FastAPI(title="ZeroClaw Reranker", version="1.0.0")

# Global model reference (loaded on startup)
_model = None
_device = None

@app.on_event("startup")
def load_model():
    global _model, _device
    from sentence_transformers import CrossEncoder

    _device = detect_device()
    logger.info(f"Loading reranker model: {MODEL_NAME} on device: {_device}")

    start = time.time()
    _model = CrossEncoder(MODEL_NAME, max_length=MAX_LEN, device=_device)
    elapsed = time.time() - start
    logger.info(f"Model loaded in {elapsed:.1f}s")

@app.get("/health")
def health():
    return {
        "status": "ok",
        "model": MODEL_NAME,
        "device": _device,
    }

@app.post("/rerank", response_model=RerankResponse)
def rerank(req: RerankRequest):
    if _model is None:
        raise HTTPException(status_code=503, detail="Model not loaded yet")

    if not req.documents:
        return RerankResponse(results=[], model=MODEL_NAME, elapsed_ms=0.0)

    if not req.query.strip():
        raise HTTPException(status_code=400, detail="Query must not be empty")

    start = time.time()

    # Build query-document pairs for cross-encoder
    pairs = [(req.query, doc) for doc in req.documents]
    scores = _model.predict(pairs)

    # Build scored results
    scored = []
    for i, (doc, score) in enumerate(zip(req.documents, scores)):
        scored.append(RerankResult(
            index=i,
            document=doc,
            score=float(score),
        ))

    # Sort by score descending
    scored.sort(key=lambda x: x.score, reverse=True)

    # Apply top_k if specified
    if req.top_k is not None and req.top_k > 0:
        scored = scored[:req.top_k]

    elapsed_ms = (time.time() - start) * 1000
    logger.info(
        f"Reranked {len(req.documents)} docs in {elapsed_ms:.1f}ms "
        f"(top score: {scored[0].score:.4f})" if scored else
        f"Reranked 0 docs in {elapsed_ms:.1f}ms"
    )

    return RerankResponse(
        results=scored,
        model=MODEL_NAME,
        elapsed_ms=elapsed_ms,
    )

# ── OpenAI-compatible /v1/rerank endpoint ────────────────────
# Some embedding providers expose reranking at this path.
# We support it for interoperability.

class OpenAIRerankRequest(BaseModel):
    model: Optional[str] = None
    query: str
    documents: list[str]
    top_n: Optional[int] = None

class OpenAIRerankResult(BaseModel):
    index: int
    relevance_score: float

class OpenAIRerankResponse(BaseModel):
    results: list[OpenAIRerankResult]
    model: str

@app.post("/v1/rerank", response_model=OpenAIRerankResponse)
def rerank_v1(req: OpenAIRerankRequest):
    """OpenAI-compatible rerank endpoint."""
    if _model is None:
        raise HTTPException(status_code=503, detail="Model not loaded yet")

    if not req.documents:
        return OpenAIRerankResponse(results=[], model=MODEL_NAME)

    if not req.query.strip():
        raise HTTPException(status_code=400, detail="Query must not be empty")

    pairs = [(req.query, doc) for doc in req.documents]
    scores = _model.predict(pairs)

    scored = []
    for i, score in enumerate(scores):
        scored.append(OpenAIRerankResult(
            index=i,
            relevance_score=float(score),
        ))

    scored.sort(key=lambda x: x.relevance_score, reverse=True)

    if req.top_n is not None and req.top_n > 0:
        scored = scored[:req.top_n]

    return OpenAIRerankResponse(
        results=scored,
        model=req.model or MODEL_NAME,
    )


if __name__ == "__main__":
    uvicorn.run(app, host=HOST, port=PORT, log_level="info")
