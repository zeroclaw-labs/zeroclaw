"""Tests for @plugin_fn decorator JSON marshalling."""

import json
import sys
import types
from unittest.mock import MagicMock

# Stub extism_pdk before importing the decorator —
# the real PDK is only available inside the Extism WASM runtime.
_pdk = types.ModuleType("extism_pdk")
_pdk.input_string = MagicMock()
_pdk.output_string = MagicMock()
_pdk.plugin = lambda f: f  # passthrough — the outer layer is tested separately
sys.modules["extism_pdk"] = _pdk

from zeroclaw_plugin_sdk.decorator import plugin_fn  # noqa: E402


class TestPluginFnJsonMarshalling:
    """@plugin_fn should deserialise JSON input and serialise JSON output."""

    def test_dict_round_trip(self):
        """Dict input is parsed and dict output is serialised."""
        @plugin_fn
        def greet(data):
            return {"message": f"Hello, {data['name']}!"}

        _pdk.input_string.return_value = json.dumps({"name": "Alice"})
        _pdk.output_string.reset_mock()

        result = greet()

        assert result == 0
        written = _pdk.output_string.call_args[0][0]
        assert json.loads(written) == {"message": "Hello, Alice!"}

    def test_list_round_trip(self):
        """List input/output survives marshalling."""
        @plugin_fn
        def double_items(data):
            return [x * 2 for x in data]

        _pdk.input_string.return_value = json.dumps([1, 2, 3])
        _pdk.output_string.reset_mock()

        result = double_items()

        assert result == 0
        written = _pdk.output_string.call_args[0][0]
        assert json.loads(written) == [2, 4, 6]

    def test_string_value_round_trip(self):
        """A plain JSON string is properly deserialised."""
        @plugin_fn
        def echo(data):
            return {"echoed": data}

        _pdk.input_string.return_value = json.dumps("hello")
        _pdk.output_string.reset_mock()

        result = echo()

        assert result == 0
        written = _pdk.output_string.call_args[0][0]
        assert json.loads(written) == {"echoed": "hello"}

    def test_empty_input(self):
        """Empty input string is passed as None."""
        @plugin_fn
        def handle(data):
            return {"received_none": data is None}

        _pdk.input_string.return_value = ""
        _pdk.output_string.reset_mock()

        result = handle()

        assert result == 0
        written = _pdk.output_string.call_args[0][0]
        assert json.loads(written) == {"received_none": True}

    def test_numeric_output(self):
        """Numeric return values are valid JSON."""
        @plugin_fn
        def compute(data):
            return data["a"] + data["b"]

        _pdk.input_string.return_value = json.dumps({"a": 3, "b": 7})
        _pdk.output_string.reset_mock()

        result = compute()

        assert result == 0
        written = _pdk.output_string.call_args[0][0]
        assert json.loads(written) == 10

    def test_nested_json(self):
        """Deeply nested structures survive marshalling."""
        payload = {"level1": {"level2": {"level3": [1, 2, {"deep": True}]}}}

        @plugin_fn
        def passthrough(data):
            return data

        _pdk.input_string.return_value = json.dumps(payload)
        _pdk.output_string.reset_mock()

        result = passthrough()

        assert result == 0
        written = _pdk.output_string.call_args[0][0]
        assert json.loads(written) == payload
