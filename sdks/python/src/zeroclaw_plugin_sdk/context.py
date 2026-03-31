"""Context module — wraps session, user_identity, and agent_config host functions.

Each function calls the corresponding ``context_*`` host function via Extism
shared memory, deserializes the JSON response, and returns a typed dataclass.
"""

from __future__ import annotations

import json
from dataclasses import dataclass, field
from typing import Dict, List

import extism_pdk as pdk


@dataclass
class SessionContext:
    """Runtime session information provided by the host."""

    channel_name: str
    conversation_id: str
    timestamp: str


@dataclass
class UserIdentity:
    """Identity of the user interacting with the agent."""

    username: str
    display_name: str
    channel_user_id: str


@dataclass
class AgentConfig:
    """Agent configuration provided by the host."""

    name: str
    personality_traits: List[str] = field(default_factory=list)
    identity: Dict[str, str] = field(default_factory=dict)


def session() -> SessionContext:
    """Return the current session context from the host.

    Raises ``RuntimeError`` if the host reports an error.
    """
    raw = pdk.host_fn("context_session", "null")
    response = json.loads(raw)
    if isinstance(response, dict) and response.get("error"):
        raise RuntimeError(response["error"])
    return SessionContext(
        channel_name=response["channel_name"],
        conversation_id=response["conversation_id"],
        timestamp=response["timestamp"],
    )


def user_identity() -> UserIdentity:
    """Return the current user identity from the host.

    Raises ``RuntimeError`` if the host reports an error.
    """
    raw = pdk.host_fn("context_user_identity", "null")
    response = json.loads(raw)
    if isinstance(response, dict) and response.get("error"):
        raise RuntimeError(response["error"])
    return UserIdentity(
        username=response["username"],
        display_name=response["display_name"],
        channel_user_id=response["channel_user_id"],
    )


def agent_config() -> AgentConfig:
    """Return the agent configuration from the host.

    Raises ``RuntimeError`` if the host reports an error.
    """
    raw = pdk.host_fn("context_agent_config", "null")
    response = json.loads(raw)
    if isinstance(response, dict) and response.get("error"):
        raise RuntimeError(response["error"])
    return AgentConfig(
        name=response["name"],
        personality_traits=response.get("personality_traits", []),
        identity=response.get("identity", {}),
    )
