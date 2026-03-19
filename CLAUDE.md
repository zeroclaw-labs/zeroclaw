# CLAUDE.md — JhedaiClaw

## Commands

```bash
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test
```

Full pre-PR validation (recommended):

```bash
./dev/ci.sh all
```

Docs-only changes: run markdown lint and link-integrity checks. If touching bootstrap scripts: `bash -n install.sh`.

## Project Snapshot

JhedaiClaw is a Rust-first autonomous agent runtime optimized for performance, efficiency, stability, extensibility, sustainability, and security.

Core architecture is trait-driven and modular. Extend by implementing traits and registering in factory modules.

Key extension points:

- `src/providers/traits.rs` (`Provider`)
- `src/channels/traits.rs` (`Channel`)
- `src/tools/traits.rs` (`Tool`)
- `src/memory/traits.rs` (`Memory`)
- `src/observability/traits.rs` (`Observer`)
- `src/runtime/traits.rs` (`RuntimeAdapter`)
- `src/peripherals/traits.rs` (`Peripheral`) — hardware boards (STM32, RPi GPIO)

## Repository Map

- `src/main.rs` — CLI entrypoint and command routing
- `src/lib.rs` — module exports and shared command enums
- `src/config/` — schema + config loading/merging
- `src/agent/` — orchestration loop
- `src/gateway/` — webhook/gateway server
- `src/security/` — policy, pairing, secret store
- `src/memory/` — markdown/sqlite memory backends + embeddings/vector merge
- `src/providers/` — model providers and resilient wrapper
- `src/channels/` — Telegram/Discord/Slack/etc channels
- `src/tools/` — tool execution surface (shell, file, memory, browser)
- `src/peripherals/` — hardware peripherals (STM32, RPi GPIO)
- `src/runtime/` — runtime adapters (currently native)
- `docs/` — topic-based documentation (setup-guides, reference, ops, security, hardware, contributing, maintainers)
- `.github/` — CI, templates, automation workflows

## Risk Tiers

- **Low risk**: docs/chore/tests-only changes
- **Medium risk**: most `src/**` behavior changes without boundary/security impact
- **High risk**: `src/security/**`, `src/runtime/**`, `src/gateway/**`, `src/tools/**`, `.github/workflows/**`, access-control boundaries

When uncertain, classify as higher risk.

## Workflow

1. **Read before write** — inspect existing module, factory wiring, and adjacent tests before editing.
2. **One concern per PR** — avoid mixed feature+refactor+infra patches.
3. **Implement minimal patch** — no speculative abstractions, no config keys without a concrete use case.
4. **Validate by risk tier** — docs-only: lightweight checks. Code changes: full relevant checks.
5. **Document impact** — update PR notes for behavior, risk, side effects, and rollback.
6. **Queue hygiene** — stacked PR: declare `Depends on #...`. Replacing old PR: declare `Supersedes #...`.

Branch/commit/PR rules:

- Work from a non-`master` branch. Open a PR to `master`; do not push directly.
- Use conventional commit titles. Prefer small PRs (`size: XS/S/M`).
- Follow `.github/pull_request_template.md` fully.
- Never commit secrets, personal data, or real identity information (see `@docs/contributing/pr-discipline.md`).

## Anti-Patterns

- Do not add heavy dependencies for minor convenience.
- Do not silently weaken security policy or access constraints.
- Do not add speculative config/feature flags "just in case".
- Do not mix massive formatting-only changes with functional changes.
- Do not modify unrelated modules "while here".
- Do not bypass failing checks without explicit explanation.
- Do not hide behavior-changing side effects in refactor commits.
- Do not include personal identity or sensitive information in test data, examples, docs, or commits.

## Linked References

- `@docs/contributing/change-playbooks.md` — adding providers, channels, tools, peripherals; security/gateway changes; architecture boundaries
- `@docs/contributing/pr-discipline.md` — privacy rules, superseded-PR attribution/templates, handoff template
- `@docs/contributing/docs-contract.md` — docs system contract, i18n rules, locale parity

## Runtime Patterns

Patrones de operación nativa de JhedaiClaw. Seguir siempre estos patrones — no usar workarounds (archivos .env, scripts intermedios, env vars manuales).

### API Keys y Secrets

- **Patrón correcto**: campo `api_key` en `~/.jhedaiclaw/config.toml`
- **Alternativa**: env var `JHEDAICLAW_API_KEY` (mayor prioridad que config)
- **Nunca**: hardcodear en scripts, archivos .env separados, o historial de shell
- Encriptación disponible: `[secrets] encrypt = true` → formato `enc2:...` (ChaCha20-Poly1305)
- Los cron jobs Shell NO heredan env vars del proceso padre (`cmd.env_clear()`)
- Para pasar vars custom a shell jobs: `autonomy.shell_env_passthrough = ["VAR"]`

### Skills

- Ubicación: `~/.jhedaiclaw/workspace/skills/<nombre>/SKILL.md`
- Se inyectan automáticamente en el system prompt del agente al inicio
- Modo: `[skills] prompt_injection_mode = "full"` (completo) o `"compact"` (lazy)
- Desactivar open skills si no se necesitan: `open_skills_enabled = false`

### MCP Servers

- Configuración: `[[mcp.servers]]` en config.toml (array, NO tablas individuales)
- En Windows: path completo — `command = "C:/Program Files/nodejs/npx.cmd"`
- `deferred_loading = false` para cargar tools completos (el agente los usa directamente)
- Auth de MCPs basados en browser: correr `npx <mcp> auth` antes del agente

### Cron Jobs

- Shell jobs (`cron add <expr> <cmd>`): ejecutan `sh -lc "<cmd>"` — no heredan env vars
- Agent jobs (`cron add <expr> <prompt> --agent`): leen config completo incluyendo `api_key`
- Para pipelines con auth interactiva: script wrapper Shell que hace auth + lanza el agente
- Timezone: `--tz "America/Santiago"` en `cron add`
- Listar: `jhedaiclaw --config-dir <dir> cron list`

### Config Dir en Windows

- Siempre usar: `--config-dir "C:\Users\Lenovo\.jhedaiclaw"`
- Sin el flag, JhedaiClaw carga un directorio temporal en `AppData\Local\Temp`
