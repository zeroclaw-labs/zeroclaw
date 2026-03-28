# How-To: Integrating OpenClaw Workflows into ZeroClaw

This guide explains how to map common patterns from [Awesome OpenClaw Examples](https://github.com/OthmaneBlial/awesome-openclaw-examples) to ZeroClaw's architecture.

## Architecture Mapping

| OpenClaw Concept | ZeroClaw Equivalent |
| --- | --- |
| `cron` setup | `zeroclaw cron` (built-in scheduler) |
| `ClawHub` skills | `SkillForge` discovery + `skills/` directory |
| `prompts/` | `agent` subcommand with `--prompt` or `--message` |
| `scripts/` | `shell` tool or `zeroclaw cron add-shell` |

## Example: Migrating "PR Radar" (Workflow #01)

The "PR Radar" workflow identifies blocked PRs and summarizes them.

### 1. Discovery & Skill Integration
OpenClaw uses a GitHub skill. ZeroClaw can discover this via SkillForge:
```bash
# If SkillForge is enabled in config.toml
zeroclaw skillforge scout --source github --query "github-pr-radar"
```
Or simply use the built-in `git_operations` and `github` tools.

### 2. Scheduling the Task
In OpenClaw, you'd set up a system-level cron. In ZeroClaw, use the internal scheduler:

```bash
# Schedule a daily PR summary at 9 AM
zeroclaw cron add "0 9 * * * " \
  "agent -m 'Analyze all open PRs in the current repo. Identify which are blocked by failed CI or missing reviews and summarize the action queue.'" \
  --name "pr-radar" \
  --job-type agent
```

## Example: Migrating "SLA Guardian" (Workflow #02)

This workflow monitors support threads for SLA breaches.

### 1. Configure the Target
ZeroClaw can deliver reports to different channels (Slack, Discord, Telegram, Mattermost).

```toml
# config.toml
[[cron.jobs]]
name = "sla-guardian"
schedule = "0 * * * *" # Every hour
job_type = "agent"
prompt = "Check the #support channel for unresolved threads older than 4 hours. Summarize and alert the @oncall-team."
delivery = { mode = "channel", channel = "support-alerts" }
```

### 2. Run the Daemon
To execute these scheduled tasks, ensure the ZeroClaw daemon is running:
```bash
zeroclaw daemon
```

## Self-Modifying Integration Strategy

OpenClaw workflows often require specific environment variables or API keys. ZeroClaw's self-modifying capability allows the agent to handle this dynamically:

1. **Step-by-Step Execution**: Start with a shell-based cron that fails if dependencies are missing.
2. **Autonomous Repair**: The agent sees the failure, researches the fix (e.g., "Install gh-cli"), and uses the `shell` tool to install it or `file_edit` to add the missing API key to `config.toml`.
3. **Validation**: The agent reruns the task and confirms success.

## Recommended "Quick Wins" for ZeroClaw Users

1. **CI Flake Doctor**: Use `zeroclaw cron` to run `agent -m "Analyze recent GitHub Actions runs for flaky failures and create a todo list."`
2. **Weekly Research Digest**: Use the `web_search` and `http_request` tools within an agent cron to summarize industry news every Friday.
3. **Model Cost Command Center**: Use the `cost` tools in ZeroClaw to monitor usage across providers (OpenCode, Gemini, Claude).

## Summary
ZeroClaw provides a more unified, Rust-native way to execute "Awesome" workflows by bringing the scheduler, the agent, and the tools into a single binary. Most OpenClaw examples can be ported by simply converting the `scripts/` logic into an `agent` prompt and adding it to `zeroclaw cron`.
