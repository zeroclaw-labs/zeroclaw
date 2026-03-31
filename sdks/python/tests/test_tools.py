"""Tests for zeroclaw_plugin_sdk.tools module."""

import json
import sys
import types
from unittest.mock import MagicMock

# Stub extism_pdk before importing the tools module —
# the real PDK is only available inside the Extism WASM runtime.
_pdk = sys.modules.get("extism_pdk")
if _pdk is None:
    _pdk = types.ModuleType("extism_pdk")
    sys.modules["extism_pdk"] = _pdk

_pdk.host_fn = MagicMock()
_pdk.input_string = MagicMock()
_pdk.output_string = MagicMock()
_pdk.plugin = lambda f: f

from zeroclaw_plugin_sdk.tools import tool_call  # noqa: E402

import pytest  # noqa: E402


class TestToolCallExposesFunction:
    """tool_call(tool_name, arguments) is importable and callable."""

    def test_importable_from_module(self):
        from zeroclaw_plugin_sdk import tools
        assert hasattr(tools, "tool_call")
        assert callable(tools.tool_call)


class TestToolCallHostFunction:
    """tool_call invokes zeroclaw_tool_call host function via Extism PDK."""

    def test_calls_host_fn_with_correct_name(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": "ok"}
        )
        tool_call("my_tool", {"key": "value"})
        _pdk.host_fn.assert_called_once()
        assert _pdk.host_fn.call_args[0][0] == "zeroclaw_tool_call"


class TestArgumentsSerialization:
    """Arguments are serialized as JSON matching the Rust SDK wire format."""

    def test_request_matches_rust_wire_format(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": "result"}
        )
        tool_call("search", {"query": "hello", "limit": 10})
        sent_json = _pdk.host_fn.call_args[0][1]
        request = json.loads(sent_json)
        assert request == {
            "tool_name": "search",
            "arguments": {"query": "hello", "limit": 10},
        }

    def test_empty_arguments_default(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": "ok"}
        )
        tool_call("ping")
        sent_json = _pdk.host_fn.call_args[0][1]
        request = json.loads(sent_json)
        assert request == {"tool_name": "ping", "arguments": {}}

    def test_nested_arguments(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": "ok"}
        )
        args = {"filters": {"status": "active", "tags": ["a", "b"]}}
        tool_call("query", args)
        sent_json = _pdk.host_fn.call_args[0][1]
        request = json.loads(sent_json)
        assert request["arguments"] == args


class TestSuccessfulResponse:
    """Successful responses return the output string."""

    def test_returns_output_string(self):
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": "tool output data"}
        )
        result = tool_call("my_tool", {"x": 1})
        assert result == "tool output data"

    def test_returns_empty_output(self):
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": ""}
        )
        result = tool_call("my_tool", {})
        assert result == ""


class TestFailedResponse:
    """Failed responses raise RuntimeError with error message."""

    def test_raises_on_error_field(self):
        _pdk.host_fn.return_value = json.dumps(
            {"error": "tool 'bad_tool' not found in registry"}
        )
        with pytest.raises(RuntimeError, match="tool 'bad_tool' not found in registry"):
            tool_call("bad_tool", {})

    def test_raises_on_success_false(self):
        _pdk.host_fn.return_value = json.dumps(
            {"success": False, "output": ""}
        )
        with pytest.raises(RuntimeError, match="tool call returned success=false"):
            tool_call("my_tool", {})

    def test_raises_on_risk_level_error(self):
        _pdk.host_fn.return_value = json.dumps(
            {"error": "risk level exceeded: tool 'rm' is Critical but caller ceiling is Low"}
        )
        with pytest.raises(RuntimeError, match="risk level exceeded"):
            tool_call("rm", {})


class TestSerializationFormat:
    """Unit tests validate the JSON serialization format matches Rust SDK."""

    def test_tool_name_is_string(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": "ok"}
        )
        tool_call("my_tool", {})
        sent_json = _pdk.host_fn.call_args[0][1]
        request = json.loads(sent_json)
        assert isinstance(request["tool_name"], str)

    def test_arguments_is_object(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": "ok"}
        )
        tool_call("my_tool", {"a": 1})
        sent_json = _pdk.host_fn.call_args[0][1]
        request = json.loads(sent_json)
        assert isinstance(request["arguments"], dict)

    def test_request_has_exactly_two_keys(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps(
            {"success": True, "output": "ok"}
        )
        tool_call("my_tool", {"key": "val"})
        sent_json = _pdk.host_fn.call_args[0][1]
        request = json.loads(sent_json)
        assert set(request.keys()) == {"tool_name", "arguments"}
