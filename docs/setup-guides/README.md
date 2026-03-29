# Getting Started Docs

For first-time setup and quick orientation.

## Start Path

1. Main overview and quick start: [../../README.md](../../README.md)
2. One-click setup and dual bootstrap mode: [one-click-bootstrap.md](one-click-bootstrap.md)
3. Update or uninstall on macOS: [macos-update-uninstall.md](macos-update-uninstall.md)
4. Find commands by tasks: [../reference/cli/commands-reference.md](../reference/cli/commands-reference.md)

## Choose Your Path

| Scenario | Command |
|----------|---------|
| I have an API key, want fastest setup | `zeroclaw onboard --api-key sk-... --provider openrouter` |
| I want guided prompts | `zeroclaw onboard` |
| Config exists, just fix channels | `zeroclaw onboard --channels-only` |
| Config exists, I intentionally want full overwrite | `zeroclaw onboard --force` |
| Using subscription auth | See [Subscription Auth](../../README.md#subscription-auth-openai-codex--claude-code) |

## Onboarding and Validation

- Quick onboarding: `zeroclaw onboard --api-key "sk-..." --provider openrouter`
- Guided onboarding: `zeroclaw onboard`
- Existing config protection: reruns require explicit confirmation (or `--force` in non-interactive flows)
- Ollama cloud models (`:cloud`) require a remote `api_url` and API key (for example `api_url = "https://ollama.com"`).
- Validate environment: `zeroclaw status` + `zeroclaw doctor`

## Provider-Specific Guides

- [GitHub Copilot Provider](copilot-provider.md) — use your Copilot subscription with ZeroClaw (OAuth device flow)
- [Z.AI GLM Setup](zai-glm-setup.md) — Z.AI/GLM models through OpenAI-compatible endpoints

## Next

- Runtime operations: [../ops/README.md](../ops/README.md)
- Reference catalogs: [../reference/README.md](../reference/README.md)
- macOS lifecycle tasks: [macos-update-uninstall.md](macos-update-uninstall.md)
