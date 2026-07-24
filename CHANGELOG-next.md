# ZeroClaw v0.8.3

This release is a large consolidation cycle spanning **379 commits** from **56 contributors**, focused on the new Standard Operating Procedure (SOP) engine, a WebAssembly plugin host, a Git forge channel, and a broad round of runtime, provider, and security hardening. If you run ZeroClaw agents in production, the headline changes are the procedural-memory/SOP substrate, tighter context-budget accounting, and a wave of SSRF and secret-leak fixes. Desktop and quickstart flows also get meaningful polish.

## Highlights

- **SOP (Standard Operating Procedure) engine** lands end to end — a daemon maintenance tick, typed step contracts, live step execution, cron/filesystem/calendar triggers, and an out-of-band approval plane.
- **WebAssembly plugin host** (wasmtime component model) for tools, channels, and memory, with per-call execution limits, signature-policy enforcement, and registry search/install by name.
- **Git forge channel** with GitHub and Gitea/Forgejo providers, plus a unified `git_forge` tool and SOP ingress.
- **Self-contained desktop app** returns — the kernel now ships as a Tauri sidecar for a Quickstart-first companion experience.
- **Cost & usage accounting** gains a task-attributed usage ledger, offline pricing catalog, and by-period / org-billed views in ZeroCode.
- **Security hardening** across the board: multiple SSRF gaps closed, constant-time token comparison, signing-key leak prevention, path-traversal fixes, and dependency bumps clearing several RUSTSEC advisories.

## What's New

### SOP & Procedural Memory
- A daemon SOP maintenance tick now drives live procedures, executing real steps, enforcing step scope/mode/routing/schemas at the engine boundary, and consuming CAS run claims (#8391, #8399, #8416, #8420, #8430, #8493, #8502, #8506).
- Cron, filesystem, and calendar no-show triggers wire into the maintenance tick (#8400, #8461, #8419).
- An out-of-band approval plane fails closed on timeout, with a priority-based gate fix, and deterministic capability steps now run through a registry that fails closed on driverless steps (#8304, #8724).
- A procedural memory workshop and web visual authoring (experimental) with channel fan-in and a selectable agent were added (#8509, #8590).

### Plugins
- A wasmtime component-model host now backs tool, channel, and memory plugins, with channel host bindings (`wasi:http`, inbound queue, config jail) and a registration API (#8368, #8551).
- Plugins gain per-call execution limits and an FND-001 backend taxonomy, honor the configured signature policy when loading tools, and support registry search plus install-by-name (#8491, #8172, #8264).

### Channels & Git Forge
- New Git forge channel with a GitHub provider and SOP ingress, plus a Gitea/Forgejo provider (#8609, #8611).
- Operators can now bind identities without the `/bind` code round-trip (#8707).
- WhatsApp gains native location-pin support on both backends and passive group context; LINE gets a loading indicator, icon/nickname switching, and bind reply feedback (#8427, #8389, #7768).
- Tool approvals can be routed to a distinct approver channel (#8231).

### Runtime & Memory
- Unified memory-context injection is now keyed on `TurnOrigin` ingress provenance, and a durable memory store seam adds supersede/dedup/budget/policy-gate handling with embedding-identity persistence and automatic vector migration on change (#8619, #8570, #8623).
- Metered provider seams (`ResolvedModelAccess::run_model_query`) now cover the model-query path and the max-iteration graceful summary (#8806, #8821).
- Process RAM/CPU sampling landed on macOS, Windows, and FreeBSD via `sysinfo`, and a model-context-window bar was added to the ZeroCode TUI, gateway agent chat, and interactive CLI (#8802, #7946).
- Goal task storage foundation and a configurable native runtime shell were added (#8685, #8311).

### ZeroCode (Desktop / TUI)
- A TodoWrite tracker (RPC + ACP + durable persistence) and cron run history/trigger were added, along with a Cost tab with by-period and org-billed views (#8639, #7905, #8483).
- ACP multiple-choice elicitation now uses `elicitation/create` when the client advertises form support, with single- and multi-select prompts rendered in the ZeroCode Code tab (#8338).
- You can now choose a saved Code session on entry, switch agents in active sessions, and use ctrl-w word delete (#8922, #8477, #8774).
- The self-contained desktop app is reintroduced as a Quickstart-first companion with a bundled kernel sidecar (#8565, #8708).
- Quickstart supports subscription authentication modes and inline CLI subscription auth, while release workflows publish self-contained desktop installers for macOS, Linux, and Windows (#8980, #8981, #8709).

### Providers, Tools & Cost
- Provider requests now thread `provider_timeout_secs` and `extra_headers` through the responses path (#8229).
- A Bocha AI web-search provider was added, and `browser_open` now allows `http://` URLs and `allowed_private_hosts` opt-in (#8737, #8136, #8171).
- Cost tracking gains a task-attributed usage ledger, offline pricing catalog, live-gateway price backfill for unpriced models, and cost/org snapshot plus windowed cost/query RPCs (#8686, #8380, #8233, #8482).
- MCP gains resources-as-context, pinning, named-prompt rendering, and a policy-gated resource/prompt client surface (#8508, #8403).

### Gateway, Config & Skills
- The gateway adds default HTTP security response headers and agent-aware `/api/tools` listing with an agent-scoped tool picker (#8829, #8331).
- Config adds independent delegate targets, a `local_small` runtime preset, and `x-required-by-transport` metadata for MCP servers (#8239, #8531, #8349).
- The web config form renders a risk profile's four tool-permission lists (`allowed_tools`/`excluded_tools`/`auto_approve`/`always_ask`) as one authorization-and-approval grid that honors the profile's autonomy level, surfacing the approval settings as stored overrides under `full`/`readonly` where they no longer drive prompts (#8879).
- Skills install/list/remove are now bundle-aware, surface security-audit-skipped skills, and support an opt-in bounded SKILL.md reflection for skill creation (#8335, #8699, #8261).
- Observability adds a runtime OpenTelemetry content policy for LLM/tool I/O and a rotating log-persistence mode (#8567, #8307).
- Per-turn output routing via `send_via` with voice-delivery fixes landed (#7361).

### Install, Release & Supply Chain
- Standard prebuilts remain on the lean supported channel set, correcting an unreleased broadening introduced after v0.8.2; target-specific Android and ARM exclusions are now resolved centrally, and `install.sh --full` remains available for the broader source-build surface (#9051, #8566).
- Release automation adds CycloneDX SBOM generation, cosign signing, SLSA provenance, and self-contained desktop installers (#8158, #8404, #8277, #8709).
- Release verification Markdown is escaped before it reaches GitHub workflow output (#9031).

### Improvements
- Tool assembly across the runtime (agent creation, independent delegates, `process_message`, and `loop_::run`) was routed through a single `ScopedToolRegistry` seam (#8711, #8744, #8701, #8700).
- MCP prompt-section composition is now owned at the `ScopedAssembled` boundary, and the orchestrator turn routes back through `ResolvedAgentExecution::resolve` (#8812, #8629).
- Performance: JSONL fsync moved off the async hot path, the web-search tag-strip regex is cached in a `LazyLock`, and the orchestrator notify channel is bounded with capped path/URL bodies (#8439, #8350, #8460).
- Windows builds now statically link the MSVC CRT, and prebuilt Docker image variants were consolidated with an added arm64 target (#8604, #8485, #5187).

### Documentation
- SOP fan-in usage docs, an autolinked ACP elicitation RFD, and repaired SOP fan-in snippet links were added (#8521, #8498, #8595).

## Bug Fixes

| Area | Fix |
|---|---|
| Runtime | Enforce context budget against provider-reported tokens; enforce leading user-turn invariant before dispatch; strip orphaned `tool_use` on max-iterations exit (#8840, #8696, #7865) |
| Runtime | Arc-share tool schemas to stop per-iteration clone churn; hot-reload log-persistence config; thread `agent_alias` into `agent_turn`'s ToolLoop (#8817, #8816, #8921) |
| Providers | Guard SSE parsers against EOF-as-success truncation; omit `tool_choice`/empty tool-call content for empty tool lists; clean Anthropic tool schemas before native serialization (#8663, #8667, #8524, #7961) |
| Providers | Distinguish missing vs expired OpenAI Codex credentials; prefer `chatgpt_account_id` claim in Codex JWT extraction; cool down rate-limited fallback entries (#8029, #8002, #8317) |
| Security | Close SSRF gaps in Matrix marker URLs, text_browser, and skill_http userinfo; harden WeChat attachment path against traversal (#8657, #8635, #8658, #8628) |
| Security | Constant-time `nodes.auth_token` comparison; reject empty bearer token; prevent signing-key leak via `VarError`; scan link/image destinations for credential patterns (#8824, #8727, #8591, #8906) |
| Config | Protect runtime state files and real `config.toml` from agent self-modification; auto-materialize new map aliases in config patch (#8660, #8606, #8842) |
| Channels | Localize channel runtime replies; use resolved agent config for `strict_tool_parsing`/`parallel_tools`; serialize per-sender session persistence to prevent races (#8769, #7836, #7847) |
| ZeroCode | Fix intermittent `ask_user` failures under ACP elicitation; use runtime-profile `max_context_tokens` for context meter; strip markdown fences from code-block copy (#8773, #8872, #8777) |
| Memory | Refresh embedder on config change; resolve dotted embedding provider refs; make `SqliteMemory: Clone` valid by sharing one embedder lock (#8625, #8152, #8868) |
| Gateway | Propagate pairing DB errors instead of panic; advertise A2A cards on the runtime port; exclude env-overridden secrets from reload drift (#8466, #8538, #8704) |
| Cost | Atomic ledger appends with concatenated-record recovery; observability CLI one-shot no longer loses telemetry/token totals on exit (#8412, #8146) |
| Deps | Bump crossbeam-epoch (RUSTSEC-2026-0204), anyhow (RUSTSEC-2026-0190), and remove rag-pdf/ttf-parser (RUSTSEC-2026-0192) (#8783, #8500, #8547) |
| Install | Prebuild dashboard for embedded web; exclude Tauri apps from `--full` app sweep; register `zerocode.exe` in the Scoop manifest (#8643, #8786, #8276) |
| Tools | Pin `http_request` to vetted DNS addresses; cap calculator values array to prevent OOM; bound `browser_open` launcher waits (#7902, #8481, #8564) |

## Breaking Changes

- **Rust toolchain floor**: the workspace MSRV is now Rust 1.96.1, with CI, containers, and documentation aligned to that version (#8801).
- **Removed the built-in ClawHub skill-install source** (`zeroclaw skills install clawhub:<slug>` and `clawhub.ai` URLs). Install skills from a local path, a Git URL (optionally `<git-url> --skill <name>` to select one skill from a catalog repo), or a registry name instead. SkillForge's default discovery sources no longer include the never-implemented `clawhub` source (#8638).

## Contributors

@alexandme
@Alix-007
@alteckclub
@Audacity88
@bheatwole
@CedricConday
@chengzhichao-xydt
@ConYel
@crh-code
@databillm
@drbparadise
@dvgamerr
@eugeneb50
@FTDGRT
@hanZeng-08
@HonorVanEr
@IftekharUddin
@initiallyqq
@jhheider
@jokewithme110
@JordanTheJet
@Leon-SK668
@Leuca
@LiLan0125
@mazhuima
@mov-xound-glitch
@Nillth
@NiuBlibing
@octo-patch
@OmkumarSolanki
@ozpool
@perlowja
@Pick-cat
@piiiico
@Project516
@rifuki
@ryanlee486
@SimianAstronaut7
@singlerider
@sonytricoire
@Stealinglight
@Super-Cabbage
@Taswen
@theonlyhennygod
@theredspoon
@thunderjr
@tidux
@tzy-17
@vrurg
@wangmiao0668000666
@WeeLi-009
@xydt-juyaohui
@yanchenko
@yuxuan-7814
@ZOOWH
@zverozabr

## Full Changelog

**Full diff:** https://github.com/zeroclaw-labs/zeroclaw/compare/v0.8.2...v0.8.3
