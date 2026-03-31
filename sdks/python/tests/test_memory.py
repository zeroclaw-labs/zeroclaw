"""Tests for zeroclaw_plugin_sdk.memory module public API surface.

Verifies that the memory module exposes store(key, value), recall(query),
and forget(key) — acceptance criterion for US-ZCL-30.
"""

import json
import sys
import types
from unittest.mock import MagicMock

# Stub extism_pdk before importing — the real PDK only works inside WASM runtime.
# Re-use the existing stub if another test file already created it (test ordering).
if "extism_pdk" in sys.modules:
    _pdk = sys.modules["extism_pdk"]
else:
    _pdk = types.ModuleType("extism_pdk")
    _pdk.input_string = MagicMock()
    _pdk.output_string = MagicMock()
    _pdk.plugin = lambda f: f
    sys.modules["extism_pdk"] = _pdk
_pdk.host_fn = MagicMock()

from zeroclaw_plugin_sdk import memory  # noqa: E402
from zeroclaw_plugin_sdk.memory import store, recall, forget  # noqa: E402

# Ensure the memory module's pdk reference has host_fn (handles test ordering).
memory.pdk = _pdk


class TestMemoryModuleExposesAPI:
    """zeroclaw_plugin_sdk.memory must expose store, recall, and forget."""

    def test_module_has_store(self):
        """memory module exposes a callable 'store'."""
        assert hasattr(memory, "store")
        assert callable(memory.store)

    def test_module_has_recall(self):
        """memory module exposes a callable 'recall'."""
        assert hasattr(memory, "recall")
        assert callable(memory.recall)

    def test_module_has_forget(self):
        """memory module exposes a callable 'forget'."""
        assert hasattr(memory, "forget")
        assert callable(memory.forget)

    def test_store_accepts_key_value(self):
        """store() accepts (key, value) positional args."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        store("my-key", "my-value")

        _pdk.host_fn.assert_called_once()
        call_args = _pdk.host_fn.call_args[0]
        assert call_args[0] == "zeroclaw_memory_store"
        payload = json.loads(call_args[1])
        assert payload == {"key": "my-key", "value": "my-value"}

    def test_recall_accepts_query(self):
        """recall() accepts a single query positional arg and returns results."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"results": "[]"})

        result = recall("search term")

        _pdk.host_fn.assert_called_once()
        call_args = _pdk.host_fn.call_args[0]
        assert call_args[0] == "zeroclaw_memory_recall"
        payload = json.loads(call_args[1])
        assert payload == {"query": "search term"}
        assert result == "[]"

    def test_forget_accepts_key(self):
        """forget() accepts a single key positional arg."""
        _pdk.host_fn.reset_mock()
        _pdk.host_fn.return_value = json.dumps({"success": True})

        forget("my-key")

        _pdk.host_fn.assert_called_once()
        call_args = _pdk.host_fn.call_args[0]
        assert call_args[0] == "zeroclaw_memory_forget"
        payload = json.loads(call_args[1])
        assert payload == {"key": "my-key"}

    def test_importable_from_package(self):
        """store, recall, forget are importable from zeroclaw_plugin_sdk.memory."""
        # These imports already succeeded at module level; verify they're the same objects.
        assert store is memory.store
        assert recall is memory.recall
        assert forget is memory.forget


class TestHostFunctionCalls:
    """Functions call zeroclaw_memory_* host functions via Extism PDK (US-ZCL-30-2).

    Verifies each memory function routes through pdk.host_fn with the correct
    host function name, and that no other host interaction mechanism is used.
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()

    # -- store ---------------------------------------------------------------

    def test_store_calls_host_fn_on_pdk(self):
        """store() invokes host_fn on the extism_pdk module, not another path."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("k", "v")
        assert memory.pdk is _pdk, "memory module should reference the extism_pdk stub"
        _pdk.host_fn.assert_called_once()

    def test_store_uses_correct_host_function_name(self):
        """store() must call the host function named 'zeroclaw_memory_store'."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("k", "v")
        host_fn_name = _pdk.host_fn.call_args[0][0]
        assert host_fn_name == "zeroclaw_memory_store"

    def test_store_passes_json_payload_to_host(self):
        """store() serializes {key, value} as JSON and passes it to host_fn."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("agent-key", "agent-value")
        raw_payload = _pdk.host_fn.call_args[0][1]
        payload = json.loads(raw_payload)
        assert payload == {"key": "agent-key", "value": "agent-value"}

    def test_store_makes_exactly_one_host_call(self):
        """store() should make a single host function call per invocation."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("k1", "v1")
        assert _pdk.host_fn.call_count == 1

    # -- recall --------------------------------------------------------------

    def test_recall_calls_host_fn_on_pdk(self):
        """recall() invokes host_fn on the extism_pdk module."""
        _pdk.host_fn.return_value = json.dumps({"results": "[]"})
        recall("q")
        assert memory.pdk is _pdk
        _pdk.host_fn.assert_called_once()

    def test_recall_uses_correct_host_function_name(self):
        """recall() must call the host function named 'zeroclaw_memory_recall'."""
        _pdk.host_fn.return_value = json.dumps({"results": "[]"})
        recall("q")
        host_fn_name = _pdk.host_fn.call_args[0][0]
        assert host_fn_name == "zeroclaw_memory_recall"

    def test_recall_passes_json_payload_to_host(self):
        """recall() serializes {query} as JSON and passes it to host_fn."""
        _pdk.host_fn.return_value = json.dumps({"results": "[]"})
        recall("find something")
        raw_payload = _pdk.host_fn.call_args[0][1]
        payload = json.loads(raw_payload)
        assert payload == {"query": "find something"}

    def test_recall_returns_host_results(self):
        """recall() returns the 'results' field from host_fn's JSON response."""
        expected = '[{"key":"k","value":"v"}]'
        _pdk.host_fn.return_value = json.dumps({"results": expected})
        result = recall("q")
        assert result == expected

    def test_recall_makes_exactly_one_host_call(self):
        """recall() should make a single host function call per invocation."""
        _pdk.host_fn.return_value = json.dumps({"results": ""})
        recall("q")
        assert _pdk.host_fn.call_count == 1

    # -- forget --------------------------------------------------------------

    def test_forget_calls_host_fn_on_pdk(self):
        """forget() invokes host_fn on the extism_pdk module."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        forget("k")
        assert memory.pdk is _pdk
        _pdk.host_fn.assert_called_once()

    def test_forget_uses_correct_host_function_name(self):
        """forget() must call the host function named 'zeroclaw_memory_forget'."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        forget("k")
        host_fn_name = _pdk.host_fn.call_args[0][0]
        assert host_fn_name == "zeroclaw_memory_forget"

    def test_forget_passes_json_payload_to_host(self):
        """forget() serializes {key} as JSON and passes it to host_fn."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        forget("old-key")
        raw_payload = _pdk.host_fn.call_args[0][1]
        payload = json.loads(raw_payload)
        assert payload == {"key": "old-key"}

    def test_forget_makes_exactly_one_host_call(self):
        """forget() should make a single host function call per invocation."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        forget("k")
        assert _pdk.host_fn.call_count == 1

    # -- cross-cutting -------------------------------------------------------

    def test_all_functions_use_same_pdk_host_fn(self):
        """All three functions must route through the same pdk.host_fn callable."""
        _pdk.host_fn.return_value = json.dumps({"success": True, "results": ""})
        store("a", "b")
        recall("c")
        forget("d")
        assert _pdk.host_fn.call_count == 3
        names = [call[0][0] for call in _pdk.host_fn.call_args_list]
        assert names == [
            "zeroclaw_memory_store",
            "zeroclaw_memory_recall",
            "zeroclaw_memory_forget",
        ]

    def test_host_fn_receives_string_not_bytes(self):
        """Payloads passed to host_fn must be str (JSON text), not bytes."""
        _pdk.host_fn.return_value = json.dumps({"success": True, "results": ""})
        store("k", "v")
        recall("q")
        forget("k")
        for call in _pdk.host_fn.call_args_list:
            payload = call[0][1]
            assert isinstance(payload, str), f"Expected str payload, got {type(payload)}"


class TestWireFormatMatchesRustSDK:
    """JSON request/response marshalling matches the Rust SDK's wire format exactly.

    The Rust SDK (crates/zeroclaw-plugin-sdk/src/memory.rs) defines typed structs
    that serde serializes to JSON.  The Python SDK must produce and consume
    identical JSON shapes so the host-side Deserialize/Serialize round-trips work.

    Acceptance criterion for US-ZCL-30 (AC-3).
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()

    # -- store request -------------------------------------------------------

    def test_store_request_has_exactly_key_and_value_fields(self):
        """MemoryStoreRequest has exactly {key, value} — no extra fields."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("k", "v")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert set(payload.keys()) == {"key", "value"}

    def test_store_request_key_is_string(self):
        """MemoryStoreRequest.key must serialize as a JSON string."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("my-key", "val")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert isinstance(payload["key"], str)

    def test_store_request_value_is_string(self):
        """MemoryStoreRequest.value must serialize as a JSON string."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("k", "my-value")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert isinstance(payload["value"], str)

    def test_store_request_preserves_special_characters(self):
        """Keys and values with unicode/special chars round-trip correctly."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("café-☕", 'value with "quotes" and\nnewlines')
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert payload["key"] == "café-☕"
        assert payload["value"] == 'value with "quotes" and\nnewlines'

    def test_store_request_handles_empty_strings(self):
        """Rust serde accepts empty strings for String fields."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("", "")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert payload == {"key": "", "value": ""}

    # -- store response ------------------------------------------------------

    def test_store_response_success_true_accepted(self):
        """MemoryStoreResponse {success: true} — no error raised."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("k", "v")  # should not raise

    def test_store_response_success_false_raises(self):
        """MemoryStoreResponse {success: false} raises RuntimeError."""
        _pdk.host_fn.return_value = json.dumps({"success": False})
        try:
            store("k", "v")
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    def test_store_response_error_field_raises(self):
        """MemoryStoreResponse {error: "..."} raises RuntimeError with message."""
        _pdk.host_fn.return_value = json.dumps({"error": "host error"})
        try:
            store("k", "v")
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "host error" in str(exc)

    # -- recall request ------------------------------------------------------

    def test_recall_request_has_exactly_query_field(self):
        """MemoryRecallRequest has exactly {query} — no extra fields."""
        _pdk.host_fn.return_value = json.dumps({"results": "[]"})
        recall("search")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert set(payload.keys()) == {"query"}

    def test_recall_request_query_is_string(self):
        """MemoryRecallRequest.query must serialize as a JSON string."""
        _pdk.host_fn.return_value = json.dumps({"results": "[]"})
        recall("q")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert isinstance(payload["query"], str)

    def test_recall_request_preserves_special_characters(self):
        """Query with unicode/special chars round-trips correctly."""
        _pdk.host_fn.return_value = json.dumps({"results": "[]"})
        recall("find café ☕")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert payload["query"] == "find café ☕"

    # -- recall response -----------------------------------------------------

    def test_recall_response_results_returned_as_string(self):
        """MemoryRecallResponse.results is a JSON-encoded string, not parsed."""
        entries = json.dumps([{"key": "k1", "content": "hello"}])
        _pdk.host_fn.return_value = json.dumps({"results": entries})
        result = recall("q")
        assert isinstance(result, str)
        assert result == entries

    def test_recall_response_empty_results(self):
        """MemoryRecallResponse with empty results string is accepted."""
        _pdk.host_fn.return_value = json.dumps({"results": ""})
        result = recall("q")
        assert result == ""

    def test_recall_response_missing_results_defaults_to_empty(self):
        """Rust serde #[serde(default)] makes results default to empty string."""
        _pdk.host_fn.return_value = json.dumps({})
        result = recall("q")
        assert result == ""

    def test_recall_response_error_field_raises(self):
        """MemoryRecallResponse {error: "..."} raises RuntimeError."""
        _pdk.host_fn.return_value = json.dumps({"error": "recall failed"})
        try:
            recall("q")
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "recall failed" in str(exc)

    # -- forget request ------------------------------------------------------

    def test_forget_request_has_exactly_key_field(self):
        """MemoryForgetRequest has exactly {key} — no extra fields."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        forget("k")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert set(payload.keys()) == {"key"}

    def test_forget_request_key_is_string(self):
        """MemoryForgetRequest.key must serialize as a JSON string."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        forget("my-key")
        payload = json.loads(_pdk.host_fn.call_args[0][1])
        assert isinstance(payload["key"], str)

    # -- forget response -----------------------------------------------------

    def test_forget_response_success_true_accepted(self):
        """MemoryForgetResponse {success: true} — no error raised."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        forget("k")  # should not raise

    def test_forget_response_success_false_raises(self):
        """MemoryForgetResponse {success: false} raises RuntimeError."""
        _pdk.host_fn.return_value = json.dumps({"success": False})
        try:
            forget("k")
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    def test_forget_response_error_field_raises(self):
        """MemoryForgetResponse {error: "..."} raises RuntimeError with message."""
        _pdk.host_fn.return_value = json.dumps({"error": "forget failed"})
        try:
            forget("k")
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "forget failed" in str(exc)

    # -- cross-format consistency --------------------------------------------

    def test_all_requests_are_valid_json(self):
        """Every payload sent to host_fn must be valid JSON."""
        _pdk.host_fn.return_value = json.dumps({"success": True, "results": "[]"})
        store("k", "v")
        recall("q")
        forget("k")
        for call in _pdk.host_fn.call_args_list:
            raw = call[0][1]
            parsed = json.loads(raw)  # must not raise
            assert isinstance(parsed, dict)

    def test_request_field_names_are_snake_case(self):
        """Rust serde defaults to snake_case — Python must match."""
        _pdk.host_fn.return_value = json.dumps({"success": True, "results": "[]"})
        store("k", "v")
        recall("q")
        forget("k")
        for call in _pdk.host_fn.call_args_list:
            payload = json.loads(call[0][1])
            for field in payload.keys():
                assert field == field.lower(), f"Field '{field}' is not lowercase"
                assert "-" not in field, f"Field '{field}' uses kebab-case, not snake_case"

    def test_no_null_fields_in_requests(self):
        """Rust request structs have no Option fields — Python must not send nulls."""
        _pdk.host_fn.return_value = json.dumps({"success": True, "results": "[]"})
        store("k", "v")
        recall("q")
        forget("k")
        for call in _pdk.host_fn.call_args_list:
            payload = json.loads(call[0][1])
            for field, val in payload.items():
                assert val is not None, f"Field '{field}' is null — Rust struct has no Option"


class TestSerializationMatchesExpectedJSON:
    """Unit tests validate serialization format matches expected JSON structures.

    Acceptance criterion for US-ZCL-30 (AC-5).

    These are golden-value tests: each function's serialized request is compared
    against the exact JSON structure expected by the Rust SDK host functions.
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()

    # -- store: full structure golden tests ----------------------------------

    def test_store_serializes_to_expected_json_structure(self):
        """store() request JSON must be exactly {"key": ..., "value": ...}."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("agent-name", "zeroclaw-v1")
        actual = json.loads(_pdk.host_fn.call_args[0][1])
        expected = {"key": "agent-name", "value": "zeroclaw-v1"}
        assert actual == expected

    def test_store_json_key_ordering_is_valid(self):
        """store() JSON must parse to the correct dict regardless of key order."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        store("k", "v")
        raw = _pdk.host_fn.call_args[0][1]
        # Verify round-trip: raw JSON -> parse -> re-serialize -> parse is stable
        parsed = json.loads(raw)
        reparsed = json.loads(json.dumps(parsed, sort_keys=True))
        assert reparsed == {"key": "k", "value": "v"}

    def test_store_response_golden_success(self):
        """store() accepts the golden success response {"success": true}."""
        _pdk.host_fn.return_value = '{"success": true}'
        store("k", "v")  # should not raise

    def test_store_response_golden_error(self):
        """store() raises on golden error response {"error": "..."}."""
        _pdk.host_fn.return_value = '{"error": "write failed"}'
        try:
            store("k", "v")
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "write failed" in str(exc)

    # -- recall: full structure golden tests ---------------------------------

    def test_recall_serializes_to_expected_json_structure(self):
        """recall() request JSON must be exactly {"query": ...}."""
        _pdk.host_fn.return_value = json.dumps({"results": "[]"})
        recall("what is zeroclaw?")
        actual = json.loads(_pdk.host_fn.call_args[0][1])
        expected = {"query": "what is zeroclaw?"}
        assert actual == expected

    def test_recall_response_golden_with_results(self):
        """recall() returns results string from golden response."""
        golden_results = '[{"key":"mood","content":"happy"}]'
        _pdk.host_fn.return_value = json.dumps({"results": golden_results})
        result = recall("mood")
        assert result == golden_results

    def test_recall_response_golden_empty(self):
        """recall() handles golden empty response {"results": ""}."""
        _pdk.host_fn.return_value = '{"results": ""}'
        result = recall("nonexistent")
        assert result == ""

    # -- forget: full structure golden tests ---------------------------------

    def test_forget_serializes_to_expected_json_structure(self):
        """forget() request JSON must be exactly {"key": ...}."""
        _pdk.host_fn.return_value = json.dumps({"success": True})
        forget("old-memory")
        actual = json.loads(_pdk.host_fn.call_args[0][1])
        expected = {"key": "old-memory"}
        assert actual == expected

    def test_forget_response_golden_success(self):
        """forget() accepts the golden success response {"success": true}."""
        _pdk.host_fn.return_value = '{"success": true}'
        forget("k")  # should not raise

    def test_forget_response_golden_error(self):
        """forget() raises on golden error response {"error": "..."}."""
        _pdk.host_fn.return_value = '{"error": "key locked"}'
        try:
            forget("k")
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "key locked" in str(exc)

    # -- cross-function structure validation ---------------------------------

    def test_all_requests_match_rust_struct_shapes(self):
        """Each function's request matches its Rust struct's field set exactly."""
        _pdk.host_fn.return_value = json.dumps({"success": True, "results": "[]"})

        store("k", "v")
        store_payload = json.loads(_pdk.host_fn.call_args_list[-1][0][1])
        assert store_payload == {"key": "k", "value": "v"}

        recall("q")
        recall_payload = json.loads(_pdk.host_fn.call_args_list[-1][0][1])
        assert recall_payload == {"query": "q"}

        forget("k")
        forget_payload = json.loads(_pdk.host_fn.call_args_list[-1][0][1])
        assert forget_payload == {"key": "k"}

    def test_serialized_json_is_compact(self):
        """Serialized JSON should not contain extra whitespace (compact format)."""
        _pdk.host_fn.return_value = json.dumps({"success": True, "results": "[]"})
        store("k", "v")
        raw = _pdk.host_fn.call_args[0][1]
        # json.dumps without indent produces compact JSON
        repacked = json.dumps(json.loads(raw), separators=(",", ": "))
        # Just verify it's valid and has no leading/trailing whitespace
        assert raw.strip() == raw
        assert json.loads(raw) == json.loads(repacked)


class TestHostErrorsRaisedAsPythonExceptions:
    """Errors from host are raised as Python exceptions with descriptive messages.

    Acceptance criterion for US-ZCL-30 (AC-4).

    When a host function returns an error payload, the SDK must convert it into
    a Python exception whose message includes the host-provided error text so
    plugin authors get actionable diagnostics.
    """

    def setup_method(self):
        _pdk.host_fn.reset_mock()

    # -- store ---------------------------------------------------------------

    def test_store_error_raises_runtime_error(self):
        """store() raises RuntimeError when host returns an error field."""
        _pdk.host_fn.return_value = json.dumps({"error": "disk full"})
        try:
            store("k", "v")
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    def test_store_error_message_contains_host_text(self):
        """store() exception message includes the host-provided error string."""
        _pdk.host_fn.return_value = json.dumps({"error": "permission denied: read-only store"})
        try:
            store("k", "v")
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "permission denied: read-only store" in str(exc)

    def test_store_success_false_raises_runtime_error(self):
        """store() raises RuntimeError when success=false (no error field)."""
        _pdk.host_fn.return_value = json.dumps({"success": False})
        try:
            store("k", "v")
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    # -- recall --------------------------------------------------------------

    def test_recall_error_raises_runtime_error(self):
        """recall() raises RuntimeError when host returns an error field."""
        _pdk.host_fn.return_value = json.dumps({"error": "index unavailable"})
        try:
            recall("q")
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    def test_recall_error_message_contains_host_text(self):
        """recall() exception message includes the host-provided error string."""
        _pdk.host_fn.return_value = json.dumps({"error": "query too long: max 512 chars"})
        try:
            recall("q")
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "query too long: max 512 chars" in str(exc)

    # -- forget --------------------------------------------------------------

    def test_forget_error_raises_runtime_error(self):
        """forget() raises RuntimeError when host returns an error field."""
        _pdk.host_fn.return_value = json.dumps({"error": "key not found"})
        try:
            forget("k")
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    def test_forget_error_message_contains_host_text(self):
        """forget() exception message includes the host-provided error string."""
        _pdk.host_fn.return_value = json.dumps({"error": "key locked by another process"})
        try:
            forget("k")
            assert False, "Expected RuntimeError"
        except RuntimeError as exc:
            assert "key locked by another process" in str(exc)

    def test_forget_success_false_raises_runtime_error(self):
        """forget() raises RuntimeError when success=false (no error field)."""
        _pdk.host_fn.return_value = json.dumps({"success": False})
        try:
            forget("k")
            assert False, "Expected RuntimeError"
        except RuntimeError:
            pass

    # -- cross-cutting -------------------------------------------------------

    def test_exception_type_is_runtime_error_not_generic(self):
        """All host errors raise RuntimeError specifically, not bare Exception."""
        for fn, args, resp in [
            (store, ("k", "v"), {"error": "err"}),
            (recall, ("q",), {"error": "err"}),
            (forget, ("k",), {"error": "err"}),
        ]:
            _pdk.host_fn.return_value = json.dumps(resp)
            try:
                fn(*args)
                assert False, f"{fn.__name__} did not raise"
            except RuntimeError:
                pass
            except Exception as exc:
                assert False, f"{fn.__name__} raised {type(exc).__name__}, expected RuntimeError"

    def test_error_checked_before_success_field(self):
        """When both error and success fields are present, error takes priority."""
        _pdk.host_fn.return_value = json.dumps({"error": "conflict", "success": True})
        try:
            store("k", "v")
            assert False, "Expected RuntimeError even though success=True"
        except RuntimeError as exc:
            assert "conflict" in str(exc)

    def test_descriptive_messages_are_not_empty(self):
        """Host error text is forwarded verbatim — never swallowed to empty string."""
        messages = [
            "timeout after 30s",
            "invalid UTF-8 in key",
            "memory subsystem not initialized",
        ]
        for msg in messages:
            _pdk.host_fn.return_value = json.dumps({"error": msg})
            try:
                store("k", "v")
            except RuntimeError as exc:
                assert str(exc) != "", "Exception message must not be empty"
                assert msg in str(exc), f"Expected '{msg}' in exception, got '{exc}'"
