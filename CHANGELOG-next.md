# ZeroClaw v0.8.0-beta-2

This is the second beta of the v0.8.0 line, and the largest release since v0.7.5. Its headline is **zerocode** — a brand-new, full-featured terminal UI for running and operating your agents without leaving the terminal. Around it, this release ships the multi-agent runtime and schema V3, a rebuilt Quickstart onboarding flow that works identically across the CLI, zerocode, and the web dashboard, and a deny-with-edit approval mode that lets you rewrite a tool result inline. Hundreds of fixes harden the credential boundary, token accounting, sandboxing, and channel delivery.

Because this beta consolidates two milestones, the **What's New** section is framed twice: everything **since v0.7.5** (the last stable release) and the subset that is new **since v0.8.0-beta-1**. Contributor credits below cover the v0.8.0-beta-1 → beta-2 window.

## Meet zerocode

**zerocode is a complete terminal interface for ZeroClaw** — a standalone binary that connects to a daemon and gives you a five-pane workspace for everything from chatting with an agent to editing config to reading live logs. It speaks to the daemon over a filesystem-permission-gated local socket (Unix domain socket or Windows named pipe) or a remote WSS connection, and it can spin up its own ephemeral daemon if one isn't already running.

What you can do in zerocode:

- **Chat** with any configured agent — streaming responses, an agent picker, an inline approval overlay for supervised tool calls, and a `/toggle-thinking` command to show or hide the model's reasoning.
- **Code** in an ACP (agent-coding) session against any working directory you pick, with syntax-highlighted `file_edit` / `file_write` diffs (tree-sitter via inkjet), absolute line numbers, and a deny-with-edit flow that lets you rewrite a proposed change before it's applied.
- **Configure** the whole daemon from a live config manager: nested navigation, kind-aware editors (enum selects, list editors, masked secret fields), `$EDITOR` integration for long values, fuzzy filtering, and composite editors generated from the wire-level schema — no hardcoded forms.
- **Watch** structured, alias-attributed logs with stacking filters, attribute search, a follow toggle, and a resizable detail view.
- **Operate** from a Dashboard with per-agent status, a one-keystroke daemon reload, connection status with reconnect, and a roster of connected TUIs.

It's built to feel native: full mouse support (selection, scrollbar drag, multi-select, pane cycling), a reusable input bar with soft-wrap and clipboard paste, per-OS key dispatch, locally-configurable themes and keybindings (with presets and chord serialization), and an HMAC-signed session identity that survives reconnects. Strings route through an independent Fluent catalogue, so zerocode is localizable from day one.

### One daemon, as many TUIs as you want

The daemon is the single source of truth; zerocode is just a client. **Open as many zerocode windows as you like against one daemon** — every connected TUI shares the same agents, sessions, and config, and each appears in the others' "Connected TUIs" roster. Those clients can be a mix of local (Unix socket / named pipe) and remote (WSS) connections to the same daemon, so you can drive a long-lived daemon on a server from several terminals at once.

If you launch `zerocode` and no daemon is running, it **spins up its own ephemeral daemon** automatically (`--ephemeral`). That daemon's lifetime follows its clients: it stays up as long as at least one TUI is connected, and when the last one disconnects it waits a short grace period (so a quick reconnect doesn't kill it) and then shuts down on its own — no orphaned background process. A daemon you start yourself (`zeroclaw daemon`) is the opposite: it persists until you stop it, and TUIs come and go against it freely.

### Your settings stay local

zerocode keeps its own client config in `<config_dir>/zerocode-config.toml` — your theme, keybindings, and locale — **completely independent of the daemon's `config.toml`**. It's read locally regardless of what you connect to, so the same preferences apply whether you're driving a local socket session or a remote daemon over WSS. Settings layer `defaults → file → ZEROCODE_* env`, so you can override any of them per-invocation without editing the file.

### How the local socket works

Local connections use a platform-native, permission-gated endpoint — no TCP port, no token:

- **Unix-like (Linux/macOS):** a Unix domain socket at `<data_dir>/daemon.sock`, created with `0600` permissions so only the owning user can connect. On Linux the daemon reads the peer's PID/UID via `SO_PEERCRED` for the connection label.
- **Windows:** a named pipe at `\\.\pipe\zeroclaw-<hash>`, where `<hash>` is derived from the data directory so each install gets its own pipe in the kernel object namespace.

The endpoint is auto-derived from the daemon's `data_dir` (override with `$ZEROCLAW_SOCKET`), and the same JSON-RPC line protocol runs over the local socket and over WSS — remote access is the same surface, just tunneled over TLS.

### Your shell environment comes with you

When zerocode connects over the local socket, it captures your real shell environment and sends it in the handshake. The daemon then **overlays that environment onto any shell subprocess it runs on your behalf** (your `PATH`, `SSH_AUTH_SOCK`, `GPG_TTY`, and the like take precedence over whatever the daemon process inherited). The practical payoff: hardware-backed credentials **just work** — if your `ssh-agent` is fronting a **YubiKey** (or any FIDO/PIV token), an agent that runs `git push` or an SSH command authenticates through your key exactly as if you'd typed the command yourself, with no key material ever stored in the daemon. Because the forwarded variables come straight from your terminal session, vars like `SSH_AUTH_SOCK` reach the subprocess even though they aren't on the daemon's default safe-env list — that's deliberate, and it's why the integration is seamless.

## Highlights

- **zerocode** — a new terminal UI for ZeroClaw. A standalone binary with a five-pane workspace (Chat, Code, Config, Logs, Dashboard) that connects to a local or remote daemon and lets you run agents, edit config, approve tool calls, and read live logs without leaving the terminal. Open as many windows as you want against one daemon — local or over WSS — and if none is running, zerocode spins up an ephemeral one that cleans itself up when you're done. See **Meet zerocode** above.
- **Multi-agent runtime + schema V3**: run several named agents from one daemon, each with its own model provider, risk profile, runtime profile, channels, and memory namespace.
- **Quickstart**, a rebuilt onboarding flow that replaces `onboard`: one backend-authored field shape drives CLI, TUI, and web with no duplicated picker rows, live model catalog, personality-file templates, and an atomic apply.
- **Deny-with-edit approvals**: when a tool call needs approval, you can edit the proposed result inline and hand the edited value back to the agent as the tool result, with the substitution recorded in the audit trail.
- **Filesystem-permission-gated RPC socket** replaces pairing-token auth for local IPC — the socket path is the trust boundary.
- **Internationalization**: CLI and TUI user-facing strings now route through Fluent, with on-disk catalogue loading and per-user locale selection.

## What's New since v0.7.5

### Agent & Runtime

- Multi-agent runtime and schema V3: multiple named agents per daemon, each with independent provider/profile/channel/memory configuration (#6398).
- New `rpc/` dispatch layer with a shared turn executor and a single `Method` enum as the source of truth (#6837).
- `--ephemeral` daemon mode for TUI auto-spawned daemons (#6818).
- Streaming turns are bounded by an idle-timeout freeze guard so a stalled stream can't wedge a session.
- Per-agent `classifier_provider` routes the reply-intent precheck to a cheaper model (#6945); per-agent memory-recall limit is configurable via the runtime profile.
- `MemoryStrategy` trait with a `DefaultMemoryStrategy` for pluggable context loading (#6907).
- Delegation is gated on a shared risk profile and the caller's `delegation_policy`; the advertised roster is filtered to same-profile peers.

### zerocode & the RPC layer

zerocode is covered in depth in **Meet zerocode** above; the daemon-side groundwork that makes it possible:

- A new `rpc/` dispatch layer with a shared turn executor and a single `Method` enum as the source of truth — every RPC method is compiler-checked, no string-literal dispatch (#6837, #6817).
- Filesystem-permission-gated local IPC over a Unix domain socket, with Windows feature parity via named pipes; pairing-token auth removed in favour of the socket as the trust boundary (#6837).
- An `--ephemeral` daemon mode so zerocode can auto-spawn a daemon when none is running (#6818); WSS transport for remote connections; an `file/attach` RPC method with base64 + path modes for inline attachments.
- Shared API types so the gateway, RPC dispatch, and zerocode all read one definition; config-introspection methods that drive the live config manager (#6825).

### Quickstart & Onboarding

- Quickstart lands end-to-end and retires the legacy `onboard` surface — a single shared field-shape API consumed by CLI, TUI, and web with no hardcoded labels.
- Atomic apply for agents, peer-groups, personality files, and skills FTUE; live model picker across all three surfaces; explicit template/scratch/skip choice per personality file.
- CLI rebuilt as a step-by-step checklist; provider/channel picker rows driven from canonical registries; humane failure messages.

### Channels

- New WeCom AI Bot WebSocket channel (#6680); Lark/Feishu approval requests (#6852) and Lark as a cron delivery channel (#6851).
- Selective channel builds — compile only the channels you need and filter the channel list by compiled features (#6866).
- Webhook retry with exponential backoff (#5838); Signal outbound emoji reactions (#6840); separate IMAP/SMTP credentials (#6666); Nextcloud Talk draft-update streaming (#6048).
- `channel_send` tool with a `default_target` (#6665); `message_id` exposed in agent channel context (#6843); honest channel readiness reporting in the gateway (#6985).

### Providers

- GitHub Models (#6445), Morph (#6440), Manifest open-source router (#6268), and atomic-chat local provider (#6513); MiniMax split into Global and China entries (#6758); llama.cpp as a dedicated provider kind (#6417).
- Native extended thinking for Anthropic and Bedrock (#5652); OpenRouter prompt caching (#6008); Codex native Responses tool calls (#6117); Ollama `num_ctx`/`num_predict`/temperature tuning (#6178).

### Tools

- New tools: `file_upload` (#6773), `file_upload_bundle`, `file_download` (#6957), and a `result` mode for `execute_pipeline` (#7009).
- Jira `list_transitions` / `transition_ticket` / `create_ticket` (#6481); Jina AI as a web_search provider (#6833); deferred MCP tools filtered by access policy (#6920); scoped tool elevation for built-in and MCP tools; `git stash push` gains `keep_index` / `paths` / `include_untracked`.

### Security & Approvals

- Deny-with-edit approval variant across `zeroclaw-api`, runtime, channels, and the TUI overlay, with a `ReplaceWith` audit entry (#6820).
- Pairing-token auth removed from the RPC socket transport in favour of filesystem permissions (#6837); `#[secret]` generalized via a `SecretField` trait (#6918).
- Runtime profile now enforced in channel-driven agent paths; Canvas iframe sandbox tightened against token theft via XSS (GHSA-f385-f6h2-3gqj, #6942); bubblewrap sandbox binds `/lib64`/`/lib` conditionally (#6902); Groq API keys detected in the leak scanner (#6812).

### Configuration

- Config get/set accepts snake_case field names (#6837 schema family); `max_image_turns` added to `MultimodalConfig`; lean default channel bundle (#6904).
- Registry-driven installer: `--apps` flag, a sectioned interactive picker that shows all features with defaults pre-checked, and apps discovered from `apps/*/`.

### Web Dashboard

- Tool-approval UI for supervised-mode execution (#6603); minimum-browser floor with an unsupported-browser fallback banner (#6936); websocket steering transcript preserved (#6933); version shown in `/api/status` and the sidebar footer (#6367).

### Observability

- OTel tool spans enriched with `gen_ai.tool.*` semantic-convention attributes (#6009); `--log-llm` payload tracing restored (#6709); recording floor split from terminal display.

### Internationalization

- CLI and TUI strings routed through Fluent with on-disk catalogue loading and per-user locale selection; zerocode ships an independent Fluent catalogue; skill install output localized (#6674).

### Installation & Distribution

- NixOS module + test for `services.zeroclaw.instances` (#6562); Tauri desktop permission onboarding for Linux/Windows (#6710) and a macOS onboarding wizard (#6506); `take_screenshot` / `run_applescript` desktop commands (#6507).

## What's New since v0.8.0-beta-1

Nearly all of the above landed after beta-1. The items that are specifically new in this beta:

- **Delegation hardening**: `delegation_policy` simplified to a mode-only (`allow`/`forbidden`) enum editable in the config UI; delegation gated on shared risk profile; advertised roster filtered to same-profile peers.
- **Installer overhaul**: `--apps` flag and a sectioned Apps/Features/Channels picker, registry-driven from `apps/*/` and the Cargo feature set, with crate defaults pre-checked.
- **Quickstart polish**: agent modal with personality + templates, peer-group selector, Esc-to-go-back through the personality stack, and a cleaner CLI checklist.
- **zerocode reaches feature-complete for beta**: the Config, Code, Chat, Logs, and Dashboard panes are all live, with syntax-highlighted diffs (inkjet/tree-sitter), markdown rendering in chat, locally-configurable themes and keybindings, per-OS key dispatch, an independent Fluent catalogue with a download-from-upstream locale tab, and a `--version` flag that flags daemon-version mismatches.
- **New tools**: `file_download`, `file_upload_bundle`, and `execute_pipeline` result mode; honest gateway channel readiness (#6985); `channel_send` with `default_target` (#6665).
- **CLI Fluent routing** with per-user locale fetch and on-disk catalogue loading.

## Bug Fixes

| Area | Fix |
|---|---|
| Security | Runtime profile enforced on channel-driven agent sessions; Canvas iframe sandbox tightened (GHSA-f385-f6h2-3gqj, #6942); bubblewrap `/lib64`/`/lib` conditional bind (#6902); Groq key leak detection (#6812); device-redirect path policy (#6236) |
| Tokens & cost | Stop double-counting cached input tokens across provider/gateway/dispatch; sum all three Anthropic input buckets per the documented formula; include cached input in TUI context usage; Gemini usage propagated to the cost tracker (#6575) |
| Agent & runtime | Apply SecurityPolicy tool filter in `process_message()` (#6960); resolve runtime-profile budgets when constructing the security policy; recover reaped sessions instead of killing them; stop ACP turns wedging on a stalled token-count write |
| Providers | Preserve provider aliases for Codex OAuth (#6938); Codex subscription auth for OpenAI (#6908); `--prompt` for gemini-cli (#6614); doctor uses configured provider credentials (#6838) |
| Channels | Slack `bot_token` optional and env-loaded at startup (#6287); WeChat context_tokens persisted + tilde expansion (#6238); Matrix duplicate inbound replies dropped (#6306); ignore blank SMTP credential overrides (#6979) |
| Gateway | `/ws/nodes` 404 when nodes disabled + reject unauthenticated combo (#6885); boot-time quickstart URLs include the configured host and port |
| Memory | Tolerate concurrent SQLite schema migrations (#6432); fix a migration guard that missed a missing UNIQUE constraint |
| Windows | Remove manual MANIFEST linker flags fixing CVT1100/LNK1123 (#6987); local IPC parity via named pipes |
| Onboarding | onboard `--help` no longer advertises removed flags; quickstart `expect()` replaced with proper error propagation; deny-with-edit replacement sanitized before reuse |

## Breaking Changes

- **`onboard` is removed in favour of `quickstart`.** The legacy section-by-section wizard and its flags (`--quick`, `--api-key`, `--model-provider`, `--<section>-only`, positional section subcommands) now error. Run `zeroclaw quickstart` instead.
- **Schema V3 (multi-agent).** Configs are auto-migrated from V2; run `zeroclaw config migrate` to write the upgraded file explicitly. Agent-level fields that duplicated runtime-profile settings now resolve through the runtime profile.
- **`zeroclaw-tui` renamed to `zerocode`** across the workspace; the TUI is installed as a standalone app (`cargo install --path apps/zerocode`) rather than a feature of the main binary. The `tui-onboarding` feature is removed.
- **RPC pairing-token auth removed.** Local IPC is gated by filesystem permissions on the socket; remote access uses WSS (#6837).
- **`delegation_policy` is now `{ mode = "allow" | "forbidden" }`** — the previous per-agent allow-list is gone; reachable delegates are determined by shared risk profile.

## Known Limitations

This is a beta. The following are known and will be addressed before the full v0.8.0 release:

- **Daemon resident memory does not fully return to baseline** (#6826). Each open zerocode Code (ACP) or Chat session holds its agent and conversation history in RAM; concurrently held sessions are additive, in practice topping out around ~200 MB. glibc arena fragmentation means resident memory does not fully return to the pre-session baseline even after sessions close. Restarting the daemon reclaims it fully.
- **Daemon restart / reconnect hangs** (#7043). Disconnecting the daemon and reconnecting TUIs across a daemon restart can leave a TUI hung. If this happens, quit and relaunch zerocode.
- **`onboard` is deprecated.** The legacy onboarding command no longer configures anything — invoking it prints a notice pointing at `zeroclaw quickstart`, and any legacy flags error. Use `zeroclaw quickstart` for setup.
- **Shell commands can "poison" a single tool call's TTY.** Certain shell invocations can corrupt the pseudo-terminal for *that one tool call* — garbage character output, an unresponsive command — of the kind `stty sane` would normally clear. It's scoped to the affected tool call only: cancelling and issuing another shell tool call runs clean. Not fixed for the beta period (any shell would fail on such commands).
- **Model-provider fallback is being rewired** (#7059, #6295). All legacy cross-provider fallback behaviors were intentionally removed for the beta. Today, a failing call retries the **same** model and provider three times before it counts as a complete failure; broader routing/fallback is planned before the full release.

## Contributors

Thanks to everyone who contributed between v0.8.0-beta-1 and v0.8.0-beta-2:

@abhinavmathur-atlan
@alexandme
@alex-nax
@Alix-007
@Audacity88
@BernardKuo
@drbparadise
@easyteacher
@FTDGRT
@h03-xydt
@jokemanfire
@JordanTheJet
@kanmars
@kristofferkoch
@locnh-ssid
@mov-xound-glitch
@nixosclaw
@perlowja
@puneetdixit200
@r4mmer
@rareba
@rifuki
@singlerider
@theonlyhennygod
@tidux
@tmigone
@tylerjenningsw
@whtiehack
@XiaoliangWang1991
@yijunyu

---

*Full diff since last stable: `git log v0.7.5..v0.8.0-beta-2 --oneline`*
*Since last beta: `git log v0.8.0-beta-1..v0.8.0-beta-2 --oneline`*
