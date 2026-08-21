#!/usr/bin/env python3
"""Validate the Android app's provider defaults against the public models.dev catalog.

This intentionally uses no provider credentials. It proves that every hosted default names a
model the provider currently publishes, and that it supports tool calls wherever that provider
offers a tool-capable model. It cannot prove credentials, quotas, or the provider's live API.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path


CATALOG_URL = "https://models.dev/api.json"

# The Android app's provider IDs do not always match models.dev's IDs.
CATALOG_PROVIDER = {
    "deepseek": "deepseek",
    "gemini": "google",
    "groq": "groq",
    "xai": "xai",
    "together": "togetherai",
    "openai": "openai",
    "anthropic": "anthropic",
    "openrouter": "openrouter",
    "mistral": "mistral",
    "cerebras": "cerebras",
    "minimax": "minimax",
    "moonshot": "moonshotai",
    "fireworks": "fireworks-ai",
    "nvidia": "nvidia",
    "perplexity": "perplexity",
    "cohere": "cohere",
    "qwen": "alibaba",
    "siliconflow": "siliconflow",
    "novita": "novita-ai",
}

LOCAL_PROVIDERS = {"ollama", "llamacpp", "lmstudio", "vllm"}
EXPECTED_DEFAULTS = set(CATALOG_PROVIDER) | LOCAL_PROVIDERS


def parse_defaults(config_store: Path) -> dict[str, str]:
    text = config_store.read_text()
    try:
        body = text.split("val DEFAULT_MODEL = mapOf(", 1)[1].split("\n    )", 1)[0]
    except IndexError as exc:
        raise ValueError("Could not find ProviderCatalog.DEFAULT_MODEL") from exc
    return dict(re.findall(r'"([^"]+)"\s+to\s+"([^"]+)"', body))


def load_catalog(path: Path | None) -> dict:
    if path:
        return json.loads(path.read_text())
    request = urllib.request.Request(
        CATALOG_URL, headers={"User-Agent": "zeroclaw-android-model-check"}
    )
    for attempt in range(3):
        try:
            with urllib.request.urlopen(request, timeout=60) as response:  # noqa: S310
                return json.load(response)
        except (TimeoutError, urllib.error.URLError):
            if attempt == 2:
                raise
            time.sleep(2 ** attempt)
    raise AssertionError("unreachable")


def validate_default_shape(defaults: dict[str, str]) -> list[str]:
    errors: list[str] = []
    missing = EXPECTED_DEFAULTS - set(defaults)
    unexpected = set(defaults) - EXPECTED_DEFAULTS
    if missing:
        errors.append(f"missing provider defaults: {', '.join(sorted(missing))}")
    if unexpected:
        errors.append(f"unexpected provider defaults: {', '.join(sorted(unexpected))}")
    blank = sorted(provider for provider, model in defaults.items() if not model.strip())
    if blank:
        errors.append(f"blank provider defaults: {', '.join(blank)}")
    return errors


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--catalog", type=Path, help="Use a downloaded models.dev api.json")
    parser.add_argument(
        "--config-store",
        type=Path,
        default=Path(__file__).parents[1]
        / "app/src/main/kotlin/org/zerodroid/bridge/ConfigStore.kt",
    )
    args = parser.parse_args()

    defaults = parse_defaults(args.config_store)
    catalog = load_catalog(args.catalog)
    errors = validate_default_shape(defaults)

    for provider, model in defaults.items():
        if provider in LOCAL_PROVIDERS:
            print(f"SKIP local  {provider:12} {model}")
            continue

        catalog_id = CATALOG_PROVIDER.get(provider)
        if not catalog_id or catalog_id not in catalog:
            errors.append(f"{provider}: no models.dev provider mapping ({catalog_id!r})")
            continue

        models = catalog[catalog_id].get("models", {})
        spec = models.get(model)
        if spec is None:
            errors.append(f"{provider}: default {model!r} is not in models.dev/{catalog_id}")
            continue

        native_tools = bool(spec.get("tool_call"))
        provider_offers_native_tools = any(
            bool(candidate.get("tool_call")) for candidate in models.values()
        )
        if not native_tools and provider_offers_native_tools:
            errors.append(f"{provider}: default {model!r} does not support native tool calls")
            continue

        suffix = "native tools" if native_tools else "provider publishes no native-tool model"
        print(f"OK          {provider:12} {model:55} {suffix}")

    if errors:
        print("\nINVALID DEFAULTS:", file=sys.stderr)
        for error in errors:
            print(f"  - {error}", file=sys.stderr)
        return 1

    print(f"\nValidated {len(defaults) - len(LOCAL_PROVIDERS)} hosted defaults; "
          f"skipped {len(LOCAL_PROVIDERS)} local providers.")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
