# Getting Started Docs

For first-time setup and quick orientation.

## Start Path

1. Main overview and quick start: [../../README.md](../../README.md)
2. One-click setup and dual bootstrap mode: [../one-click-bootstrap.md](../one-click-bootstrap.md)
3. Find commands by tasks: [../commands-reference.md](../commands-reference.md)

## Choose Your Path

| Scenario | Command |
|----------|---------|
| I have an API key, want fastest setup | `zeroclaw onboard --api-key sk-... --provider openrouter` |
| I want guided prompts | `zeroclaw onboard --interactive` |
| Config exists, just fix channels | `zeroclaw onboard --channels-only` |
| Using subscription auth | See [Subscription Auth](../../README.md#subscription-auth-openai-codex--claude-code) |

## Onboarding and Validation

- Quick onboarding: `zeroclaw onboard --api-key "sk-..." --provider openrouter`
- Interactive onboarding: `zeroclaw onboard --interactive`
- Validate environment: `zeroclaw status` + `zeroclaw doctor`

## Next

- Runtime operations: [../operations/README.md](../operations/README.md)
- Reference catalogs: [../reference/README.md](../reference/README.md)
