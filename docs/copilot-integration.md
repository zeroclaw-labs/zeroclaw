# GitHub Copilot Integration — Change Summary (Initial)

Date: 2026-04-04

## Overview

This document summarizes the recent changes that add native GitHub Copilot provider support and update curated model lists across the ZeroClaw repository. It describes what was changed, how the new Copilot provider works, how to test it, and next steps.

## High-level summary

- Added a native GitHub Copilot provider with device-code OAuth support and token lifecycle management.
- Added CLI helper to perform Copilot device-code login: `models auth login-github-copilot`.
- Integrated Copilot as an onboarding provider option and updated onboarding guidance.
- Updated curated model lists to include canonical Copilot model IDs (e.g., `gpt-5.4`, `gpt-5.4-mini`, `gpt-4.1`, `gpt-4o`, `gemini-3.1-pro`, `gemini-2.5-pro`).
- Added optional OpenClaw plugin scaffolding for a local Copilot proxy/bridge.

## Files changed or added

- `src/auth/github_copilot_oauth.rs` — Device-code OAuth helpers (request + poll).
- `src/providers/copilot.rs` — Copilot provider implementation (token handling, dual-transport chat, parsing).
- `src/providers/mod.rs` — Provider factory wiring and env-var candidate additions.
- `src/onboard/wizard.rs` — Onboarding additions (Copilot provider choice, curated models guidance).
- `src/main.rs` — CLI additions: `models auth login-github-copilot` subcommand.
- `src/tools/model_switch.rs` — Curated model lists updated for OpenAI/Copilot/Gemini providers.
- `openclaw/github-copilot/*` & `openclaw/copilot-proxy/*` — TypeScript plugin scaffolding (optional).
- `docs/copilot-integration.md` — This initial documentation file.

## Implementation details

### Authentication and token flow

- The provider uses GitHub's Device Authorization Flow:
  1. `request_device_code()` calls GitHub's device endpoint and returns a `user_code`, `verification_uri`, and `device_code`.
  2. The user opens the verification URL, submits the code, and authorizes the application.
  3. `poll_for_access_token()` polls the token endpoint until the token is ready, then stores the token in the repo's `AuthProfile` store.
- Tokens are cached in AuthProfile and respect existing repo encryption/storage patterns. A pre-acquired token can be supplied via `COPILOT_GITHUB_TOKEN` (env-var candidate), where appropriate.

### Provider behavior

- Dual-transport support:
  - For GPT-style Copilot models (e.g., `gpt-5.4`, `gpt-4o`), the provider uses an OpenAI-compatible request/response path.
  - For Claude/Anthropic-style models (e.g., `claude-opus-4.6`), the provider uses an Anthropic-style message API path.
  - Transport selection is based on model ID heuristics inside the provider.
- Messages and responses are converted to/from the agent's internal chat format, including tool invocations when present.

### Model catalog changes

Curated model additions were aligned with Github Copilot docs as of 2026-04-04. Notable canonical IDs added:

- OpenAI / Copilot models: `gpt-5.4`, `gpt-5.4-mini`, `gpt-5.3`, `gpt-5.3-codex`, `gpt-5.2`, `gpt-5.2-codex`, `gpt-5.1`, `gpt-5-mini`, `gpt-4.1`, `gpt-4o`.
- Google / Gemini: `gemini-3.1-pro`, `gemini-3-pro`, `gemini-3-flash`, `gemini-2.5-pro`, `gemini-2.0-flash`.
- Anthropic / Claude: `claude-opus-4.6`, `claude-opus-4.5`, `claude-sonnet-4.5`, `claude-haiku-4.5`.
- xAI / Grok: `grok-code-fast-1`.

These are surfaced in `src/tools/model_switch.rs` for quick listing via the model switch tool.

## How to use and verify

1. Format & build checks

```bash
cargo fmt --all
cargo check --all-targets
```

1. Onboarding (choose Copilot)

```bash
zeroclaw onboard
# or
zeroclaw onboard --provider github-copilot
```

1. Login to Copilot via CLI helper

```bash
zeroclaw models auth login-github-copilot
```

Follow the displayed device-code instructions to authorize.

1. List and set models

- Use the `model_switch` tool to list models for a provider (agent tool or CLI if available):
  - action: `list_models`, provider: `github-copilot`
- Example set via agent tool or CLI:

```bash
# Example (CLI wrapper may vary):
zeroclaw models set github-copilot gpt-5.4
```

1. Verify the agent is using the requested model

```bash
zeroclaw status
zeroclaw agent -m "Which model are you using?"
```

## Environment variables

- `COPILOT_GITHUB_TOKEN` — optional pre-supplied GitHub Copilot token (if you prefer non-interactive setup).

## Tests & CI notes

- Local unit tests for Copilot provider and CLI parsing were exercised during development.
- `cargo fmt` and `cargo check` were run successfully.
- Repo-wide `clippy` with `-D warnings` may fail due to unrelated existing lints elsewhere — consider a separate cleanup PR.

## Limitations & caveats

- Model availability varies by Copilot plan and client; some models are preview/plan-limited.
- Device-code requires user interaction; automated CI flows should use pre-provisioned tokens.
- Model IDs and availability may change; keep the curated list in `src/tools/model_switch.rs` synchronized with GitHub docs.

## Follow-ups and recommendations

- Add mocked HTTP-based unit tests for the device-code flow to enable CI validation without request to live GitHub endpoints.
- Onboarding defaults now surface `gpt-5.4` / `gpt-5.4-mini` as Copilot suggestions.
- Address repo-wide clippy warnings in a separate cleanup PR.
- Exercise the OpenClaw TypeScript plugin scaffolding in an OpenClaw runtime to validate JS-side behavior.

---

If you'd like a different filename/location, or a shorter/longer form (e.g., a PR-ready changelog/PR body), say how you'd like it and I will update it.
