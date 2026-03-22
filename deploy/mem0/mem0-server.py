"""Minimal OpenMemory-compatible REST server wrapping mem0 Python SDK."""
import asyncio
import json, os, uuid, httpx
from datetime import datetime, timezone
from fastapi import FastAPI, Query
from pydantic import BaseModel
from typing import Optional
from mem0 import Memory

app = FastAPI()

RERANKER_URL = os.environ.get("RERANKER_URL", "http://127.0.0.1:8678/rerank")

CUSTOM_EXTRACTION_PROMPT = """You are a memory extraction specialist for a Cantonese/Chinese chat assistant.

Extract ONLY important, persistent facts from the conversation. Rules:
1. Extract personal preferences, habits, relationships, names, locations
2. Extract decisions, plans, and commitments people make
3. SKIP small talk, greetings, reactions ("ok", "哈哈", "係呀")
4. SKIP temporary states ("我依家食緊飯") unless they reveal a habit
5. Keep facts in the ORIGINAL language (Cantonese/Chinese/English)
6. For each fact, note WHO it's about (use their name or identifier if known)
7. Merge/update existing facts rather than creating duplicates

Return a list of facts in JSON format: {"facts": ["fact1", "fact2", ...]}
"""

PROCEDURAL_EXTRACTION_PROMPT = """You are a procedural memory specialist for an AI assistant.

Extract HOW-TO patterns and reusable procedures from the conversation trace. Rules:
1. Identify step-by-step procedures the assistant followed to accomplish a task
2. Extract tool usage patterns: which tools were called, in what order, with what arguments
3. Capture decision points: why the assistant chose one approach over another
4. Note error-recovery patterns: what failed, how it was fixed
5. Keep the procedure generic enough to apply to similar future tasks
6. Preserve technical details (commands, file paths, API calls) that are reusable
7. SKIP greetings, small talk, and conversational filler
8. Format each procedure as: "To [goal]: [step1] -> [step2] -> ... -> [result]"

Return a list of procedures in JSON format: {"facts": ["procedure1", "procedure2", ...]}
"""

# ── Configurable via environment variables ─────────────────────────
# LLM (for fact extraction when infer=true)
MEM0_LLM_PROVIDER = os.environ.get("MEM0_LLM_PROVIDER", "openai")      # "openai" (compatible), "anthropic", etc.
MEM0_LLM_MODEL = os.environ.get("MEM0_LLM_MODEL", "glm-5-turbo")
MEM0_LLM_API_KEY = os.environ.get("MEM0_LLM_API_KEY") or os.environ.get("ZAI_API_KEY", "")
MEM0_LLM_BASE_URL = os.environ.get("MEM0_LLM_BASE_URL", "https://api.z.ai/api/coding/paas/v4")

# Embedder
MEM0_EMBEDDER_PROVIDER = os.environ.get("MEM0_EMBEDDER_PROVIDER", "huggingface")  # "huggingface", "openai", etc.
MEM0_EMBEDDER_MODEL = os.environ.get("MEM0_EMBEDDER_MODEL", "BAAI/bge-m3")
MEM0_EMBEDDER_DIMS = int(os.environ.get("MEM0_EMBEDDER_DIMS", "1024"))
MEM0_EMBEDDER_DEVICE = os.environ.get("MEM0_EMBEDDER_DEVICE", "cuda")   # "cuda", "cpu", "auto"

# Vector store
MEM0_VECTOR_PROVIDER = os.environ.get("MEM0_VECTOR_PROVIDER", "qdrant")  # "qdrant", "chroma", etc.
MEM0_VECTOR_COLLECTION = os.environ.get("MEM0_VECTOR_COLLECTION", "zeroclaw_mem0")
MEM0_VECTOR_PATH = os.environ.get("MEM0_VECTOR_PATH", os.path.expanduser("~/mem0-data/qdrant"))

config = {
    "llm": {
        "provider": MEM0_LLM_PROVIDER,
        "config": {
            "model": MEM0_LLM_MODEL,
            "api_key": MEM0_LLM_API_KEY,
            "openai_base_url": MEM0_LLM_BASE_URL,
        },
    },
    "embedder": {
        "provider": MEM0_EMBEDDER_PROVIDER,
        "config": {
            "model": MEM0_EMBEDDER_MODEL,
            "embedding_dims": MEM0_EMBEDDER_DIMS,
            "model_kwargs": {"device": MEM0_EMBEDDER_DEVICE},
        },
    },
    "vector_store": {
        "provider": MEM0_VECTOR_PROVIDER,
        "config": {
            "collection_name": MEM0_VECTOR_COLLECTION,
            "embedding_model_dims": MEM0_EMBEDDER_DIMS,
            "path": MEM0_VECTOR_PATH,
            "on_disk": True,
        },
    },
    "custom_fact_extraction_prompt": CUSTOM_EXTRACTION_PROMPT,
}

m = Memory.from_config(config)


def _patch_memory_metadata(mem_instance, memory_id: str, new_metadata: dict):
    """Patch metadata on an existing memory entry via the vector store.

    mem0's Memory.update() only accepts (id, data) with no metadata param,
    so we reach into the underlying vector store to merge new metadata fields.
    """
    try:
        existing = mem_instance.get(memory_id)
    except Exception:
        existing = None
    if not existing or not isinstance(existing, dict):
        return
    old_meta = existing.get("metadata", {}) or {}
    merged = {**old_meta, **new_metadata}
    # Access the vector store's client to set_payload directly
    vs = getattr(mem_instance, "vector_store", None)
    client = getattr(vs, "client", None) if vs else None
    collection = getattr(vs, "collection_name", MEM0_VECTOR_COLLECTION) if vs else MEM0_VECTOR_COLLECTION
    if client is not None:
        try:
            from qdrant_client.models import PointIdsList
            client.set_payload(
                collection_name=collection,
                payload={"metadata": merged},
                points=PointIdsList(points=[memory_id]),
            )
            return
        except Exception as e:
            print(f"qdrant set_payload failed for {memory_id}: {e}")
    # Fallback: use mem0's update with the same text to trigger metadata refresh
    try:
        mem_instance.update(memory_id, data=existing.get("memory", ""))
    except Exception:
        pass


def rerank_results(query: str, items: list, top_k: int = 10) -> list:
    """Rerank search results using bge-reranker-v2-m3."""
    if not items:
        return items
    documents = [item.get("memory", "") for item in items]
    try:
        resp = httpx.post(
            RERANKER_URL,
            json={"query": query, "documents": documents, "top_k": top_k},
            timeout=10.0,
        )
        resp.raise_for_status()
        ranked = resp.json().get("results", [])
        return [items[r["index"]] for r in ranked]
    except Exception as e:
        print(f"Reranker failed, using original order: {e}")
        return items


class AddMemoryRequest(BaseModel):
    user_id: str
    text: str
    metadata: Optional[dict] = None
    infer: bool = True
    app: Optional[str] = None
    custom_instructions: Optional[str] = None
    # LLM overrides — passed from zeroclaw config to override server defaults
    llm_model: Optional[str] = None
    llm_api_key: Optional[str] = None
    llm_base_url: Optional[str] = None


def _get_memory_instance(req: AddMemoryRequest):
    """Return a Memory instance, applying per-request LLM overrides if provided."""
    if not req.llm_model and not req.llm_api_key and not req.llm_base_url:
        return m  # use default singleton
    # Build override config
    override_config = json.loads(json.dumps(config))  # deep copy
    if req.llm_model:
        override_config["llm"]["config"]["model"] = req.llm_model
    if req.llm_api_key:
        override_config["llm"]["config"]["api_key"] = req.llm_api_key
    if req.llm_base_url:
        override_config["llm"]["config"]["openai_base_url"] = req.llm_base_url
    return Memory.from_config(override_config)


@app.post("/api/v1/memories/")
async def add_memory(req: AddMemoryRequest):
    # Use client-supplied prompt, fall back to server default, then mem0 SDK default
    prompt = req.custom_instructions or CUSTOM_EXTRACTION_PROMPT
    mem_instance = _get_memory_instance(req)
    result = await asyncio.to_thread(mem_instance.add, req.text, user_id=req.user_id, metadata=req.metadata or {}, prompt=prompt)

    # mem0 SDK's ADD/UPDATE events don't propagate request metadata to the
    # resulting entries. Post-patch affected entries via the vector store directly,
    # since Memory.update() only accepts (memory_id, data) with no metadata param.
    if req.metadata:
        results = result.get("results", []) if isinstance(result, dict) else result if isinstance(result, list) else []
        for r in results:
            if not isinstance(r, dict):
                continue
            event = r.get("event", "")
            mem_id = r.get("id")
            if mem_id and event in ("ADD", "UPDATE"):
                try:
                    _patch_memory_metadata(mem_instance, mem_id, req.metadata)
                except Exception as e:
                    print(f"metadata patch failed for {mem_id}: {e}")

    return {"id": str(uuid.uuid4()), "status": "ok", "result": result}


class ProceduralMemoryRequest(BaseModel):
    user_id: str
    messages: list[dict]
    metadata: Optional[dict] = None


@app.post("/api/v1/memories/procedural")
async def add_procedural_memory(req: ProceduralMemoryRequest):
    """Store a conversation trace as procedural memory.

    Accepts a list of messages (role/content dicts) representing a full
    conversation turn including tool calls, then uses mem0's native
    procedural memory extraction to learn reusable "how to" patterns.
    """
    # Build metadata with procedural type marker
    meta = {"type": "procedural"}
    if req.metadata:
        meta.update(req.metadata)

    # Use mem0's native message list support + procedural prompt
    result = await asyncio.to_thread(m.add,
        req.messages,
        user_id=req.user_id,
        metadata=meta,
        prompt=PROCEDURAL_EXTRACTION_PROMPT,
    )

    return {"id": str(uuid.uuid4()), "status": "ok", "result": result}


def _parse_mem0_results(raw_results) -> list:
    raw = raw_results.get("results", raw_results) if isinstance(raw_results, dict) else raw_results
    items = []
    # Fields that may be stored as top-level qdrant payload fields (via
    # _patch_memory_metadata) but need to be surfaced under "metadata" in the
    # API response so the Rust client can deserialize them.
    SCOPE_FIELDS = ("scope", "scope_user", "scope_group", "key", "category", "session_id")
    for r in raw:
        item = r if isinstance(r, dict) else {"memory": str(r)}
        meta = dict(item.get("metadata", {}) or {})
        # Promote top-level scope fields into metadata dict if missing
        for field in SCOPE_FIELDS:
            if field in item and field not in meta:
                meta[field] = item[field]
        items.append({
            "id": item.get("id", str(uuid.uuid4())),
            "memory": item.get("memory", item.get("text", "")),
            "created_at": item.get("created_at", datetime.now(timezone.utc).isoformat()),
            "metadata": meta,
        })
    return items


def _parse_iso_timestamp(value: str) -> Optional[datetime]:
    """Parse an ISO 8601 timestamp string, returning None on failure."""
    try:
        dt = datetime.fromisoformat(value)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt
    except (ValueError, TypeError):
        return None


def _item_created_at(item: dict) -> Optional[datetime]:
    """Extract created_at from an item as a timezone-aware datetime."""
    raw = item.get("created_at")
    if raw is None:
        return None
    if isinstance(raw, datetime):
        if raw.tzinfo is None:
            raw = raw.replace(tzinfo=timezone.utc)
        return raw
    return _parse_iso_timestamp(str(raw))


def _apply_post_filters(
    items: list,
    created_after: Optional[str],
    created_before: Optional[str],
) -> list:
    """Filter items by created_after / created_before timestamps (post-query)."""
    after_dt = _parse_iso_timestamp(created_after) if created_after else None
    before_dt = _parse_iso_timestamp(created_before) if created_before else None
    if after_dt is None and before_dt is None:
        return items
    filtered = []
    for item in items:
        ts = _item_created_at(item)
        if ts is None:
            # Keep items without a parseable timestamp
            filtered.append(item)
            continue
        if after_dt and ts < after_dt:
            continue
        if before_dt and ts > before_dt:
            continue
        filtered.append(item)
    return filtered


@app.get("/api/v1/memories/")
async def list_or_search_memories(
    user_id: str = Query(...),
    search_query: Optional[str] = Query(None),
    size: int = Query(10),
    rerank: bool = Query(True),
    created_after: Optional[str] = Query(None),
    created_before: Optional[str] = Query(None),
    metadata_filter: Optional[str] = Query(None),
):
    # Build mem0 SDK filters dict from metadata_filter JSON param
    sdk_filters = None
    if metadata_filter:
        try:
            sdk_filters = json.loads(metadata_filter)
        except json.JSONDecodeError:
            sdk_filters = None

    if search_query:
        # Fetch more results than needed so reranker has candidates to work with
        fetch_size = min(size * 3, 50)
        results = await asyncio.to_thread(m.search,
            search_query,
            user_id=user_id,
            limit=fetch_size,
            filters=sdk_filters,
        )
        items = _parse_mem0_results(results)
        items = _apply_post_filters(items, created_after, created_before)
        if rerank and items:
            items = rerank_results(search_query, items, top_k=size)
        else:
            items = items[:size]
        return {"items": items, "total": len(items)}
    else:
        results = await asyncio.to_thread(m.get_all,user_id=user_id, filters=sdk_filters)
        items = _parse_mem0_results(results)
        items = _apply_post_filters(items, created_after, created_before)
        return {"items": items, "total": len(items)}


@app.delete("/api/v1/memories/{memory_id}")
async def delete_memory(memory_id: str):
    try:
        await asyncio.to_thread(m.delete, memory_id)
    except Exception:
        pass
    return {"status": "ok"}


@app.get("/api/v1/memories/{memory_id}/history")
async def get_memory_history(memory_id: str):
    """Return the edit history of a specific memory."""
    try:
        history = await asyncio.to_thread(m.history, memory_id)
        # Normalize to list of dicts
        entries = []
        raw = history if isinstance(history, list) else history.get("results", history) if isinstance(history, dict) else [history]
        for h in raw:
            entry = h if isinstance(h, dict) else {"event": str(h)}
            entries.append(entry)
        return {"memory_id": memory_id, "history": entries}
    except Exception as e:
        return {"memory_id": memory_id, "history": [], "error": str(e)}


if __name__ == "__main__":
    import uvicorn
    uvicorn.run(app, host="0.0.0.0", port=8765)
