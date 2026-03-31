"""Plugin entry-point decorator for JSON marshalling over Extism shared memory."""

from __future__ import annotations

import json
import functools
from typing import Any, Callable

import extism_pdk as pdk


def plugin_fn(func: Callable[[Any], Any]) -> Callable[[], int]:
    """Decorator that wraps a function to handle JSON serialization/deserialization
    over Extism shared memory.

    The decorated function receives a parsed JSON object from Extism input
    and its return value is JSON-serialized back to Extism output.

    Usage:
        @plugin_fn
        def greet(input):
            return {"message": f"Hello, {input['name']}!"}
    """

    @pdk.plugin
    @functools.wraps(func)
    def wrapper() -> int:
        raw_input = pdk.input_string()
        parsed = json.loads(raw_input) if raw_input else None
        result = func(parsed)
        output = json.dumps(result)
        pdk.output_string(output)
        return 0

    return wrapper
