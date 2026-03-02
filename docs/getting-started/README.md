# Getting Started Docs

For first-time setup and quick orientation.

## Start Path

1. Main overview and quick start: [../../README.md](../../README.md)
2. One-click setup and dual bootstrap mode: [../one-click-bootstrap.md](../one-click-bootstrap.md)
3. Update or uninstall on macOS: [macos-update-uninstall.md](macos-update-uninstall.md)
4. Set up on Android (Termux/ADB): [../android-setup.md](../android-setup.md)
5. Find commands by tasks: [../commands-reference.md](../commands-reference.md)

## Choose Your Path

| Scenario | Command |
|----------|---------|
| I have an API key, want fastest setup | `zeroclaw onboard --api-key sk-... --provider openrouter` |
| I want guided prompts | `zeroclaw onboard --interactive` |
| Config exists, just fix channels | `zeroclaw onboard --channels-only` |
| Config exists, I intentionally want full overwrite | `zeroclaw onboard --force` |
| Using OpenAI Codex subscription auth | See [OpenAI Codex OAuth Quick Setup](#openai-codex-oauth-quick-setup) |

## Onboarding and Validation

- Quick onboarding: `zeroclaw onboard --api-key "sk-..." --provider openrouter`
- Interactive onboarding: `zeroclaw onboard --interactive`
- Existing config protection: reruns require explicit confirmation (or `--force` in non-interactive flows)
- Ollama cloud models (`:cloud`) require a remote `api_url` and API key (for example `api_url = "https://ollama.com"`).
- Validate environment: `zeroclaw status` + `zeroclaw doctor`

## OpenAI Codex OAuth Quick Setup

Use this path when you want `openai-codex` with subscription OAuth credentials (no API key required).

1. Authenticate:

```bash
zeroclaw auth login --provider openai-codex
```

2. Verify auth material is loaded:

```bash
zeroclaw auth status --provider openai-codex
```

3. Set provider/model defaults:

```toml
default_provider = "openai-codex"
default_model = "gpt-5.3-codex"
default_temperature = 0.2

[provider]
transport = "auto"
reasoning_level = "high"
```

4. Optional stable fallback model (if your account/region does not currently expose `gpt-5.3-codex`):

```toml
default_model = "gpt-5.2-codex"
```

5. Start chat:

```bash
zeroclaw chat
```

Notes:
- You do not need to define a custom `[model_providers."openai-codex"]` block for normal OAuth usage.
- If you see raw `<tool_call>` tags in output, first verify you are on the built-in `openai-codex` provider path above and not a custom OpenAI-compatible provider override.

## Next

- Runtime operations: [../operations/README.md](../operations/README.md)
- Reference catalogs: [../reference/README.md](../reference/README.md)
- macOS lifecycle tasks: [macos-update-uninstall.md](macos-update-uninstall.md)
- Android setup path: [../android-setup.md](../android-setup.md)
