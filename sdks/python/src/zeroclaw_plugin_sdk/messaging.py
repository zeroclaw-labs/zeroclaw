"""Messaging module — wraps send_message and get_channels host functions.

Each function serializes a typed request dict to JSON, calls the
corresponding ``zeroclaw_*`` host function via Extism shared memory,
and deserializes the JSON response.
"""

from __future__ import annotations

import json
from typing import List

import extism_pdk as pdk


def send(channel: str, recipient: str, message: str) -> None:
    """Send a message to a recipient on the given channel.

    Raises ``RuntimeError`` if the host reports an error.
    """
    request = json.dumps({
        "channel": channel,
        "recipient": recipient,
        "message": message,
    })
    raw = pdk.host_fn("zeroclaw_send_message", request)
    response = json.loads(raw)
    if response.get("error"):
        raise RuntimeError(response["error"])
    if not response.get("success"):
        raise RuntimeError("send_message returned success=false")


def get_channels() -> List[str]:
    """Get the list of available channel names.

    Returns a Python list of channel name strings.
    Raises ``RuntimeError`` if the host reports an error.
    """
    request = json.dumps({})
    raw = pdk.host_fn("zeroclaw_get_channels", request)
    response = json.loads(raw)
    if response.get("error"):
        raise RuntimeError(response["error"])
    return response.get("channels", [])
