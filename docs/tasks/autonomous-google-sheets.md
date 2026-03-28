# How-To: Autonomous Google Sheets Analysis (Self-Modifying Workflow)

This guide explains how to use ZeroClaw in a self-modifying "learning" mode where the agent identifies missing configuration (like allowed domains or credentials) and guides you through adding them during a task.

## Prerequisites

- ZeroClaw installed.
- **Supervised Mode** enabled (recommended) to see the agent's reasoning.
- `file_edit` and `http_request` tools available.

## 1. The Autonomous Strategy

Instead of pre-configuring everything, you can start with a broad task. ZeroClaw is designed to fail gracefully, report the specific reason for failure (e.g., "Domain not allowed"), and then suggest the fix.

### Example Workflow:

1. **User Request**: "Analyze this sheet: https://docs.google.com/spreadsheets/d/ID/export?format=csv"
2. **Agent Failure**: The `http_request` tool returns: `Error: Host 'docs.google.com' is not in http_request.allowed_domains`.
3. **Agent Learning**: The agent identifies it needs to modify `~/.zeroclaw/config.toml`.
4. **Agent Action**: It uses `file_read` to see your current config, then `file_edit` to add the domain.
5. **User Approval**: You approve the `file_edit` (in Supervised mode).
6. **Task Resumption**: The agent retries the download and succeeds.

## 2. Guide ZeroClaw through "Just-In-Time" Onboarding

If the agent doesn't automatically suggest the fix, you can prompt it to be autonomous:

**Prompt:**
> "I want you to download and analyze this Google Sheet: [URL]. If you encounter any configuration blocks (like blocked domains or missing API keys), do not just stop. Read my `config.toml`, identify the missing setting, and suggest a `file_edit` to fix it. If you need a secret key I don't have in config, ask me for it."

## 3. Handling Credentials Dynamically

For private sheets that require an API Key or OAuth:

1. **Agent identifies the need**: It sees a `401 Unauthorized` or `403 Forbidden`.
2. **Agent requests input**: "I need a Google API Key to access this private sheet. Please provide it, and I will add it to your `config.toml` secrets section."
3. **User provides key**: You provide the key in the chat.
4. **Agent persists key**: The agent uses `file_edit` to update the `secrets` or `http_request.headers` section.

## 4. Implementation Example (The "Self-Modifying" Prompt)

Use this "meta-prompt" to trigger the self-learning behavior for the Google Sheets task:

```text
Target: https://docs.google.com/spreadsheets/d/1La4FNw8tM3nHVcwoG-D0YmAm2LRWdZtH8e4pXt011Uo/export?format=csv

Task:
1. Attempt to download the target CSV.
2. If blocked by 'allowed_domains', read my config.toml, use file_edit to add 'docs.google.com', and retry.
3. If you get a 403, tell me exactly what header or key is missing.
4. Once downloaded, save to 'sheet.csv' and summarize the data.
```

## Why this works
ZeroClaw's "Self-Modifying" reputation comes from its ability to use its own file manipulation tools (`file_read`, `file_edit`) on its own configuration files. By giving the agent permission to "fix itself," you create a loop where the environment evolves to meet the task requirements.

## Troubleshooting the "Learning" Loop

- **Permission Denied**: Ensure `config.toml` is not write-protected.
- **Infinite Loops**: If the agent keeps trying the same failing edit, intervene and tell it to "Stop and research why the previous edit didn't take effect."
- **Security**: Always review `file_edit` calls on your `config.toml` to ensure the agent isn't accidentally opening up too many permissions (e.g., adding `*` to `allowed_domains`).
