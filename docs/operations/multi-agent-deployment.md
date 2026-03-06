# Multi-Agent Deployment Guide

**Status:** Implementation Guide
**Last Updated:** 2026-03-06
**Version:** 1.0.0

---

## Overview

This guide covers deploying ZeroClaw with multi-agent capabilities enabled. Multi-agent mode allows coordinated task execution across specialized worker agents.

---

## Prerequisites

- ZeroClaw binary (built with `--release`)
- Docker (optional, for containerized deployment)
- LLM provider API key (OpenRouter, OpenAI, Anthropic, or Ollama)

---

## Deployment Modes

### 1. Single-Process Multi-Agent

All agents run within a single ZeroClaw process. Suitable for:

- Development and testing
- Low-volume workloads
- Single-machine deployments

**Configuration:**

```toml
# config.toml
[coordination]
enabled = true
max_concurrent_agents = 5
agent_timeout_seconds = 300
```

### 2. Docker Compose Deployment

Multi-service deployment with separate worker containers.

```bash
# Start coordinator + workers
docker compose --profile multi-agent up -d
```

**Services:**

| Service | Description |
|---------|-------------|
| `zeroclaw` | Main coordinator + gateway |
| `zeroclaw-worker` | Worker agent processes |

### 3. Distributed Deployment

Separate machines for coordinator and workers.

**Coordinator:**

```bash
zeroclaw gateway \
  --coordination-enabled \
  --coordinator-mode main
```

**Worker:**

```bash
zeroclaw agent run \
  --agent-mode worker \
  --coordinator-endpoint http://coordinator.example.com:42617
```

---

## Configuration Reference

### Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `ZEROCLAW_AGENTS_DIR` | Path to agent definitions | `/zeroclaw-data/agents` |
| `ZEROCLAW_TEAMS_DIR` | Path to team definitions | `/zeroclaw-data/teams` |
| `ZEROCLAW_AGENT_MODE` | Agent mode (`main` or `worker`) | `main` |
| `ZEROCLAW_COORDINATOR_ENDPOINT` | Worker coordinator endpoint | `http://localhost:42617` |
| `ZEROCLAW_MAX_CONCURRENT_AGENTS` | Max concurrent workers | `5` |

### Agent Definition Files

Place in `$ZEROCLAW_AGENTS_DIR`:

```toml
# agents/researcher.toml
[agent]
id = "researcher"
name = "Research Agent"
version = "1.0.0"
description = "Conducts web research"

[execution]
mode = "subprocess"

[provider]
name = "openrouter"
model = "anthropic/claude-sonnet-4-6"

[tools.tools]
name = "web_search"
enabled = true
```

### Team Definition Files

Place in `$ZEROCLAW_TEAMS_DIR`:

```toml
# teams/research-team.toml
[team]
name = "research-team"
description = "Research and analysis team"

[[team.members]]
id = "researcher"
role = "lead"

[[team.members]]
id = "analyst"
role = "member"

[team.coordination]
max_parallel = 3
timeout_seconds = 600
```

---

## CI/CD Integration

### Multi-Agent Test Workflow

ZeroClaw includes a dedicated CI workflow for multi-agent features:

- **File:** `.github/workflows/test-multi-agent.yml`
- **Triggers:** Changes to `src/agent/**`, `src/tools/delegate.rs`, agent test files
- **Jobs:**
  - Unit tests (`agent::*` modules)
  - CLI tests (`zeroclaw agent *` commands)
  - Integration tests (agent E2E)
  - Registry tests (agent discovery/loading)
  - Team tests (team orchestration)
  - Docker tests (container agent execution)

### Running Multi-Agent Tests Locally

```bash
# Run all multi-agent tests
cargo test --test cli_agent_tests --test agent_e2e

# Run specific agent module tests
cargo test --package zeroclaw --lib agent::registry

# Run with verbose output
cargo test --test cli_agent_tests -- --nocapture --test-threads=1
```

---

## Docker Deployment

### Build with Multi-Agent Support

```bash
docker build \
  --build-arg ZEROCLAW_CARGO_FEATURES="" \
  --target release \
  -t zeroclaw:multi-agent \
  -f Dockerfile .
```

### Verify Multi-Agent Support

```bash
docker run --rm zeroclaw:multi-agent zeroclaw agent list
docker run --rm zeroclaw:multi-agent zeroclaw agent --help
```

### Docker Compose with Workers

```bash
# Start coordinator + 3 workers
docker compose up -d --scale zeroclaw-worker=3
```

---

## Health Monitoring

### Check Agent Status

```bash
# List all available agents
zeroclaw agent list

# Show specific agent details
zeroclaw agent show researcher

# Validate agent configuration
zeroclaw agent validate researcher
```

### Coordinator Health

```bash
# Check coordinator status
zeroclaw status

# Gateway health endpoint
curl http://localhost:42617/health
```

---

## Troubleshooting

### Agent Not Found

**Symptom:** `zeroclaw agent show <id>` returns "not found"

**Solution:**
1. Verify agent file exists in `$ZEROCLAW_AGENTS_DIR`
2. Run `zeroclaw agent reload` to refresh registry
3. Check file syntax: `zeroclaw agent validate <id>`

### Worker Cannot Connect

**Symptom:** Worker exits with "connection refused"

**Solution:**
1. Verify coordinator endpoint is reachable
2. Check `ZEROCLAW_COORDINATOR_ENDPOINT` environment variable
3. Ensure coordinator is running: `zeroclaw status`

### Timeout Errors

**Symptom:** Agent tasks time out after default duration

**Solution:**
1. Increase timeout in agent definition: `timeout_seconds = 600`
2. Override with `--timeout` flag: `zeroclaw agent run --timeout 600`
3. Check for resource constraints on workers

---

## Security Considerations

### Agent Sandboxing

| Execution Mode | Isolation | Use Case |
|----------------|-----------|----------|
| `subprocess` | Process-level | Trusted agents |
| `docker` | Container | Untrusted, file operations |
| `wasm` | Memory-only | High security needs |

### Permission Model

```toml
# Agent-specific permissions
[permissions]
network = true
allowed_domains = ["api.github.com", "crates.io"]
file_scope = "workspace"
max_execution_seconds = 300
```

### Secret Management

- Never commit API keys to agent definition files
- Use environment variables for sensitive values
- Rotate worker credentials regularly

---

## Performance Tuning

### Concurrent Agent Limits

```toml
[coordination]
max_concurrent_agents = 10
queue_size = 100
spawn_timeout_seconds = 30
```

### Resource Allocation

```yaml
# docker-compose.yml
deploy:
  resources:
    limits:
      cpus: '2'
      memory: 2G
    reservations:
      cpus: '0.5'
      memory: 512M
```

---

## Migration from Single-Agent

### Update Configuration

1. Add `[coordination]` section to `config.toml`
2. Create `$ZEROCLAW_AGENTS_DIR` and move agent definitions
3. Set `ZEROCLAW_AGENTS_DIR` environment variable

### Verify Migration

```bash
# Test agent loading
zeroclaw agent list

# Run simple agent task
zeroclaw agent run --agent-id researcher "test task"
```

---

## Rollback Procedure

If multi-agent deployment causes issues:

1. **Disable coordination:**
   ```bash
   # Set in config.toml
   [coordination]
   enabled = false
   ```

2. **Remove worker containers:**
   ```bash
   docker compose --profile multi-agent down
   docker compose up -d  # restart without workers
   ```

3. **Restore single-agent mode:**
   ```bash
   unset ZEROCLAW_AGENTS_DIR
   unset ZEROCLAW_TEAMS_DIR
   ```

---

## References

- [Multi-Agent Architecture Design](../project/multi-agent-file-based-architecture.md)
- [Commands Reference](../commands-reference.md)
- [CI/CD Map](../ci-map.md)
