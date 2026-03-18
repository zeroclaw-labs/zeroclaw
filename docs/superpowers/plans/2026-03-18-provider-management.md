# Provider Management Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Extend the ZeroClaw bot's provider management capabilities with model testing, model listing, fallback removal, comprehensive TOOLS.md workflows, and 14 E2E bot-level tests.

**Architecture:** Three new Python scripts (`provider_test.py`, `provider_models.py`) + one modified script (`provider_apply.py`) in the existing `provider-manager` skill. New `CHAT_ENDPOINTS` and `KNOWN_MODELS` dicts in `providers.py`. SKILL.toml updated with 2 new tool definitions. TOOLS.md expanded with all workflows + multi-turn examples. E2E tests in Rust via `tokio-tungstenite` WebSocket to gateway.

**Tech Stack:** Python 3 (urllib, json), Rust (tokio, tokio-tungstenite, serde_json), ZeroClaw gateway WebSocket protocol.

**Spec:** `docs/superpowers/specs/2026-03-18-provider-management-design.md`

---

## File Structure

| File | Responsibility |
|------|---------------|
| `~/.zeroclaw/workspace/skills/provider-manager/scripts/providers.py` | **Modify:** Add `CHAT_ENDPOINTS` dict (per-provider chat completion config) and `KNOWN_MODELS` dict (static model lists for providers without list API) |
| `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_test.py` | **Create:** Test a model via chat completion request. Validates key + model availability. 20s timeout. |
| `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_models.py` | **Create:** List available models for a provider. Live API or static fallback. |
| `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_apply.py` | **Modify:** Add `remove_fallback` action — remove profile from fallback chain. |
| `~/.zeroclaw/workspace/skills/provider-manager/SKILL.toml` | **Modify:** Add `provider_test` and `provider_models` tool defs. Update `provider_apply` action enum. |
| `~/.zeroclaw/workspace/TOOLS.md` | **Modify:** Add new workflows, multi-turn examples, updated model table. |
| `tests/provider_management_e2e.rs` | **Create:** 14 E2E test scenarios via WebSocket. |

---

## Task 1: Add CHAT_ENDPOINTS and KNOWN_MODELS to providers.py

**Files:**
- Modify: `~/.zeroclaw/workspace/skills/provider-manager/scripts/providers.py:148+`

- [ ] **Step 1: Add CHAT_ENDPOINTS dict after PROVIDERS dict**

Add after line 148 (after `PROVIDERS` dict closing brace):

```python
# Chat completion endpoints — used by provider_test.py
# Different from quota_* fields which are health-check endpoints.
CHAT_ENDPOINTS: dict[str, dict] = {
    "google": {
        "url": "https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent?key={key}",
        "method": "POST",
        "headers": {"Content-Type": "application/json"},
        "body_template": '{{"contents":[{{"parts":[{{"text":"{prompt}"}}]}}]}}',
        "response_path": "candidates.0.content.parts.0.text",
    },
    "anthropic": {
        "url": "https://api.anthropic.com/v1/messages",
        "method": "POST",
        "headers": {
            "x-api-key": "{key}",
            "anthropic-version": "2023-06-01",
            "Content-Type": "application/json",
        },
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "content.0.text",
    },
    # OpenAI-compatible providers share one template
    "openai": {
        "url": "https://api.openai.com/v1/chat/completions",
        "method": "POST",
        "headers": {
            "Authorization": "Bearer {key}",
            "Content-Type": "application/json",
        },
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "deepseek": {
        "url": "https://api.deepseek.com/v1/chat/completions",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "moonshot": {
        "url": "https://api.moonshot.cn/v1/chat/completions",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "minimax": {
        "url": "https://api.minimax.chat/v1/text/chatcompletion_v2",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "groq": {
        "url": "https://api.groq.com/openai/v1/chat/completions",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "together": {
        "url": "https://api.together.xyz/v1/chat/completions",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "mistral": {
        "url": "https://api.mistral.ai/v1/chat/completions",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "fireworks": {
        "url": "https://api.fireworks.ai/inference/v1/chat/completions",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "openrouter": {
        "url": "https://openrouter.ai/api/v1/chat/completions",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
    "perplexity": {
        "url": "https://api.perplexity.ai/chat/completions",
        "method": "POST",
        "headers": {"Authorization": "Bearer {key}", "Content-Type": "application/json"},
        "body_template": '{{"model":"{model}","max_tokens":50,"messages":[{{"role":"user","content":"{prompt}"}}]}}',
        "response_path": "choices.0.message.content",
    },
}


KNOWN_MODELS: dict[str, list[str]] = {
    "anthropic": [
        "claude-opus-4-6", "claude-sonnet-4-6",
        "claude-haiku-4-5-20251001", "claude-sonnet-4-20250514",
    ],
    "perplexity": ["sonar", "sonar-pro", "sonar-reasoning"],
}
```

- [ ] **Step 2: Add helper functions for chat endpoints**

Add after the `mask()` function (after line 216):

```python
def get_chat_config(name: str) -> dict | None:
    """Get chat completion config for a provider. Returns None if unknown."""
    canonical = normalize(name)
    return CHAT_ENDPOINTS.get(canonical)


def get_known_models(name: str) -> list[str] | None:
    """Get static known models for a provider. Returns None if not in KNOWN_MODELS."""
    canonical = normalize(name)
    return KNOWN_MODELS.get(canonical)
```

- [ ] **Step 3: Verify no syntax errors**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager/scripts && python3 -c "import providers; print(len(providers.CHAT_ENDPOINTS), 'chat endpoints,', len(providers.KNOWN_MODELS), 'known model lists')"
```
Expected: `12 chat endpoints, 2 known model lists`

- [ ] **Step 4: Commit**

```bash
cd ~/.zeroclaw/workspace/skills/provider-manager && git add scripts/providers.py && git commit -m "feat(provider-manager): add CHAT_ENDPOINTS and KNOWN_MODELS to providers.py"
```

---

## Task 2: Create provider_test.py

**Files:**
- Create: `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_test.py`

- [ ] **Step 1: Write provider_test.py**

```python
"""provider_test — test a model via chat completion, validate key (JSON output)."""

from __future__ import annotations

import argparse
import json
import sys
import os
import time
import urllib.request
import urllib.error

sys.path.insert(0, os.path.dirname(__file__))

from providers import normalize, get_chat_config, get_default_model, mask
from output import success, error, emit
from gateway import get_config_toml
from config_model import ConfigModel


def _resolve_path(obj: dict | list, path: str):
    """Navigate a nested dict/list by dot-separated path like 'choices.0.message.content'."""
    for part in path.split("."):
        if obj is None:
            return None
        if isinstance(obj, list):
            try:
                obj = obj[int(part)]
            except (IndexError, ValueError):
                return None
        elif isinstance(obj, dict):
            obj = obj.get(part)
        else:
            return None
    return obj


def test_model(provider: str, model: str, key: str, prompt: str) -> dict:
    """Send a chat completion request and return result dict."""
    cfg = get_chat_config(provider)
    if not cfg:
        return {"ok": False, "error": f"Provider '{provider}' not supported for chat testing"}

    url = cfg["url"].format(key=key, model=model)
    hdrs = {k: v.format(key=key) for k, v in cfg["headers"].items()}
    hdrs["User-Agent"] = "provider-manager/2.0"

    body_str = cfg["body_template"].format(model=model, prompt=prompt)
    data = body_str.encode()

    req = urllib.request.Request(url, headers=hdrs, method=cfg["method"], data=data)

    start = time.monotonic()
    try:
        with urllib.request.urlopen(req, timeout=20) as resp:
            latency_ms = int((time.monotonic() - start) * 1000)
            resp_body = json.loads(resp.read().decode())
            text = _resolve_path(resp_body, cfg["response_path"])
            return {
                "ok": True,
                "provider": provider,
                "model": model,
                "key_masked": mask(key),
                "response": str(text)[:200] if text else "(empty response)",
                "latency_ms": latency_ms,
                "valid": True,
            }
    except urllib.error.HTTPError as exc:
        latency_ms = int((time.monotonic() - start) * 1000)
        body = exc.read().decode()[:500] if exc.fp else ""
        return {
            "ok": False,
            "error": f"{exc.code} — {body[:150]}",
            "provider": provider,
            "model": model,
            "key_masked": mask(key),
            "valid": False,
            "latency_ms": latency_ms,
        }
    except Exception as exc:
        latency_ms = int((time.monotonic() - start) * 1000)
        return {
            "ok": False,
            "error": str(exc)[:200],
            "provider": provider,
            "model": model,
            "key_masked": mask(key),
            "valid": False,
            "latency_ms": latency_ms,
        }


def run():
    parser = argparse.ArgumentParser()
    parser.add_argument("--provider", required=True,
                        help="Provider name: google, openai, deepseek, etc.")
    parser.add_argument("--model", required=True,
                        help="Model ID to test")
    parser.add_argument("--key", default="",
                        help="Specific API key to test (optional, uses config if omitted)")
    parser.add_argument("--prompt", default="Say hello in one word",
                        help="Test prompt to send")
    args = parser.parse_args()

    canonical = normalize(args.provider)
    key = args.key.strip() if args.key.strip() else None

    # If no key provided, get from config
    if not key:
        try:
            toml_str = get_config_toml()
            cfg = ConfigModel.from_toml(toml_str)
            # Try fallback keys first, then main api_key
            for prof, k in cfg.fallback_keys.items():
                if normalize(prof) == canonical and k and not k.startswith("***"):
                    key = k
                    break
            if not key and cfg.api_key and not cfg.api_key.startswith("***"):
                prov = normalize(cfg.default_provider or "")
                if prov == canonical:
                    key = cfg.api_key
        except Exception:
            pass

    if not key:
        emit(error("provider_test",
                    f"No API key found for '{args.provider}'. Provide --key or configure in fallback chain."))
        return

    result = test_model(canonical, args.model, key, args.prompt)
    if result.pop("ok"):
        emit(success("provider_test", result))
    else:
        err_msg = result.pop("error", "Unknown error")
        emit(error("provider_test", err_msg, result))


if __name__ == "__main__":
    run()
```

- [ ] **Step 2: Verify script loads without errors**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager/scripts && python3 -c "import provider_test; print('OK')"
```
Expected: `OK`

- [ ] **Step 3: Quick smoke test with gemini**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager/scripts && python3 provider_test.py --provider gemini --model gemini-2.5-flash --prompt "Say ok"
```
Expected: JSON with `"ok": true`, `"valid": true`, `"response"` contains text, `"latency_ms"` present.

- [ ] **Step 4: Test with invalid key**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager/scripts && python3 provider_test.py --provider deepseek --model deepseek-chat --key "sk-0000000000000000000000000000000000"
```
Expected: JSON with `"ok": false`, `"valid": false`.

- [ ] **Step 5: Commit**

```bash
cd ~/.zeroclaw/workspace/skills/provider-manager && git add scripts/provider_test.py && git commit -m "feat(provider-manager): add provider_test.py — chat completion testing + key validation"
```

---

## Task 3: Create provider_models.py

**Files:**
- Create: `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_models.py`

- [ ] **Step 1: Write provider_models.py**

```python
"""provider_models — list available models for a provider (JSON output)."""

from __future__ import annotations

import argparse
import json
import sys
import os
import urllib.request
import urllib.error

sys.path.insert(0, os.path.dirname(__file__))

from providers import normalize, get_provider, get_known_models, mask, PROVIDERS
from output import success, error, emit
from gateway import get_config_toml
from config_model import ConfigModel


# Providers that use GET /v1/models (OpenAI-compatible)
_MODELS_API: dict[str, str] = {
    "openai": "https://api.openai.com/v1/models",
    "deepseek": "https://api.deepseek.com/v1/models",
    "moonshot": "https://api.moonshot.cn/v1/models",
    "minimax": "https://api.minimax.chat/v1/models",
    "groq": "https://api.groq.com/openai/v1/models",
    "together": "https://api.together.xyz/v1/models",
    "mistral": "https://api.mistral.ai/v1/models",
    "fireworks": "https://api.fireworks.ai/inference/v1/models",
    "openrouter": "https://openrouter.ai/api/v1/models",
}

_GEMINI_MODELS_URL = "https://generativelanguage.googleapis.com/v1beta/models?key={key}"

# Filter out non-chat models (embedding, whisper, tts, etc.)
_SKIP_PATTERNS = ["embed", "whisper", "tts", "dall-e", "moderation", "davinci", "babbage"]


def _is_chat_model(model_id: str) -> bool:
    """Filter out non-chat models."""
    lower = model_id.lower()
    return not any(pat in lower for pat in _SKIP_PATTERNS)


def _fetch_models_openai_compat(url: str, key: str) -> list[str]:
    """Fetch models from OpenAI-compatible /v1/models endpoint."""
    hdrs = {"Authorization": f"Bearer {key}", "User-Agent": "provider-manager/2.0"}
    req = urllib.request.Request(url, headers=hdrs, method="GET")
    with urllib.request.urlopen(req, timeout=15) as resp:
        data = json.loads(resp.read().decode())
    models = [m["id"] for m in data.get("data", []) if isinstance(m, dict) and "id" in m]
    return sorted([m for m in models if _is_chat_model(m)])[:50]


def _fetch_models_gemini(key: str) -> list[str]:
    """Fetch models from Gemini API."""
    url = _GEMINI_MODELS_URL.format(key=key)
    hdrs = {"User-Agent": "provider-manager/2.0"}
    req = urllib.request.Request(url, headers=hdrs, method="GET")
    with urllib.request.urlopen(req, timeout=15) as resp:
        data = json.loads(resp.read().decode())
    models = []
    for m in data.get("models", []):
        name = m.get("name", "")
        # Gemini returns "models/gemini-2.5-flash" — strip prefix
        if name.startswith("models/"):
            name = name[7:]
        if name and _is_chat_model(name):
            models.append(name)
    return sorted(models)[:50]


def run():
    parser = argparse.ArgumentParser()
    parser.add_argument("--provider", required=True,
                        help="Provider name: google, openai, deepseek, etc.")
    parser.add_argument("--key", default="",
                        help="Specific API key (optional, uses config if omitted)")
    args = parser.parse_args()

    canonical = normalize(args.provider)
    prov_info = get_provider(args.provider)

    if not prov_info:
        emit(error("provider_models", f"Provider '{args.provider}' is not supported yet."))
        return

    key = args.key.strip() if args.key.strip() else None

    # If no key, get from config
    if not key:
        try:
            toml_str = get_config_toml()
            cfg = ConfigModel.from_toml(toml_str)
            for prof, k in cfg.fallback_keys.items():
                if normalize(prof) == canonical and k and not k.startswith("***"):
                    key = k
                    break
            if not key and cfg.api_key and not cfg.api_key.startswith("***"):
                prov = normalize(cfg.default_provider or "")
                if prov == canonical:
                    key = cfg.api_key
        except Exception:
            pass

    if not key:
        # Try static known models as fallback
        known = get_known_models(canonical)
        if known:
            emit(success("provider_models", {
                "provider": canonical,
                "models": known,
                "count": len(known),
                "source": "static",
            }))
            return
        emit(error("provider_models",
                    f"No API key found for '{args.provider}' and no static model list available."))
        return

    try:
        if canonical == "google":
            models = _fetch_models_gemini(key)
            source = "api"
        elif canonical in _MODELS_API:
            models = _fetch_models_openai_compat(_MODELS_API[canonical], key)
            source = "api"
        else:
            # Provider exists but no models API — use static
            known = get_known_models(canonical)
            if known:
                models = known
                source = "static"
            else:
                models = [prov_info["default_model"]]
                source = "default_only"

        emit(success("provider_models", {
            "provider": canonical,
            "models": models,
            "count": len(models),
            "source": source,
        }))

    except urllib.error.HTTPError as exc:
        body = exc.read().decode()[:200] if exc.fp else ""
        emit(error("provider_models", f"API error {exc.code}: {body[:150]}",
                    {"provider": canonical}))
    except Exception as exc:
        emit(error("provider_models", str(exc)[:200], {"provider": canonical}))


if __name__ == "__main__":
    run()
```

- [ ] **Step 2: Verify script loads**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager/scripts && python3 -c "import provider_models; print('OK')"
```
Expected: `OK`

- [ ] **Step 3: Smoke test with gemini**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager/scripts && python3 provider_models.py --provider gemini
```
Expected: JSON with `"ok": true`, `"models"` list containing gemini model names, `"source": "api"`.

- [ ] **Step 4: Test static fallback (anthropic)**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager/scripts && python3 provider_models.py --provider anthropic
```
Expected: JSON with `"source": "static"` (since no anthropic key in config).

- [ ] **Step 5: Commit**

```bash
cd ~/.zeroclaw/workspace/skills/provider-manager && git add scripts/provider_models.py && git commit -m "feat(provider-manager): add provider_models.py — list available models per provider"
```

---

## Task 4: Add remove_fallback to provider_apply.py

**Files:**
- Modify: `~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_apply.py:87-161`

- [ ] **Step 1: Add _action_remove_fallback handler function**

Add after the `_action_add_fallback` function (after line 112):

```python
def _action_remove_fallback(cfg: ConfigModel, profile: str):
    config = cfg.prepare_toml_for_write()
    reliability = config.setdefault("reliability", {})
    fallback_providers = reliability.get("fallback_providers", [])
    fallback_keys = reliability.get("fallback_api_keys", {})

    # Find matching profile(s) — support partial match (e.g. "groq" matches "groq:groq-1")
    removed = []
    new_chain = []
    for fp in fallback_providers:
        if fp == profile or fp.startswith(profile + ":"):
            removed.append(fp)
            # Also remove from keys
            fallback_keys.pop(fp, None)
        else:
            new_chain.append(fp)

    if not removed:
        emit(error("remove_fallback",
                    f"Profile '{profile}' not found in fallback chain. "
                    f"Current chain: {fallback_providers}"))
        return

    reliability["fallback_providers"] = new_chain
    reliability["fallback_api_keys"] = fallback_keys

    new_toml = tomli_w.dumps(config)
    gw_result = put_config_toml(new_toml)

    emit(success("remove_fallback", {
        "removed": removed,
        "chain": new_chain,
        "gateway": gw_result,
    }))
```

- [ ] **Step 2: Add remove_fallback to argparse choices**

In `run()` function, change line 118 from:
```python
                        choices=["replace_keys", "set_default", "add_fallback"])
```
to:
```python
                        choices=["replace_keys", "set_default", "add_fallback", "remove_fallback"])
```

- [ ] **Step 3: Add remove_fallback dispatch in run()**

Add after the `add_fallback` elif block (after line 157), before `if __name__`:

```python
    elif args.action == "remove_fallback":
        if not profile:
            emit(error("remove_fallback", "remove_fallback requires --profile"))
            return
        _action_remove_fallback(cfg, profile)
```

- [ ] **Step 4: Verify script loads**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager/scripts && python3 -c "import provider_apply; print('OK')"
```
Expected: `OK`

- [ ] **Step 5: Commit**

```bash
cd ~/.zeroclaw/workspace/skills/provider-manager && git add scripts/provider_apply.py && git commit -m "feat(provider-manager): add remove_fallback action to provider_apply"
```

---

## Task 5: Update SKILL.toml with new tools

**Files:**
- Modify: `~/.zeroclaw/workspace/skills/provider-manager/SKILL.toml`

- [ ] **Step 1: Update provider_apply tool description and action arg**

In `SKILL.toml`, change the `provider_apply` tool description to include `remove_fallback`:

In the `description` field (around line 93-103), add after `add_fallback` line:
```
  remove_fallback — remove provider from fallback chain
```

Add example:
```
  provider_apply(action="remove_fallback", profile="groq:groq-1")
```

Change `action` arg (line 110) from:
```
action = "REQUIRED. 'replace_keys', 'set_default', or 'add_fallback'"
```
to:
```
action = "REQUIRED. 'replace_keys', 'set_default', 'add_fallback', or 'remove_fallback'"
```

- [ ] **Step 2: Add provider_test tool definition**

Add after the last `[[tools]]` block (after line 131):

```toml
[[tools]]
name = "provider_test"
description = """Test a model by sending a real chat completion request. Also validates API keys.

Examples:
  provider_test(provider="gemini", model="gemini-2.5-flash")          — test model with config key
  provider_test(provider="deepseek", model="deepseek-chat", key="sk-...") — validate specific key
  provider_test(provider="openai", model="gpt-4o", prompt="Tell me a joke") — custom test prompt
"""
kind = "shell"
command = "~/.zeroclaw/workspace/.venv/bin/python3 ~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_test.py --provider {provider} --model {model} --key {key} --prompt {prompt}"
max_result_chars = 4000
max_calls_per_turn = 2

[tools.args]
provider = "REQUIRED. Provider: google, openai, anthropic, groq, deepseek, moonshot, minimax, together, mistral, fireworks, openrouter, perplexity"
model = "REQUIRED. Model ID to test, e.g. 'gemini-2.5-flash', 'deepseek-chat', 'gpt-4o'"
key = "Specific API key to validate (optional, uses configured key if empty)"
prompt = "Test prompt to send (optional, default: 'Say hello in one word')"
```

- [ ] **Step 3: Add provider_models tool definition**

Add after provider_test:

```toml
[[tools]]
name = "provider_models"
description = """List available models for a provider (JSON).

Examples:
  provider_models(provider="openai")           — list OpenAI models
  provider_models(provider="gemini")           — list Gemini models
  provider_models(provider="groq", key="gsk_...") — list with specific key
"""
kind = "shell"
command = "~/.zeroclaw/workspace/.venv/bin/python3 ~/.zeroclaw/workspace/skills/provider-manager/scripts/provider_models.py --provider {provider} --key {key}"
max_result_chars = 4000
max_calls_per_turn = 1

[tools.args]
provider = "REQUIRED. Provider: google, openai, anthropic, groq, deepseek, moonshot, minimax, together, mistral, fireworks, openrouter, perplexity"
key = "Specific API key to use (optional, uses configured key if empty)"
```

- [ ] **Step 4: Update the prompts section**

In the `prompts` array (lines 8-55), add the new tools to the TOOLS list and TYPICAL WORKFLOW:

After `provider_health()` entry, add:
```
  5. provider_test(provider, model, key?, prompt?)
     — Test a model with a real chat completion request. Also validates API keys.
     — JSON: {provider, model, key_masked, response, latency_ms, valid}

  6. provider_models(provider, key?)
     — List available models for a provider (live API or static)
     — JSON: {provider, models, count, source}
```

In the actions list under `provider_apply`, add:
```
     — remove_fallback — remove provider from fallback chain
```

- [ ] **Step 5: Add provider_test and provider_models to config.toml auto_approve**

Read `~/.zeroclaw/config.toml`, find the `auto_approve` list (under `[security]` or top-level). Add the two new tools:

```toml
auto_approve = [
    # ... existing entries like "provider_apply", "provider_status", "provider_find", "provider_health" ...
    "provider_test",
    "provider_models",
]
```

Verify with: `grep -A 30 'auto_approve' ~/.zeroclaw/config.toml`

- [ ] **Step 6: Verify TOML is valid**

Run:
```bash
cd ~/.zeroclaw/workspace/skills/provider-manager && python3 -c "import tomllib; tomllib.load(open('SKILL.toml','rb')); print('TOML OK')"
```
Expected: `TOML OK`

- [ ] **Step 7: Commit**

```bash
cd ~/.zeroclaw/workspace/skills/provider-manager && git add SKILL.toml && git commit -m "feat(provider-manager): add provider_test + provider_models tool defs, remove_fallback action"
```

---

## Task 6: Update TOOLS.md

**Files:**
- Modify: `~/.zeroclaw/workspace/TOOLS.md`

- [ ] **Step 1: Rewrite TOOLS.md with all workflows**

Replace the full content of `~/.zeroclaw/workspace/TOOLS.md` with the expanded version below.

```markdown
# Provider & Key Management

## How providers work
- ZeroClaw has a default provider + fallback chain in `config.toml`
- Each fallback entry is `provider:profile-name` with an API key
- If default fails, it tries fallbacks in order until one works
- Provider names: google, openai, deepseek, moonshot, minimax, groq, mistral, openrouter, together, perplexity, fireworks, anthropic
- Planned (not yet supported): zhipu, alibaba, sambanova, cohere

## Provider ↔ Model Reference

| Provider | Models (use exact ID) | Test prompt hint |
|----------|----------------------|------------------|
| google/gemini | `gemini-3-flash-preview`, `gemini-2.5-flash`, `gemini-2.5-pro`, `gemini-2.5-flash-lite` | Works with any prompt |
| openai | `gpt-4o`, `gpt-4o-mini`, `gpt-5.1`, `o4-mini` | Works with any prompt |
| deepseek | `deepseek-chat`, `deepseek-coder`, `deepseek-reasoner` | `deepseek-reasoner` is slow (~10s), use short prompt |
| moonshot | `moonshot-v1-128k`, `moonshot-v1-32k`, `moonshot-v1-8k` | Works with any prompt |
| minimax | `MiniMax-M1`, `MiniMax-Text-01` | Case-sensitive model IDs! |
| groq | `llama-3.3-70b-versatile`, `mixtral-8x7b-32768` | Fast responses (~200ms) |
| together | `meta-llama/Llama-3.3-70B-Instruct-Turbo` | Full path model IDs |
| mistral | `mistral-large-latest` | Works with any prompt |
| fireworks | `accounts/fireworks/models/llama-v3p3-70b-instruct` | Full path model IDs |
| openrouter | Any model via routing (e.g. `anthropic/claude-sonnet-4-6`) | Use provider/model format |
| perplexity | `sonar`, `sonar-pro`, `sonar-reasoning` | Returns with citations |
| anthropic | `claude-opus-4-6`, `claude-sonnet-4-6`, `claude-haiku-4-5-20251001` | Expensive, use short prompt |

## Tools — Full Reference

### 1. provider_status()
Show current providers, fallback chain, and live key status.
No parameters.

```
provider_status()
```

### 2. provider_find(provider, count)
Find working replacement keys from the key store.
- **provider** (REQUIRED, STRING): provider name — "google", "openai", "deepseek", etc.
- **count** (STRING, default "3"): how many keys to find — "1", "3", "5", "10"

```
provider_find(provider="google", count="3")
provider_find(provider="groq", count="5")
```

### 3. provider_apply(action, provider?, model?, profile?, keys?, profiles?)
Apply changes to provider config. Hot — no restart needed.

**Actions:**
- `set_default` — change default provider and model
  - **provider** (REQUIRED): provider name
  - **model** (optional): model ID; if omitted, uses provider's default model
  ```
  provider_apply(action="set_default", provider="deepseek", model="deepseek-chat")
  provider_apply(action="set_default", provider="gemini", model="gemini-2.5-flash")
  ```

- `add_fallback` — add provider to fallback chain
  - **profile** (REQUIRED): "provider:profile-name" format
  - **keys** (REQUIRED): API key
  ```
  provider_apply(action="add_fallback", profile="moonshot:ms-1", keys="sk-...")
  provider_apply(action="add_fallback", profile="groq:groq-1", keys="gsk_...")
  ```

- `remove_fallback` — remove provider from fallback chain
  - **profile** (REQUIRED): profile to remove (exact or prefix match, e.g. "groq" removes "groq:groq-1")
  ```
  provider_apply(action="remove_fallback", profile="groq:groq-1")
  provider_apply(action="remove_fallback", profile="moonshot")
  ```

- `replace_keys` — swap fallback API keys
  - **profiles** (REQUIRED): comma-separated "profile=KEY" pairs
  ```
  provider_apply(action="replace_keys", profiles="gemini:gemini-api-1=AIzaSy...,gemini:gemini-api-2=AIzaSy...")
  ```

### 4. provider_health()
Auto-heal: check all keys, replace dead ones from key store automatically.
No parameters.

```
provider_health()
```

### 5. provider_test(provider, model, key?, prompt?)
Test a model by sending a real chat completion request. Also validates API keys.
- **provider** (REQUIRED, STRING): provider name
- **model** (REQUIRED, STRING): model ID from the table above (use exact ID!)
- **key** (optional): specific API key to validate; if omitted, uses configured key
- **prompt** (optional): test prompt; default "Say hello in one word"

```
provider_test(provider="gemini", model="gemini-2.5-flash")
provider_test(provider="deepseek", model="deepseek-chat", key="sk-...")
provider_test(provider="openai", model="gpt-4o", prompt="What is 2+2?")
```

**How to test specific providers:**
- Gemini: `provider_test(provider="gemini", model="gemini-2.5-flash")` — cheapest, fastest
- OpenAI: `provider_test(provider="openai", model="gpt-4o-mini")` — cheapest OpenAI model
- DeepSeek: `provider_test(provider="deepseek", model="deepseek-chat")` — fast; avoid deepseek-reasoner for quick checks (slow)
- Groq: `provider_test(provider="groq", model="llama-3.3-70b-versatile")` — very fast
- MiniMax: `provider_test(provider="minimax", model="MiniMax-M1")` — note case-sensitive ID
- To validate a specific key: `provider_test(provider="deepseek", model="deepseek-chat", key="sk-the-key-here")`

### 6. provider_models(provider, key?)
List available models for a provider.
- **provider** (REQUIRED, STRING): provider name
- **key** (optional): specific API key; if omitted, uses configured key

```
provider_models(provider="openai")
provider_models(provider="gemini")
provider_models(provider="groq", key="gsk_...")
```

## Workflows

### "добавь провайдера X" / "add provider X to fallback"
1. Get a working key: `key_store(action="list", provider="X")` or `provider_find(provider="X", count="3")`
2. Add to fallback: `provider_apply(action="add_fallback", profile="X:x-1", keys="THE_KEY")`
3. Test it: `provider_test(provider="X", model="model-name")`

### "переключи на модель Y" / "switch to model Y"
1. `provider_apply(action="set_default", provider="X", model="Y")`

### "удали X из фоллбэков" / "remove X from fallbacks"
1. `provider_apply(action="remove_fallback", profile="X:x-1")` or `provider_apply(action="remove_fallback", profile="X")`

### "протестируй модель X" / "test model X"
1. `provider_test(provider="...", model="X")` — uses configured key
2. Report: response text, latency, valid/invalid

### "валидируй ключ" / "validate key"
1. `provider_test(provider="...", model="...", key="the-key")` — tests specific key
2. Report: valid or invalid with error reason

### "какие модели у X?" / "what models does X have?"
1. `provider_models(provider="X")`
2. Report: list of model IDs

### "проверь/почини провайдеры" / "check/fix providers"
1. `provider_health()` — auto-checks all, replaces dead keys

### "статус провайдеров" / "provider status"
1. `provider_status()`

## Multi-turn Examples

The bot keeps conversation context. "there", "that one", "first key" are resolved from previous messages.

**Example 1:**
- User: "сколько ключей deepseek?"
  → `key_store(action="list", provider="deepseek")` → "У deepseek 15 активных ключей"
- User: "какие модели там доступны?"
  → `provider_models(provider="deepseek")` → "deepseek-chat, deepseek-coder, deepseek-reasoner"
- User: "установи deepseek-reasoner основной"
  → `provider_apply(action="set_default", provider="deepseek", model="deepseek-reasoner")`

**Example 2:**
- User: "найди ключи groq"
  → `provider_find(provider="groq", count="3")` → "Нашёл 3 рабочих ключа: gsk_..."
- User: "проверь первый"
  → `provider_test(provider="groq", model="llama-3.3-70b-versatile", key="gsk_...")` → "Ключ валиден, latency 180ms"
- User: "добавь его"
  → `provider_apply(action="add_fallback", profile="groq:groq-1", keys="gsk_...")`

## CRITICAL RULES
- key_store action parameter is ALWAYS a STRING: "list", "stats" — NEVER a number
- provider_find count is a STRING: "3", "5" — NEVER a number
- provider_test model is ALWAYS a STRING with exact model ID from the table above
- MiniMax model IDs are case-sensitive: "MiniMax-M1", NOT "minimax-m1"
- together/fireworks model IDs use full paths: "meta-llama/Llama-3.3-70B-Instruct-Turbo"
- When user asks to add a provider, ALWAYS get a key first (key_store or provider_find), then provider_apply
- Do NOT invent API keys. Always get them from key_store or provider_find
- After any provider change, tell the user what was done and whether it worked
- When testing a model, pick the cheapest/fastest variant (e.g. gpt-4o-mini not gpt-5.1, gemini-2.5-flash not gemini-2.5-pro)
- Respond in the SAME LANGUAGE as the user's question
```

- [ ] **Step 2: Verify no broken formatting**

Read the file back and check markdown structure.

- [ ] **Step 3: Commit**

```bash
cd ~/.zeroclaw/workspace && git add TOOLS.md && git commit -m "docs: expand TOOLS.md with provider_test, provider_models, remove_fallback workflows + multi-turn examples"
```

---

## Task 7: Restart daemon to pick up new tools

- [ ] **Step 1: Restart daemon**

Run:
```bash
cd /home/spex/work/erp/zeroclaws && ./dev/restart-daemon.sh
```

- [ ] **Step 2: Verify new tools are loaded**

Run:
```bash
curl -s http://127.0.0.1:42617/api/status | python3 -c "import sys,json; d=json.load(sys.stdin); print('Tools:', [t for t in d.get('tools',[]) if 'provider' in t])"
```
Expected: list includes `provider_test` and `provider_models`.

- [ ] **Step 3: Quick manual test via bot**

Send in Telegram: "протестируй модель gemini-2.5-flash"
Expected: bot calls `provider_test`, returns response with latency.

---

## Task 8: Create E2E test infrastructure

**Files:**
- Create: `tests/provider_management_e2e.rs`

- [ ] **Step 1: Write test helpers and first 3 single-turn tests**

Create `tests/provider_management_e2e.rs` with:

```rust
//! End-to-end provider management tests via WebSocket.
//!
//! These tests send natural language messages to ZeroClaw via the gateway
//! WebSocket and assert on the bot's responses (tool calls + content).
//!
//! Requirements:
//!   - Running ZeroClaw daemon with gateway on port 42617
//!   - ZEROCLAW_GATEWAY_TOKEN env var (or auto-pairing enabled)
//!   - Network access to provider APIs (for tests that validate real calls)
//!
//! Run:
//!   source .env && cargo test --test provider_management_e2e -- --ignored --test-threads=1

use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use std::time::Duration;
use tokio::time::timeout;
use tokio_tungstenite::{connect_async, tungstenite::Message};

/// Gateway WebSocket URL.
fn ws_url() -> String {
    let port = std::env::var("ZEROCLAW_GATEWAY_PORT").unwrap_or_else(|_| "42617".into());
    let token = std::env::var("ZEROCLAW_GATEWAY_TOKEN").unwrap_or_default();
    if token.is_empty() {
        format!("ws://127.0.0.1:{port}/ws/chat")
    } else {
        format!("ws://127.0.0.1:{port}/ws/chat?token={token}")
    }
}

/// Send a message and wait for "done" or "error" response.
/// Returns the full_response text.
async fn send_and_wait(
    sender: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    receiver: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    content: &str,
) -> String {
    let msg = serde_json::json!({"type": "message", "content": content});
    sender
        .send(Message::Text(msg.to_string().into()))
        .await
        .expect("send failed");

    // Wait up to 120s for done/error (bot may call tools which take time)
    let deadline = Duration::from_secs(120);
    let result = timeout(deadline, async {
        while let Some(Ok(frame)) = receiver.next().await {
            if let Message::Text(text) = frame {
                if let Ok(v) = serde_json::from_str::<Value>(&text) {
                    match v["type"].as_str() {
                        Some("done") => {
                            return v["full_response"]
                                .as_str()
                                .unwrap_or("")
                                .to_string();
                        }
                        Some("error") => {
                            return format!(
                                "ERROR: {}",
                                v["message"].as_str().unwrap_or("unknown")
                            );
                        }
                        _ => continue,
                    }
                }
            }
        }
        "ERROR: connection closed".to_string()
    })
    .await;

    result.unwrap_or_else(|_| "ERROR: timeout".to_string())
}

/// Open a WebSocket connection, skip the session_start frame.
async fn connect() -> (
    futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
) {
    let url = ws_url();
    let (ws_stream, _) = connect_async(&url)
        .await
        .unwrap_or_else(|e| panic!("Failed to connect to {url}: {e}"));

    let (sender, mut receiver) = ws_stream.split();

    // Skip session_start message
    if let Some(Ok(Message::Text(_))) = receiver.next().await {
        // session_start consumed
    }

    (sender, receiver)
}

// ──────────────────────────────────────────────────────────
// Single-turn tests
// ──────────────────────────────────────────────────────────

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_01_provider_status() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "покажи статус провайдеров").await;
    assert!(
        !resp.starts_with("ERROR"),
        "Got error: {resp}"
    );
    // Should mention the default provider
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("gemini") || lower.contains("default") || lower.contains("провайдер"),
        "Response should contain provider info: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_02_key_count() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "сколько у нас ключей deepseek?").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    // Should contain a number
    assert!(
        resp.chars().any(|c| c.is_ascii_digit()),
        "Response should contain key count: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_05_test_model() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "проверь модель gemini-2.5-flash").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("latency") || lower.contains("мс") || lower.contains("ms")
            || lower.contains("response") || lower.contains("ответ")
            || lower.contains("valid") || lower.contains("работает"),
        "Response should contain test result: {resp}"
    );
}
```

- [ ] **Step 2: Verify it compiles**

Run:
```bash
cd /home/spex/work/erp/zeroclaws && cargo test --test provider_management_e2e --no-run 2>&1 | tail -5
```
Expected: compiles successfully.

- [ ] **Step 3: Run the first 3 tests**

Run:
```bash
source .env && cargo test --test provider_management_e2e -- --ignored --test-threads=1 --nocapture 2>&1 | tail -20
```
Expected: 3 tests pass (pm_01, pm_02, pm_05).

- [ ] **Step 4: Commit**

```bash
cd /home/spex/work/erp/zeroclaws && git add tests/provider_management_e2e.rs && git commit -m "test: add provider management E2E test infrastructure + first 3 tests"
```

---

## Task 9: Add remaining single-turn E2E tests

**Files:**
- Modify: `tests/provider_management_e2e.rs`

- [ ] **Step 1: Add tests pm_06 through pm_09, pm_13, pm_14**

Append to the test file:

```rust
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_06_list_models() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "какие модели есть у openai?").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("gpt") || lower.contains("model") || lower.contains("модел"),
        "Response should list models: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_07_switch_provider() {
    let (mut tx, mut rx) = connect().await;

    // First, save current state
    let status_before = send_and_wait(&mut tx, &mut rx, "какой сейчас основной провайдер?").await;

    // Switch to deepseek
    let resp = send_and_wait(&mut tx, &mut rx, "переключи на deepseek-chat").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("deepseek") || lower.contains("переключ") || lower.contains("установлен")
            || lower.contains("switch") || lower.contains("default"),
        "Response should confirm switch: {resp}"
    );

    // Switch back to gemini
    let _ = send_and_wait(&mut tx, &mut rx, "переключи на gemini-3-flash-preview").await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_08_provider_health() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "проверь здоровье провайдеров").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("check") || lower.contains("провер") || lower.contains("ключ")
            || lower.contains("key") || lower.contains("dead") || lower.contains("replaced")
            || lower.contains("ok") || lower.contains("здоров"),
        "Response should contain health info: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_09_validate_key() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(
        &mut tx,
        &mut rx,
        "валидируй ключ deepseek sk-a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4",
    )
    .await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    // Invalid key should be reported
    assert!(
        lower.contains("invalid") || lower.contains("невалид") || lower.contains("ошибк")
            || lower.contains("fail") || lower.contains("401") || lower.contains("неверн")
            || lower.contains("dead") || lower.contains("не работает"),
        "Response should indicate invalid key: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_13_english_input() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "find me some groq keys").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("groq") || lower.contains("key") || lower.contains("found")
            || lower.contains("gsk_"),
        "Response should contain groq key info: {resp}"
    );
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_14_current_default() {
    let (mut tx, mut rx) = connect().await;
    let resp = send_and_wait(&mut tx, &mut rx, "что сейчас основной провайдер?").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("gemini") || lower.contains("default") || lower.contains("основн")
            || lower.contains("провайдер"),
        "Response should mention current default: {resp}"
    );
}
```

- [ ] **Step 2: Run all single-turn tests**

Run:
```bash
source .env && cargo test --test provider_management_e2e -- --ignored --test-threads=1 --nocapture 2>&1 | tail -30
```
Expected: All single-turn tests pass (pm_01, 02, 05, 06, 07, 08, 09, 13, 14).

- [ ] **Step 3: Commit**

```bash
cd /home/spex/work/erp/zeroclaws && git add tests/provider_management_e2e.rs && git commit -m "test: add remaining single-turn provider management E2E tests"
```

---

## Task 10: Add config-mutating E2E tests (add/remove fallback)

**Files:**
- Modify: `tests/provider_management_e2e.rs`

- [ ] **Step 1: Add config snapshot/restore helpers**

Add after the `connect()` function:

```rust
/// Snapshot current config.toml via gateway API.
async fn snapshot_config() -> String {
    let port = std::env::var("ZEROCLAW_GATEWAY_PORT").unwrap_or_else(|_| "42617".into());
    let token = std::env::var("ZEROCLAW_GATEWAY_TOKEN").unwrap_or_default();
    let url = format!("http://127.0.0.1:{port}/api/config");
    let client = reqwest::Client::new();
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("snapshot GET failed")
        .json::<Value>()
        .await
        .expect("snapshot parse failed");
    resp["content"].as_str().unwrap_or("").to_string()
}

/// Restore config.toml via gateway API.
async fn restore_config(toml: &str) {
    let port = std::env::var("ZEROCLAW_GATEWAY_PORT").unwrap_or_else(|_| "42617".into());
    let token = std::env::var("ZEROCLAW_GATEWAY_TOKEN").unwrap_or_default();
    let url = format!("http://127.0.0.1:{port}/api/config");
    let client = reqwest::Client::new();
    let _ = client
        .put(&url)
        .bearer_auth(&token)
        .body(toml.to_string())
        .send()
        .await;
}
```

Note: `reqwest` is already a dependency in `Cargo.toml` — no changes needed.

- [ ] **Step 2: Add pm_03 (add fallback) and pm_04 (remove fallback)**

```rust
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_03_add_fallback() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Bot should autonomously: 1) find a moonshot key (key_store/provider_find)
    // 2) add it to fallback (provider_apply). Both happen in one turn.
    let resp = send_and_wait(&mut tx, &mut rx, "добавь moonshot в фоллбэк").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    // Must mention moonshot AND confirm it was added (not just "found keys")
    assert!(
        lower.contains("moonshot"),
        "Response should mention moonshot: {resp}"
    );
    assert!(
        lower.contains("добавлен") || lower.contains("fallback") || lower.contains("added")
            || lower.contains("add_fallback") || lower.contains("chain"),
        "Response should confirm addition to fallback chain: {resp}"
    );

    // Restore config
    restore_config(&snapshot).await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_04_remove_fallback() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // First ensure groq is in fallback (add it)
    let _ = send_and_wait(&mut tx, &mut rx, "добавь groq в фоллбэк").await;
    // Now remove it
    let resp = send_and_wait(&mut tx, &mut rx, "удали groq из фоллбэков").await;
    assert!(!resp.starts_with("ERROR"), "Got error: {resp}");
    let lower = resp.to_lowercase();
    assert!(
        lower.contains("удал") || lower.contains("remov") || lower.contains("groq"),
        "Response should confirm removal: {resp}"
    );

    // Restore config
    restore_config(&snapshot).await;
}
```

- [ ] **Step 3: Run config-mutating tests**

Run:
```bash
source .env && cargo test --test provider_management_e2e pm_03 pm_04 -- --ignored --test-threads=1 --nocapture 2>&1
```
Expected: Both pass, config is restored after each test.

- [ ] **Step 4: Commit**

```bash
cd /home/spex/work/erp/zeroclaws && git add tests/provider_management_e2e.rs && git commit -m "test: add config-mutating E2E tests (add/remove fallback) with snapshot/restore"
```

---

## Task 11: Add multi-turn E2E tests

**Files:**
- Modify: `tests/provider_management_e2e.rs`

- [ ] **Step 1: Add pm_10 (multi-turn: key count → models → set default)**

```rust
#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_10_multi_turn_deepseek_chain() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: ask about deepseek keys
    let r1 = send_and_wait(&mut tx, &mut rx, "сколько ключей deepseek?").await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");
    assert!(
        r1.chars().any(|c| c.is_ascii_digit()),
        "Step 1 should contain count: {r1}"
    );

    // Step 2: "what models there?" — bot should resolve "there" = deepseek
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(&mut tx, &mut rx, "какие модели там доступны?").await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");
    let lower2 = r2.to_lowercase();
    assert!(
        lower2.contains("deepseek") || lower2.contains("model") || lower2.contains("модел"),
        "Step 2 should list deepseek models: {r2}"
    );

    // Step 3: "set deepseek-reasoner as default" — continuing deepseek context
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r3 = send_and_wait(&mut tx, &mut rx, "установи deepseek-reasoner основной").await;
    assert!(!r3.starts_with("ERROR"), "Step 3 error: {r3}");
    let lower3 = r3.to_lowercase();
    assert!(
        lower3.contains("deepseek") || lower3.contains("установлен") || lower3.contains("default"),
        "Step 3 should confirm switch: {r3}"
    );

    restore_config(&snapshot).await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_11_multi_turn_add_test_default() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: add minimax
    let r1 = send_and_wait(&mut tx, &mut rx, "добавь minimax в фоллбэк").await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");

    // Step 2: test MiniMax-M1
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(&mut tx, &mut rx, "протестируй MiniMax-M1").await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");

    // Step 3: set as default
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r3 = send_and_wait(&mut tx, &mut rx, "сделай основным").await;
    assert!(!r3.starts_with("ERROR"), "Step 3 error: {r3}");

    restore_config(&snapshot).await;
}

#[tokio::test]
#[ignore = "requires running ZeroClaw daemon"]
async fn pm_12_multi_turn_find_validate_add() {
    let snapshot = snapshot_config().await;
    let (mut tx, mut rx) = connect().await;

    // Step 1: find groq keys
    let r1 = send_and_wait(&mut tx, &mut rx, "найди ключи groq").await;
    assert!(!r1.starts_with("ERROR"), "Step 1 error: {r1}");
    let lower1 = r1.to_lowercase();
    if lower1.contains("не найден") || lower1.contains("not found") || lower1.contains("0 key") {
        eprintln!("SKIP pm_12: no groq keys in store");
        restore_config(&snapshot).await;
        return;
    }

    // Step 2: validate first key
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r2 = send_and_wait(&mut tx, &mut rx, "проверь первый ключ").await;
    assert!(!r2.starts_with("ERROR"), "Step 2 error: {r2}");

    // Step 3: add it
    tokio::time::sleep(Duration::from_secs(2)).await;
    let r3 = send_and_wait(&mut tx, &mut rx, "добавь его в фоллбэк").await;
    assert!(!r3.starts_with("ERROR"), "Step 3 error: {r3}");

    restore_config(&snapshot).await;
}
```

- [ ] **Step 2: Run multi-turn tests**

Run:
```bash
source .env && cargo test --test provider_management_e2e pm_10 pm_11 pm_12 -- --ignored --test-threads=1 --nocapture 2>&1
```
Expected: All pass (pm_12 may skip if no groq keys).

- [ ] **Step 3: Commit**

```bash
cd /home/spex/work/erp/zeroclaws && git add tests/provider_management_e2e.rs && git commit -m "test: add multi-turn provider management E2E tests (context resolution)"
```

---

## Task 12: Run full E2E suite and fix failures

- [ ] **Step 1: Run all 14 tests**

Run:
```bash
source .env && cargo test --test provider_management_e2e -- --ignored --test-threads=1 --nocapture 2>&1
```

- [ ] **Step 2: Fix any failures**

For each failing test:
1. Read the error output
2. Determine if the issue is in the test assertion (too strict/wrong pattern) or in the tool/bot behavior
3. Fix accordingly
4. Re-run the specific test

- [ ] **Step 3: Final commit**

```bash
cd /home/spex/work/erp/zeroclaws && git add tests/provider_management_e2e.rs && git commit -m "test: fix E2E test assertions after full suite run"
```

---

## Execution Order Summary

| Task | Depends On | Description |
|------|-----------|-------------|
| 1 | — | providers.py: CHAT_ENDPOINTS + KNOWN_MODELS |
| 2 | 1 | provider_test.py |
| 3 | 1 | provider_models.py |
| 4 | — | provider_apply.py: remove_fallback |
| 5 | 1,2,3,4 | SKILL.toml + auto_approve |
| 6 | — | TOOLS.md |
| 7 | 1-6 | Restart daemon |
| 8 | 7 | E2E test infrastructure + first 3 tests |
| 9 | 8 | Remaining single-turn E2E tests |
| 10 | 9 | Config-mutating E2E tests |
| 11 | 10 | Multi-turn E2E tests |
| 12 | 11 | Full suite run + fix |

**Parallelizable:** Tasks 1-4 can run in parallel (independent scripts). Task 6 is independent of 1-5.
