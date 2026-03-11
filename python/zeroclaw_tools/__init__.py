"""
ZeroClaw Tools - LangGraph-based tool calling for consistent LLM agent execution.

This package provides a reliable tool-calling layer for LLM providers that may have
inconsistent native tool calling behavior. Built on LangGraph for guaranteed execution.
"""

from .agent import create_agent, ZeroclawAgent
from .tools import (
    shell,
    file_read,
    file_write,
    web_search,
    http_request,
    memory_store,
    memory_recall,
)
from .tools.base import tool

__version__ = "0.1.0"
__all__ = [
    "create_agent",
    "ZeroclawAgent",
    "tool",
    "shell",
    "file_read",
    "file_write",
    "web_search",
    "http_request",
    "memory_store",
    "memory_recall",
]
