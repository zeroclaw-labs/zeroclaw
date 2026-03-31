"""Tests for zeroclaw_plugin_sdk.context module public API surface.

Verifies that the context module exposes session(), user_identity(), and
agent_config() — each returning a typed dataclass. Acceptance criteria
for US-ZCL-33.
"""

import json
import sys
import types
from dataclasses import fields as dc_fields
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

from zeroclaw_plugin_sdk import context  # noqa: E402
from zeroclaw_plugin_sdk.context import (  # noqa: E402
    AgentConfig,
    SessionContext,
    UserIdentity,
    agent_config,
    session,
    user_identity,
)

# Ensure the context module's pdk reference has host_fn (handles test ordering).
context.pdk = _pdk

# ---------------------------------------------------------------------------
# Golden JSON responses matching the Rust SDK wire format
# ---------------------------------------------------------------------------
GOLDEN_SESSION = {
    "channel_name": "telegram",
    "conversation_id": "conv-42",
    "timestamp": "2026-03-30T12:00:00Z",
}

GOLDEN_USER_IDENTITY = {
    "username": "jdoe",
    "display_name": "Jane Doe",
    "channel_user_id": "U12345",
}

GOLDEN_AGENT_CONFIG = {
    "name": "ZeroClaw",
    "personality_traits": ["friendly", "concise"],
    "identity": {"role": "assistant", "team": "engineering"},
}


class TestContextModuleExposesAPI:
    """zeroclaw_plugin_sdk.context must expose session, user_identity, and agent_config."""

    def test_module_has_session(self):
        assert hasattr(context, "session")
        assert callable(context.session)

    def test_module_has_user_identity(self):
        assert hasattr(context, "user_identity")
        assert callable(context.user_identity)

    def test_module_has_agent_config(self):
        assert hasattr(context, "agent_config")
        assert callable(context.agent_config)

    def test_importable_from_package(self):
        assert session is context.session
        assert user_identity is context.user_identity
        assert agent_config is context.agent_config


class TestReturnsTypedDataclasses:
    """Each function returns a Python dataclass (SessionContext, UserIdentity, AgentConfig).

    Acceptance criterion for US-ZCL-33 (AC-2).
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()

    def test_session_returns_session_context(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        result = session()
        assert isinstance(result, SessionContext)

    def test_session_context_has_correct_fields(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        result = session()
        assert result.channel_name == "telegram"
        assert result.conversation_id == "conv-42"
        assert result.timestamp == "2026-03-30T12:00:00Z"

    def test_session_context_is_dataclass(self):
        names = {f.name for f in dc_fields(SessionContext)}
        assert names == {"channel_name", "conversation_id", "timestamp"}

    def test_user_identity_returns_user_identity(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        result = user_identity()
        assert isinstance(result, UserIdentity)

    def test_user_identity_has_correct_fields(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        result = user_identity()
        assert result.username == "jdoe"
        assert result.display_name == "Jane Doe"
        assert result.channel_user_id == "U12345"

    def test_user_identity_is_dataclass(self):
        names = {f.name for f in dc_fields(UserIdentity)}
        assert names == {"username", "display_name", "channel_user_id"}

    def test_agent_config_returns_agent_config(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        result = agent_config()
        assert isinstance(result, AgentConfig)

    def test_agent_config_has_correct_fields(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        result = agent_config()
        assert result.name == "ZeroClaw"
        assert result.personality_traits == ["friendly", "concise"]
        assert result.identity == {"role": "assistant", "team": "engineering"}

    def test_agent_config_is_dataclass(self):
        names = {f.name for f in dc_fields(AgentConfig)}
        assert names == {"name", "personality_traits", "identity"}

    def test_agent_config_defaults_for_optional_fields(self):
        """personality_traits and identity default to empty when missing from JSON."""
        _pdk.host_fn.return_value = json.dumps({"name": "minimal"})
        result = agent_config()
        assert result.personality_traits == []
        assert result.identity == {}


class TestHostFunctionCalls:
    """Functions call context_session/user_identity/agent_config host functions.

    Acceptance criterion for US-ZCL-33 (AC-3).
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()

    # -- session -------------------------------------------------------------

    def test_session_calls_host_fn_on_pdk(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        session()
        assert context.pdk is _pdk
        _pdk.host_fn.assert_called_once()

    def test_session_uses_correct_host_function_name(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        session()
        host_fn_name = _pdk.host_fn.call_args[0][0]
        assert host_fn_name == "context_session"

    def test_session_sends_null_payload(self):
        """context_session takes Json<()> in Rust — Python sends 'null'."""
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        session()
        payload = _pdk.host_fn.call_args[0][1]
        assert payload == "null"

    def test_session_makes_exactly_one_host_call(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        session()
        assert _pdk.host_fn.call_count == 1

    # -- user_identity -------------------------------------------------------

    def test_user_identity_calls_host_fn_on_pdk(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        user_identity()
        assert context.pdk is _pdk
        _pdk.host_fn.assert_called_once()

    def test_user_identity_uses_correct_host_function_name(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        user_identity()
        host_fn_name = _pdk.host_fn.call_args[0][0]
        assert host_fn_name == "context_user_identity"

    def test_user_identity_sends_null_payload(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        user_identity()
        payload = _pdk.host_fn.call_args[0][1]
        assert payload == "null"

    def test_user_identity_makes_exactly_one_host_call(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        user_identity()
        assert _pdk.host_fn.call_count == 1

    # -- agent_config --------------------------------------------------------

    def test_agent_config_calls_host_fn_on_pdk(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        agent_config()
        assert context.pdk is _pdk
        _pdk.host_fn.assert_called_once()

    def test_agent_config_uses_correct_host_function_name(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        agent_config()
        host_fn_name = _pdk.host_fn.call_args[0][0]
        assert host_fn_name == "context_agent_config"

    def test_agent_config_sends_null_payload(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        agent_config()
        payload = _pdk.host_fn.call_args[0][1]
        assert payload == "null"

    def test_agent_config_makes_exactly_one_host_call(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        agent_config()
        assert _pdk.host_fn.call_count == 1

    # -- cross-cutting -------------------------------------------------------

    def test_all_functions_use_same_pdk_host_fn(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.side_effect = [
            json.dumps(GOLDEN_SESSION),
            json.dumps(GOLDEN_USER_IDENTITY),
            json.dumps(GOLDEN_AGENT_CONFIG),
        ]
        session()
        user_identity()
        agent_config()
        assert _pdk.host_fn.call_count == 3
        names = [call[0][0] for call in _pdk.host_fn.call_args_list]
        assert names == [
            "context_session",
            "context_user_identity",
            "context_agent_config",
        ]

    def test_host_fn_receives_string_not_bytes(self):
        _pdk.host_fn.side_effect = [
            json.dumps(GOLDEN_SESSION),
            json.dumps(GOLDEN_USER_IDENTITY),
            json.dumps(GOLDEN_AGENT_CONFIG),
        ]
        session()
        user_identity()
        agent_config()
        for call in _pdk.host_fn.call_args_list:
            payload = call[0][1]
            assert isinstance(payload, str), f"Expected str payload, got {type(payload)}"


class TestWireFormatMatchesRustSDK:
    """JSON wire format matches the Rust SDK exactly.

    Acceptance criterion for US-ZCL-33 (AC-4).
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.side_effect = None

    # -- SessionContext fields -----------------------------------------------

    def test_session_response_has_channel_name_string(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        result = session()
        assert isinstance(result.channel_name, str)

    def test_session_response_has_conversation_id_string(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        result = session()
        assert isinstance(result.conversation_id, str)

    def test_session_response_has_timestamp_string(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        result = session()
        assert isinstance(result.timestamp, str)

    def test_session_response_field_names_match_rust(self):
        """SessionContext field names must be snake_case matching Rust serde."""
        names = {f.name for f in dc_fields(SessionContext)}
        assert names == {"channel_name", "conversation_id", "timestamp"}

    # -- UserIdentity fields -------------------------------------------------

    def test_user_identity_response_has_username_string(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        result = user_identity()
        assert isinstance(result.username, str)

    def test_user_identity_response_has_display_name_string(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        result = user_identity()
        assert isinstance(result.display_name, str)

    def test_user_identity_response_has_channel_user_id_string(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        result = user_identity()
        assert isinstance(result.channel_user_id, str)

    def test_user_identity_response_field_names_match_rust(self):
        names = {f.name for f in dc_fields(UserIdentity)}
        assert names == {"username", "display_name", "channel_user_id"}

    # -- AgentConfig fields --------------------------------------------------

    def test_agent_config_response_has_name_string(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        result = agent_config()
        assert isinstance(result.name, str)

    def test_agent_config_response_has_personality_traits_list(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        result = agent_config()
        assert isinstance(result.personality_traits, list)
        assert all(isinstance(t, str) for t in result.personality_traits)

    def test_agent_config_response_has_identity_dict(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        result = agent_config()
        assert isinstance(result.identity, dict)
        assert all(isinstance(k, str) and isinstance(v, str) for k, v in result.identity.items())

    def test_agent_config_response_field_names_match_rust(self):
        names = {f.name for f in dc_fields(AgentConfig)}
        assert names == {"name", "personality_traits", "identity"}

    # -- golden round-trip tests ---------------------------------------------

    def test_session_golden_round_trip(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        result = session()
        assert result.channel_name == GOLDEN_SESSION["channel_name"]
        assert result.conversation_id == GOLDEN_SESSION["conversation_id"]
        assert result.timestamp == GOLDEN_SESSION["timestamp"]

    def test_user_identity_golden_round_trip(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        result = user_identity()
        assert result.username == GOLDEN_USER_IDENTITY["username"]
        assert result.display_name == GOLDEN_USER_IDENTITY["display_name"]
        assert result.channel_user_id == GOLDEN_USER_IDENTITY["channel_user_id"]

    def test_agent_config_golden_round_trip(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        result = agent_config()
        assert result.name == GOLDEN_AGENT_CONFIG["name"]
        assert result.personality_traits == GOLDEN_AGENT_CONFIG["personality_traits"]
        assert result.identity == GOLDEN_AGENT_CONFIG["identity"]

    # -- exact field count parity with Rust SDK -------------------------------

    def test_session_context_has_exactly_3_fields(self):
        """Rust SessionContext has exactly 3 fields — Python must match."""
        assert len(dc_fields(SessionContext)) == 3

    def test_user_identity_has_exactly_3_fields(self):
        """Rust UserIdentity has exactly 3 fields — Python must match."""
        assert len(dc_fields(UserIdentity)) == 3

    def test_agent_config_has_exactly_3_fields(self):
        """Rust AgentConfig has exactly 3 fields — Python must match."""
        assert len(dc_fields(AgentConfig)) == 3

    # -- Python→JSON round-trip produces Rust-compatible wire format --------

    def test_session_context_serializes_to_rust_wire_format(self):
        """Serializing a Python SessionContext back to JSON must produce
        keys that match the Rust serde output exactly."""
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        ctx = session()
        serialized = {
            "channel_name": ctx.channel_name,
            "conversation_id": ctx.conversation_id,
            "timestamp": ctx.timestamp,
        }
        assert serialized == GOLDEN_SESSION

    def test_user_identity_serializes_to_rust_wire_format(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        uid = user_identity()
        serialized = {
            "username": uid.username,
            "display_name": uid.display_name,
            "channel_user_id": uid.channel_user_id,
        }
        assert serialized == GOLDEN_USER_IDENTITY

    def test_agent_config_serializes_to_rust_wire_format(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        cfg = agent_config()
        serialized = {
            "name": cfg.name,
            "personality_traits": cfg.personality_traits,
            "identity": cfg.identity,
        }
        assert serialized == GOLDEN_AGENT_CONFIG

    # -- JSON key names use snake_case (no camelCase / kebab-case) ---------

    def test_golden_json_keys_are_all_snake_case(self):
        """Rust serde defaults to snake_case — verify golden payloads match."""
        import re
        snake = re.compile(r"^[a-z][a-z0-9]*(_[a-z0-9]+)*$")
        for golden in (GOLDEN_SESSION, GOLDEN_USER_IDENTITY, GOLDEN_AGENT_CONFIG):
            for key in golden:
                assert snake.match(key), f"Key '{key}' is not snake_case"

    # -- Rust serde does NOT wrap in an outer key --------------------------

    def test_session_is_flat_object_not_wrapped(self):
        """Rust serializes SessionContext as a flat object, not {\"session\": ...}."""
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        ctx = session()
        assert isinstance(ctx, SessionContext)
        assert ctx.channel_name == "telegram"

    def test_user_identity_is_flat_object_not_wrapped(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        uid = user_identity()
        assert isinstance(uid, UserIdentity)
        assert uid.username == "jdoe"

    def test_agent_config_is_flat_object_not_wrapped(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        cfg = agent_config()
        assert isinstance(cfg, AgentConfig)
        assert cfg.name == "ZeroClaw"

    # -- edge cases ----------------------------------------------------------

    def test_session_with_empty_strings(self):
        _pdk.host_fn.return_value = json.dumps({
            "channel_name": "",
            "conversation_id": "",
            "timestamp": "",
        })
        result = session()
        assert result.channel_name == ""
        assert result.conversation_id == ""
        assert result.timestamp == ""

    def test_user_identity_with_unicode(self):
        _pdk.host_fn.return_value = json.dumps({
            "username": "caf\u00e9",
            "display_name": "\u2615 Coffee Bot",
            "channel_user_id": "U-\u00fc\u00f1\u00ee",
        })
        result = user_identity()
        assert result.username == "caf\u00e9"
        assert result.display_name == "\u2615 Coffee Bot"
        assert result.channel_user_id == "U-\u00fc\u00f1\u00ee"

    def test_agent_config_with_empty_collections(self):
        _pdk.host_fn.return_value = json.dumps({
            "name": "empty",
            "personality_traits": [],
            "identity": {},
        })
        result = agent_config()
        assert result.personality_traits == []
        assert result.identity == {}


class TestHostErrorsRaisedAsPythonExceptions:
    """Errors from host are raised as Python exceptions with descriptive messages.

    Acceptance criterion for US-ZCL-33 (AC-5).
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.side_effect = None

    # -- session -------------------------------------------------------------

    def test_session_error_raises_runtime_error(self):
        _pdk.host_fn.return_value = json.dumps({"error": "context access denied"})
        try:
            session()
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    def test_session_error_message_contains_host_text(self):
        _pdk.host_fn.return_value = json.dumps({"error": "context access denied (paranoid mode)"})
        try:
            session()
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "context access denied (paranoid mode)" in str(exc)

    # -- user_identity -------------------------------------------------------

    def test_user_identity_error_raises_runtime_error(self):
        _pdk.host_fn.return_value = json.dumps({"error": "identity not available"})
        try:
            user_identity()
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    def test_user_identity_error_message_contains_host_text(self):
        _pdk.host_fn.return_value = json.dumps({"error": "user identity denied by policy"})
        try:
            user_identity()
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "user identity denied by policy" in str(exc)

    # -- agent_config --------------------------------------------------------

    def test_agent_config_error_raises_runtime_error(self):
        _pdk.host_fn.return_value = json.dumps({"error": "config unavailable"})
        try:
            agent_config()
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    def test_agent_config_error_message_contains_host_text(self):
        _pdk.host_fn.return_value = json.dumps({"error": "agent config access denied"})
        try:
            agent_config()
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "agent config access denied" in str(exc)

    # -- cross-cutting -------------------------------------------------------

    def test_exception_type_is_runtime_error_not_generic(self):
        for fn, resp in [
            (session, {"error": "err"}),
            (user_identity, {"error": "err"}),
            (agent_config, {"error": "err"}),
        ]:
            _pdk.host_fn.return_value = json.dumps(resp)
            try:
                fn()
                assert False, f"{fn.__name__} did not raise"
            except RuntimeError:
                pass
            except Exception as exc:
                assert False, f"{fn.__name__} raised {type(exc).__name__}, expected RuntimeError"

    def test_descriptive_messages_are_not_empty(self):
        messages = [
            "context access denied (paranoid mode)",
            "capability not enabled in manifest",
            "internal host error: mutex poisoned",
        ]
        for msg in messages:
            _pdk.host_fn.return_value = json.dumps({"error": msg})
            try:
                session()
            except RuntimeError as exc:
                assert str(exc) != "", "Exception message must not be empty"
                assert msg in str(exc), f"Expected '{msg}' in exception, got '{exc}'"

    # -- edge cases: falsy error values should NOT raise ---------------------

    def test_empty_error_string_does_not_raise(self):
        """An empty error string is falsy — treat as success, not an error."""
        payload = {**GOLDEN_SESSION, "error": ""}
        _pdk.host_fn.return_value = json.dumps(payload)
        result = session()
        assert isinstance(result, SessionContext)

    def test_null_error_does_not_raise(self):
        """A null/None error field is falsy — treat as success."""
        payload = {**GOLDEN_SESSION, "error": None}
        _pdk.host_fn.return_value = json.dumps(payload)
        result = session()
        assert isinstance(result, SessionContext)

    # -- edge cases: non-dict responses should NOT trigger error check -------

    def test_non_dict_response_does_not_trigger_error_check(self):
        """If host returns a list (not a dict), error check is skipped."""
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        result = session()
        assert isinstance(result, SessionContext)

    # -- edge cases: host function itself raises ----------------------------

    def test_host_fn_exception_propagates_for_session(self):
        """If pdk.host_fn itself throws, it should propagate as-is."""
        _pdk.host_fn.side_effect = OSError("host communication failed")
        try:
            session()
            assert False, "Expected OSError"
        except OSError as exc:
            assert "host communication failed" in str(exc)

    def test_host_fn_exception_propagates_for_user_identity(self):
        _pdk.host_fn.side_effect = OSError("host communication failed")
        try:
            user_identity()
            assert False, "Expected OSError"
        except OSError as exc:
            assert "host communication failed" in str(exc)

    def test_host_fn_exception_propagates_for_agent_config(self):
        _pdk.host_fn.side_effect = OSError("host communication failed")
        try:
            agent_config()
            assert False, "Expected OSError"
        except OSError as exc:
            assert "host communication failed" in str(exc)

    # -- edge cases: invalid JSON from host ---------------------------------

    def test_invalid_json_raises_decode_error(self):
        """Garbled host response should raise json.JSONDecodeError."""
        _pdk.host_fn.return_value = "not valid json{{"
        _pdk.host_fn.side_effect = None
        try:
            session()
            assert False, "Expected JSONDecodeError"
        except json.JSONDecodeError:
            pass

    # -- edge cases: error with special characters --------------------------

    def test_error_with_unicode_and_newlines(self):
        msg = "host error: caf\u00e9 \u2615\nstack trace line 1\nstack trace line 2"
        _pdk.host_fn.return_value = json.dumps({"error": msg})
        _pdk.host_fn.side_effect = None
        try:
            session()
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "caf\u00e9" in str(exc)


class TestDeserializationIntoDataclasses:
    """Unit tests validate deserialization into dataclasses.

    Acceptance criterion for US-ZCL-33 (AC-6).
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.side_effect = None

    def test_session_context_fields_are_accessible_as_attributes(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        ctx = session()
        assert ctx.channel_name == "telegram"
        assert ctx.conversation_id == "conv-42"
        assert ctx.timestamp == "2026-03-30T12:00:00Z"

    def test_user_identity_fields_are_accessible_as_attributes(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_USER_IDENTITY)
        uid = user_identity()
        assert uid.username == "jdoe"
        assert uid.display_name == "Jane Doe"
        assert uid.channel_user_id == "U12345"

    def test_agent_config_fields_are_accessible_as_attributes(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_AGENT_CONFIG)
        cfg = agent_config()
        assert cfg.name == "ZeroClaw"
        assert cfg.personality_traits == ["friendly", "concise"]
        assert cfg.identity == {"role": "assistant", "team": "engineering"}

    def test_dataclasses_support_equality(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        a = session()
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        b = session()
        assert a == b

    def test_dataclasses_have_repr(self):
        _pdk.host_fn.return_value = json.dumps(GOLDEN_SESSION)
        ctx = session()
        r = repr(ctx)
        assert "SessionContext" in r
        assert "telegram" in r

    def test_extra_json_fields_ignored_gracefully(self):
        """Host may add new fields in the future — SDK must not break."""
        extended = {**GOLDEN_SESSION, "extra_field": "ignored"}
        _pdk.host_fn.return_value = json.dumps(extended)
        result = session()
        assert result.channel_name == "telegram"
        assert not hasattr(result, "extra_field")

    def test_agent_config_many_personality_traits(self):
        data = {
            "name": "verbose",
            "personality_traits": ["friendly", "verbose", "technical", "formal", "patient"],
            "identity": {},
        }
        _pdk.host_fn.return_value = json.dumps(data)
        result = agent_config()
        assert len(result.personality_traits) == 5

    def test_agent_config_many_identity_entries(self):
        data = {
            "name": "rich",
            "personality_traits": [],
            "identity": {"role": "assistant", "team": "eng", "org": "acme", "level": "L5"},
        }
        _pdk.host_fn.return_value = json.dumps(data)
        result = agent_config()
        assert len(result.identity) == 4
        assert result.identity["org"] == "acme"
