"""Tools module — wraps the zeroclaw_tool_call host function.

Serializes a typed request dict to JSON, calls the ``zeroclaw_tool_call``
host function via Extism shared memory, and deserializes the JSON response.
"""

from __future__ import annotations

import json
from typing import Any, Dict

import extism_pdk as pdk


def tool_call(tool_name: str, arguments: Dict[str, Any] | None = None) -> str:
    """Call a tool by name with the given arguments.

    Args:
        tool_name: Name of the tool to invoke.
        arguments: JSON-serializable dict of arguments for the tool.
            Defaults to an empty dict if not provided.

    Returns:
        The output string from the tool on success.

    Raises:
        RuntimeError: If the host reports an error or the call fails.
    """
    request = json.dumps({
        "tool_name": tool_name,
        "arguments": arguments if arguments is not None else {},
    })
    raw = pdk.host_fn("zeroclaw_tool_call", request)
    response = json.loads(raw)
    if response.get("error"):
        raise RuntimeError(response["error"])
    if not response.get("success"):
        raise RuntimeError("tool call returned success=false")
    return response.get("output", "")
