"""ZeroClaw Plugin SDK — Python bindings for building WASM plugins with Extism PDK."""

from zeroclaw_plugin_sdk.decorator import plugin_fn
from zeroclaw_plugin_sdk import context
from zeroclaw_plugin_sdk import memory
from zeroclaw_plugin_sdk import messaging
from zeroclaw_plugin_sdk import tools

__all__ = ["context", "memory", "messaging", "plugin_fn", "tools"]
__version__ = "0.1.0"
