"""Tests for zeroclaw_plugin_sdk.messaging module public API surface.

Verifies that the messaging module exposes send(channel, recipient, message)
and get_channels() — acceptance criterion for US-ZCL-32.
"""

import json
import sys
import types
from unittest.mock import MagicMock

# Stub extism_pdk before importing — the real PDK only works inside WASM runtime.
if "extism_pdk" in sys.modules:
    _pdk = sys.modules["extism_pdk"]
else:
    _pdk = types.ModuleType("extism_pdk")
    _pdk.input_string = MagicMock()
    _pdk.output_string = MagicMock()
    _pdk.plugin = lambda f: f
    sys.modules["extism_pdk"] = _pdk
_pdk.host_fn = MagicMock()

from zeroclaw_plugin_sdk import messaging  # noqa: E402
from zeroclaw_plugin_sdk.messaging import send, get_channels  # noqa: E402

# Ensure the messaging module's pdk reference has host_fn (handles test ordering).
messaging.pdk = _pdk


class TestMessagingModuleExposesAPI:
    """zeroclaw_plugin_sdk.messaging must expose send and get_channels."""

    def test_module_has_send(self):
        """messaging module exposes a callable 'send'."""
        assert hasattr(messaging, "send")
        assert callable(messaging.send)

    def test_module_has_get_channels(self):
        """messaging module exposes a callable 'get_channels'."""
        assert hasattr(messaging, "get_channels")
        assert callable(messaging.get_channels)

    def test_send_accepts_channel_recipient_message(self):
        """send() accepts (channel, recipient, message) positional args."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("telegram", "alice", "hello")

        _pdk.host_fn.assert_called_once()
        call_args = _pdk.host_fn.call_args[0]
        assert call_args[0] == "zeroclaw_send_message"
        payload = json.loads(call_args[1])
        assert payload == {"channel": "telegram", "recipient": "alice", "message": "hello"}

    def test_get_channels_accepts_no_args(self):
        """get_channels() accepts no arguments and returns a list."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": ["telegram", "discord"]})

        result = get_channels()

        _pdk.host_fn.assert_called_once()
        call_args = _pdk.host_fn.call_args[0]
        assert call_args[0] == "zeroclaw_get_channels"
        assert result == ["telegram", "discord"]

    def test_importable_from_package(self):
        """send, get_channels are importable from zeroclaw_plugin_sdk.messaging."""
        assert send is messaging.send
        assert get_channels is messaging.get_channels

    def test_messaging_importable_from_sdk_root(self):
        """messaging module is importable from zeroclaw_plugin_sdk."""
        from zeroclaw_plugin_sdk import messaging as msg
        assert msg is messaging


class TestCallsHostFunctions:
    """send() must call zeroclaw_send_message and get_channels() must call
    zeroclaw_get_channels — acceptance criterion for US-ZCL-32."""

    def test_send_calls_zeroclaw_send_message(self):
        """send() delegates to the zeroclaw_send_message host function."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("discord", "bob", "hi")

        _pdk.host_fn.assert_called_once()
        host_fn_name = _pdk.host_fn.call_args[0][0]
        assert host_fn_name == "zeroclaw_send_message"

    def test_get_channels_calls_zeroclaw_get_channels(self):
        """get_channels() delegates to the zeroclaw_get_channels host function."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": []})

        get_channels()

        _pdk.host_fn.assert_called_once()
        host_fn_name = _pdk.host_fn.call_args[0][0]
        assert host_fn_name == "zeroclaw_get_channels"

    def test_send_passes_payload_via_host_fn(self):
        """send() passes serialized JSON payload through pdk.host_fn."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("telegram", "carol", "test msg")

        raw_payload = _pdk.host_fn.call_args[0][1]
        payload = json.loads(raw_payload)
        assert payload["channel"] == "telegram"
        assert payload["recipient"] == "carol"
        assert payload["message"] == "test msg"

    def test_get_channels_passes_empty_payload_via_host_fn(self):
        """get_channels() passes an empty JSON object through pdk.host_fn."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": ["slack"]})

        get_channels()

        raw_payload = _pdk.host_fn.call_args[0][1]
        payload = json.loads(raw_payload)
        assert payload == {}

    def test_send_uses_host_fn_not_direct_call(self):
        """send() uses pdk.host_fn (Extism host function mechanism), not any
        other call path."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("irc", "dave", "yo")

        assert _pdk.host_fn.call_count == 1

    def test_get_channels_uses_host_fn_not_direct_call(self):
        """get_channels() uses pdk.host_fn (Extism host function mechanism)."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": []})

        get_channels()

        assert _pdk.host_fn.call_count == 1


class TestWireFormatMatchesRustSDK:
    """JSON wire format produced by the Python SDK must match the Rust SDK
    exactly — acceptance criterion for US-ZCL-32.

    The Rust SDK (crates/zeroclaw-plugin-sdk/src/messaging.rs) defines:

    send_message request:  {"channel": str, "recipient": str, "message": str}
    send_message response: {"success": bool}  or  {"error": str}

    get_channels request:  {}
    get_channels response: {"channels": [str]}  or  {"error": str}

    Field names, types, and structure must be identical.
    """

    # --- send() request wire format ---

    def test_send_request_has_exactly_three_fields(self):
        """send() request JSON must contain exactly channel, recipient, message."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("telegram", "alice", "hello")

        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert set(payload.keys()) == {"channel", "recipient", "message"}

    def test_send_request_no_extra_fields(self):
        """send() request must not include any extra fields beyond the Rust SDK spec."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("discord", "bob", "hi there")

        payload = json.loads(_pdk.host_fn.call_args[0][1])
        rust_sdk_fields = {"channel", "recipient", "message"}
        extra = set(payload.keys()) - rust_sdk_fields
        assert extra == set(), f"Extra fields not in Rust SDK: {extra}"

    def test_send_request_field_names_are_lowercase(self):
        """Rust SDK uses lowercase field names (serde default); Python must match."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("slack", "carol", "test")

        payload = json.loads(_pdk.host_fn.call_args[0][1])
        for key in payload:
            assert key == key.lower(), f"Field '{key}' must be lowercase"

    def test_send_request_values_are_strings(self):
        """All send() request fields must be JSON strings, matching Rust String type."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("telegram", "dave", "msg")

        payload = json.loads(_pdk.host_fn.call_args[0][1])
        for key, val in payload.items():
            assert isinstance(val, str), f"Field '{key}' must be a string, got {type(val)}"

    def test_send_request_is_valid_json_object(self):
        """send() must send a JSON object (dict), not array or primitive."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("irc", "eve", "hello")

        raw = _pdk.host_fn.call_args[0][1]
        parsed = json.loads(raw)
        assert isinstance(parsed, dict), "Request must be a JSON object"

    # --- get_channels() request wire format ---

    def test_get_channels_request_is_empty_object(self):
        """Rust SDK sends {} for get_channels; Python must match."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": ["slack"]})

        get_channels()

        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert payload == {}, f"get_channels request must be empty object, got {payload}"

    # --- Response deserialization matches Rust SDK ---

    def test_send_accepts_success_true_response(self):
        """Rust SDK deserializes {"success": true}; Python must handle it."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("telegram", "frank", "ok")  # Should not raise

    def test_send_accepts_error_response(self):
        """Rust SDK deserializes {"error": "msg"}; Python must raise RuntimeError."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "channel not found"})

        import pytest
        with pytest.raises(RuntimeError, match="channel not found"):
            send("bad", "nobody", "fail")

    def test_get_channels_deserializes_channels_array(self):
        """Rust SDK deserializes {"channels": [...]}; Python must return list."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": ["a", "b", "c"]})

        result = get_channels()
        assert result == ["a", "b", "c"]

    def test_get_channels_deserializes_empty_channels(self):
        """Rust SDK uses #[serde(default)] on channels vec; empty list is valid."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": []})

        result = get_channels()
        assert result == []

    def test_get_channels_missing_channels_field_returns_empty(self):
        """Rust SDK has #[serde(default)] on channels; omitted field → empty vec."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({})

        result = get_channels()
        assert result == []

    def test_get_channels_error_response(self):
        """Rust SDK checks error field first; Python must raise RuntimeError."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "permission denied"})

        import pytest
        with pytest.raises(RuntimeError, match="permission denied"):
            get_channels()


class TestErrorsRaisedAsExceptions:
    """AC: Errors raised as exceptions (US-ZCL-32).

    Both send() and get_channels() must raise Python exceptions when the
    host returns an error response, rather than silently returning or
    returning an error dict.
    """

    # --- send() error handling ---

    def test_send_raises_runtime_error_on_error_field(self):
        """send() raises RuntimeError when response contains 'error' field."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "channel not found"})

        import pytest
        with pytest.raises(RuntimeError, match="channel not found"):
            send("bad_channel", "alice", "hello")

    def test_send_raises_runtime_error_on_success_false(self):
        """send() raises RuntimeError when response has success=false."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": False})

        import pytest
        with pytest.raises(RuntimeError):
            send("telegram", "bob", "msg")

    def test_send_error_message_preserved_in_exception(self):
        """The host error message must appear in the RuntimeError string."""
        _pdk.host_fn.reset_mock()
        error_msg = "rate limit exceeded"
        _pdk.host_fn.return_value = json.dumps({"error": error_msg})

        import pytest
        with pytest.raises(RuntimeError, match=error_msg):
            send("telegram", "carol", "spam")

    def test_send_does_not_return_error_dict(self):
        """send() must not silently return an error dict — it must raise."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "fail"})

        import pytest
        with pytest.raises(RuntimeError):
            result = send("telegram", "dave", "msg")
            # If we reach here, send() returned instead of raising
            assert result is None or not isinstance(result, dict)

    def test_send_exception_type_is_runtime_error(self):
        """send() must raise RuntimeError specifically (not generic Exception)."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "some error"})

        try:
            send("telegram", "eve", "msg")
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass  # correct
        except Exception:
            assert False, "Expected RuntimeError, got different exception type"

    # --- get_channels() error handling ---

    def test_get_channels_raises_runtime_error_on_error_field(self):
        """get_channels() raises RuntimeError when response contains 'error'."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "permission denied"})

        import pytest
        with pytest.raises(RuntimeError, match="permission denied"):
            get_channels()

    def test_get_channels_error_message_preserved_in_exception(self):
        """The host error message must appear in the RuntimeError string."""
        _pdk.host_fn.reset_mock()
        error_msg = "authentication required"
        _pdk.host_fn.return_value = json.dumps({"error": error_msg})

        import pytest
        with pytest.raises(RuntimeError, match=error_msg):
            get_channels()

    def test_get_channels_does_not_return_error_dict(self):
        """get_channels() must not silently return error info — it must raise."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "fail"})

        import pytest
        with pytest.raises(RuntimeError):
            result = get_channels()
            assert not isinstance(result, dict)

    def test_get_channels_exception_type_is_runtime_error(self):
        """get_channels() must raise RuntimeError specifically."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "broken"})

        try:
            get_channels()
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass
        except Exception:
            assert False, "Expected RuntimeError, got different exception type"

    # --- Edge cases ---

    def test_send_error_field_with_empty_string_is_falsy(self):
        """An empty error string is falsy — send() should NOT raise."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "", "success": True})

        # Empty error string is falsy, so no exception expected
        send("telegram", "frank", "msg")

    def test_get_channels_error_field_with_empty_string_is_falsy(self):
        """An empty error string is falsy — get_channels() should NOT raise."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"error": "", "channels": ["a"]})

        result = get_channels()
        assert result == ["a"]


class TestGetChannelsReturnsPythonListOfStrings:
    """AC: get_channels returns a Python list of strings (US-ZCL-32)."""

    def test_return_type_is_list(self):
        """get_channels() must return an instance of list, not tuple/set/etc."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": ["telegram", "discord"]})

        result = get_channels()
        assert isinstance(result, list), f"Expected list, got {type(result).__name__}"

    def test_elements_are_str(self):
        """Every element in the returned list must be a Python str."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": ["telegram", "discord", "slack"]})

        result = get_channels()
        for i, elem in enumerate(result):
            assert isinstance(elem, str), f"Element {i} is {type(elem).__name__}, expected str"

    def test_single_channel_returns_list(self):
        """A single-element channels array must still return a list (not bare str)."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": ["telegram"]})

        result = get_channels()
        assert isinstance(result, list)
        assert len(result) == 1
        assert result[0] == "telegram"

    def test_empty_channels_returns_empty_list(self):
        """Empty channels array returns an empty list (not None or other falsy)."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": []})

        result = get_channels()
        assert isinstance(result, list)
        assert result == []

    def test_many_channels_preserves_order(self):
        """Returned list preserves the order from the host response."""
        _pdk.host_fn.reset_mock()
        channels = ["alpha", "bravo", "charlie", "delta", "echo"]
        _pdk.host_fn.return_value = json.dumps({"channels": channels})

        result = get_channels()
        assert result == channels


class TestSerializationValidation:
    """AC: Unit tests validate serialization (US-ZCL-32).

    Verifies JSON serialization roundtrip fidelity for both request
    payloads (Python → JSON → host) and response payloads (host → JSON →
    Python), including edge cases like unicode and special characters.
    """

    # --- send() request serialization ---

    def test_send_serializes_to_valid_json_string(self):
        """send() must pass a raw JSON string to the host function."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("telegram", "alice", "hello")

        raw = _pdk.host_fn.call_args[0][1]
        assert isinstance(raw, str)
        # Must be parseable JSON
        parsed = json.loads(raw)
        assert isinstance(parsed, dict)

    def test_send_serializes_unicode_correctly(self):
        """Unicode in channel/recipient/message must survive serialization."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("télégram", "アリス", "こんにちは 🌍")

        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert payload["channel"] == "télégram"
        assert payload["recipient"] == "アリス"
        assert payload["message"] == "こんにちは 🌍"

    def test_send_serializes_empty_strings(self):
        """Empty strings are valid and must serialize as empty JSON strings."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("", "", "")

        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert payload == {"channel": "", "recipient": "", "message": ""}

    def test_send_serializes_special_json_characters(self):
        """Characters that need JSON escaping (quotes, backslashes, newlines)."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        send("chan", "bob", 'line1\nline2\t"quoted"\\back')

        raw = _pdk.host_fn.call_args[0][1]
        # Must be valid JSON (json.loads would raise on bad escaping)
        payload = json.loads(raw)
        assert payload["message"] == 'line1\nline2\t"quoted"\\back'

    # --- get_channels() request serialization ---

    def test_get_channels_serializes_empty_object(self):
        """get_channels() request must serialize as exactly '{}'."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": []})

        get_channels()

        raw = _pdk.host_fn.call_args[0][1]
        assert json.loads(raw) == {}

    # --- Response deserialization ---

    def test_send_deserializes_success_response(self):
        """send() correctly deserializes a {"success": true} response."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        # Should not raise
        send("ch", "user", "msg")

    def test_get_channels_deserializes_unicode_channel_names(self):
        """Unicode channel names must survive response deserialization."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"channels": ["général", "日本語"]})

        result = get_channels()
        assert result == ["général", "日本語"]

    def test_send_request_json_roundtrip_matches_input(self):
        """Serialized JSON, when deserialized, must exactly reproduce the inputs."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        channel, recipient, message = "discord", "carol", "test message"
        send(channel, recipient, message)

        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert payload["channel"] == channel
        assert payload["recipient"] == recipient
        assert payload["message"] == message

    def test_get_channels_response_roundtrip_preserves_values(self):
        """Deserialized channel list must exactly match the JSON input values."""
        _pdk.host_fn.reset_mock()
        expected = ["alpha", "bravo", "charlie"]
        _pdk.host_fn.return_value = json.dumps({"channels": expected})

        result = get_channels()
        assert result == expected
