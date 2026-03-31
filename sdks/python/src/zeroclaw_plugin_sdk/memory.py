"""Memory module — wraps store, recall, and forget host functions.

Each function serializes a typed request dict to JSON, calls the
corresponding ``zeroclaw_memory_*`` host function via Extism shared memory,
and deserializes the JSON response.
"""

from __future__ import annotations

import json

import extism_pdk as pdk


def store(key: str, value: str) -> None:
    """Store a key-value pair in the agent's memory.

    Raises ``RuntimeError`` if the host reports an error.
    """
    request = json.dumps({"key": key, "value": value})
    raw = pdk.host_fn("zeroclaw_memory_store", request)
    response = json.loads(raw)
    if response.get("error"):
        raise RuntimeError(response["error"])
    if not response.get("success"):
        raise RuntimeError("memory store returned success=false")


def recall(query: str) -> str:
    """Recall memories matching the given query string.

    Returns the raw results string from the host.
    Raises ``RuntimeError`` if the host reports an error.
    """
    request = json.dumps({"query": query})
    raw = pdk.host_fn("zeroclaw_memory_recall", request)
    response = json.loads(raw)
    if response.get("error"):
        raise RuntimeError(response["error"])
    return response.get("results", "")


def forget(key: str) -> None:
    """Forget (delete) a memory entry by key.

    Raises ``RuntimeError`` if the host reports an error.
    """
    request = json.dumps({"key": key})
    raw = pdk.host_fn("zeroclaw_memory_forget", request)
    response = json.loads(raw)
    if response.get("error"):
        raise RuntimeError(response["error"])
    if not response.get("success"):
        raise RuntimeError("memory forget returned success=false")
