# GitHub Copilot Provider Setup

ZeroClaw supports GitHub Copilot as a model provider using the same OAuth
device-flow authentication that VS Code and other third-party Copilot
integrations use.

## Prerequisites

- A GitHub account with an **active GitHub Copilot subscription**
  (Individual, Business, or Enterprise).
- A working internet connection (the device-flow requires browser access to
  `github.com/login/device`).

## Quick Start (Onboarding Wizard)

```bash
zeroclaw onboard --provider copilot
```

No API key is required. On first use, ZeroClaw will prompt you to visit
GitHub's device authorization page and enter a one-time code.

## Manual Configuration

Edit `~/.zeroclaw/config.toml`:

```toml
default_provider = "copilot"
default_model = "gpt-4o"
default_temperature = 0.7
```

Then run any command (for example `zeroclaw agent -m "Hello!"`). The OAuth
device-flow login will trigger automatically on first request.

### Skipping Device Flow with a GitHub Token

If you already have a GitHub personal access token (PAT) with Copilot
access, you can supply it directly:

```toml
api_key = "ghp_YOUR_GITHUB_TOKEN"
default_provider = "copilot"
default_model = "gpt-4o"
```

Or via environment variable:

```bash
export API_KEY="ghp_YOUR_GITHUB_TOKEN"
```

When `api_key` is set, ZeroClaw skips the device-flow login and uses the
token directly to obtain short-lived Copilot API keys.

## How Authentication Works

1. **Device code request** -- ZeroClaw requests a device code from GitHub
   using the VS Code Copilot OAuth client ID.
2. **User authorization** -- you visit `https://github.com/login/device` in
   a browser and enter the displayed code.
3. **Token exchange** -- once authorized, ZeroClaw receives a GitHub access
   token and exchanges it for a short-lived Copilot API key via
   `api.github.com/copilot_internal/v2/token`.
4. **Token caching** -- the access token and API key are cached to
   `~/.config/zeroclaw/copilot/` (with `0600` permissions on Unix) and
   automatically refreshed before expiry.

No secrets are stored in the ZeroClaw config file when using device-flow.

## Obtaining a GitHub Token Manually

If you prefer to supply a token instead of using device-flow:

### From GitHub CLI

```bash
gh auth token
```

### From VS Code

1. Open VS Code with the GitHub Copilot extension installed.
2. Open the command palette (`Ctrl+Shift+P` / `Cmd+Shift+P`).
3. Run "GitHub Copilot: Sign In" if not already signed in.
4. The token is stored in VS Code's secret storage -- use `gh auth token`
   for a simpler extraction path.

### From GitHub Settings

1. Go to [github.com/settings/tokens](https://github.com/settings/tokens).
2. Create a personal access token (classic) with the `read:user` scope.
3. Use this token as the `api_key` value.

Note: a PAT only works if your GitHub account has an active Copilot
subscription. The token itself does not grant Copilot access.

## Available Models

The Copilot API serves models through `api.githubcopilot.com`. Commonly
available models include:

| Model | Description |
|-------|-------------|
| `gpt-4o` | GPT-4o (recommended default) |
| `gpt-4o-mini` | GPT-4o mini (faster, lower cost) |
| `gpt-4` | GPT-4 (previous generation) |
| `claude-3.5-sonnet` | Claude 3.5 Sonnet (if enabled on your plan) |
| `o1-preview` | o1 Preview (reasoning model, if available) |
| `o1-mini` | o1 mini (reasoning, faster) |

Model availability depends on your Copilot plan and GitHub's current
model roster. GitHub may add or remove models at any time.

## Provider Aliases

ZeroClaw accepts both `copilot` and `github-copilot` as the provider name:

```bash
zeroclaw onboard --provider copilot
zeroclaw onboard --provider github-copilot
```

## Verify Setup

```bash
# Test agent directly
zeroclaw agent -m "Hello"

# Check configuration status
zeroclaw status
```

## Proxy Configuration

If you need to route Copilot traffic through an HTTP proxy, set the
`provider.copilot` proxy service key in your config or use the standard
`HTTPS_PROXY` / `HTTP_PROXY` environment variables.

## Troubleshooting

### "Ensure your GitHub account has an active Copilot subscription"

**Symptom:** 401 or 403 errors after authentication.

**Cause:** Your GitHub account does not have Copilot enabled, or the
subscription has expired.

**Solution:**
- Check your subscription at
  [github.com/settings/copilot](https://github.com/settings/copilot).
- If using a PAT, ensure the token belongs to an account with Copilot
  access.
- ZeroClaw automatically clears cached tokens on 401/403, so the next
  request will re-trigger the device-flow.

### "Timed out waiting for GitHub authorization"

**Symptom:** The device-flow expires before you complete browser
authorization.

**Solution:**
- You have 15 minutes to complete the authorization.
- Ensure you are visiting the correct URL (`github.com/login/device`) and
  entering the code exactly as shown.
- Check that your browser is logged into the correct GitHub account.

### "GitHub auth failed: access_denied"

**Symptom:** You denied the authorization request in the browser.

**Solution:**
- Re-run the command and approve the authorization this time.

### Token Cache Location

Cached tokens are stored in:
- Linux/macOS: `~/.config/zeroclaw/copilot/`
- Windows: `%APPDATA%\zeroclaw\config\copilot\`

To force re-authentication, delete the files in this directory:

```bash
rm -rf ~/.config/zeroclaw/copilot/
```

## Important Notes

- The Copilot provider uses VS Code's OAuth client ID and editor headers.
  This is the same approach used by LiteLLM, Codex CLI, and other
  third-party Copilot integrations.
- The Copilot token endpoint is private and undocumented. GitHub could
  change or revoke third-party access at any time.
- This provider does not support live model discovery (`zeroclaw doctor`
  will report model probing as skipped for Copilot).

## Related Documentation

- [Providers Reference](../reference/api/providers-reference.md)
- [Getting Started](README.md)
- [Custom Provider Endpoints](../contributing/custom-providers.md)
