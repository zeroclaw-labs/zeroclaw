from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path


SCRIPT = Path(__file__).parents[1] / "check-model-defaults.py"
SPEC = importlib.util.spec_from_file_location("check_model_defaults", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class DefaultShapeTest(unittest.TestCase):
    def test_catalog_body_limit_rejects_oversized_success(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "response exceeds 2 bytes"):
            MODULE.decode_catalog_body(b"{}\n", "catalog.test", max_bytes=2)

    def test_zerorouter_catalog_is_indexed_by_model_id(self) -> None:
        models = MODULE.zerorouter_models(
            {
                "data": [
                    {"id": "google/gemini-3.7-flash", "tool_call": True},
                    {"object": "model"},
                ]
            }
        )

        self.assertEqual(list(models), ["google/gemini-3.7-flash"])
        self.assertTrue(models["google/gemini-3.7-flash"]["tool_call"])

    def test_empty_map_is_rejected(self) -> None:
        errors = MODULE.validate_default_shape({})

        self.assertTrue(any("missing provider defaults" in error for error in errors))

    def test_exact_nonblank_provider_set_is_accepted(self) -> None:
        defaults = {provider: "model" for provider in MODULE.EXPECTED_DEFAULTS}

        self.assertEqual(MODULE.validate_default_shape(defaults), [])

    def test_unexpected_and_blank_defaults_are_rejected(self) -> None:
        defaults = {provider: "model" for provider in MODULE.EXPECTED_DEFAULTS}
        defaults["gemini"] = ""
        defaults["surprise"] = "model"

        errors = MODULE.validate_default_shape(defaults)
        self.assertTrue(any("blank provider defaults: gemini" in error for error in errors))
        self.assertTrue(any("unexpected provider defaults: surprise" in error for error in errors))


if __name__ == "__main__":
    unittest.main()
