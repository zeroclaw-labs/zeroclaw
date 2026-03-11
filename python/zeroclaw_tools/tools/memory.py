"""
Memory storage tools for persisting data between conversations.
"""

import json
from pathlib import Path

from langchain_core.tools import tool


def _get_memory_path() -> Path:
    """Get the path to the memory storage file."""
    return Path.home() / ".zeroclaw" / "memory_store.json"


def _load_memory() -> dict:
    """Load memory from disk."""
    path = _get_memory_path()
    if not path.exists():
        return {}
    try:
        with open(path, "r", encoding="utf-8") as f:
            return json.load(f)
    except Exception:
        return {}


def _save_memory(data: dict) -> None:
    """Save memory to disk."""
    path = _get_memory_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, indent=2)


@tool
def memory_store(key: str, value: str) -> str:
    """
    Store a key-value pair in persistent memory.

    Args:
        key: The key to store under
        value: The value to store

    Returns:
        Confirmation message
    """
    try:
        data = _load_memory()
        data[key] = value
        _save_memory(data)
        return f"Stored: {key}"
    except Exception as e:
        return f"Error: {e}"


@tool
def memory_recall(query: str) -> str:
    """
    Search memory for entries matching the query.

    Args:
        query: The search query

    Returns:
        Matching entries or "no matches" message
    """
    try:
        data = _load_memory()
        if not data:
            return "No memories stored yet"

        query_lower = query.lower()
        matches = {
            k: v
            for k, v in data.items()
            if query_lower in k.lower() or query_lower in str(v).lower()
        }

        if not matches:
            return f"No matches for: {query}"

        return json.dumps(matches, indent=2)
    except Exception as e:
        return f"Error: {e}"
