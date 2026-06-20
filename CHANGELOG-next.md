# ZeroClaw v0.8.1

ZeroClaw v0.8.1 is the first patch release on the v0.8.x line, focused on stabilizing the multi-agent runtime, channels, and provider stack that landed in v0.8.0. It spans 207 commits from 45 contributors since v0.8.0 — heavy on bug fixes (123), with substantial new features (46) including a delete/rename-with-cascade configuration story, Discord slash commands derived from installed skills, an OpenAI-compatible cached-input pricing path, and a Phase 0 agent eval harness.

If you upgraded to v0.8.0 and hit rough edges with channel routing, cron delivery, provider replay, or config save round-trips, this is the release that picks those up.

## Highlights

- **Cascading config edits**: `delete_with_cascade` and `rename_with_cascade` now ripple through agents, providers, channels, model providers, and skill bundles instead of leaving dangling references (#7837, #7838, #7839, #7830, #7840, #7841, #7842, #7785).
- **Discord skill-driven slash commands**: dynamic slash commands generated from installed skills, with typed options, chunked interaction followups, edit/delete/bulk-delete archive sync, and configurable reaction recording (#7490, #7489, #7525, #7526, #7844).
- **[Operator Console redesign](https://docs.zeroclawlabs.ai/master/en/gateway/web-dashboard.html)**: the gateway web UI ships a schema-driven config editor, multi-agent chat, read-only skills browser, and a long list of ported fixes (#7665, #6700).
- **Cached-input pricing for OpenAI-compatible providers**: cost tracking now captures `cached_input_tokens` for any provider that reports them, including OpenRouter system-prompt caching (#7492, #7634).
- **Phase 0 agent eval harness**: deterministic replay-driven evaluation lands as the foundation for the eval story going forward (#7067).
- **Workflow hardening for releases and docs**: a canonical install spec drives every generated installer surface (#7558), gh-pages becomes ephemeral with bounded clone size (#7676), and rustdoc is published once and deduped per locale (#7754).

## What's New

### Multi-agent and config

- Typed `delete_with_cascade` and `rename_with_cascade` for aliased entries thread through agents, channels, model providers, and skill bundles, with a `find_all_references` / `plan_delete` foundation under the hood (#7785, #7837, #7838, #7839, #7830, #7840, #7841, #7842).
- Per-agent delegate roster with cross-profile reach (#7590).
- Type-driven alias-ref pickers and self-declaring config enums in the Config menu, plus declarative section grouping (#7556, #7594).
- Agent eval harness — Phase 0 with deterministic replay (#7067).

### Channels

- **Discord**: dynamic slash commands derived from installed skills, dispatched over the existing Gateway WebSocket, with typed options and chunked interaction followups (#7489, #7490, #7844). Inbound reactions are recorded behind a scope config (#7526). Archive syncs on message edit, delete, and bulk delete (#7525). Gateway intents are derived from config instead of hardcoded (#7524). The internals were split into a module tree behind a structured outgoing payload (#7832), and Discord is now part of the default-channels bundle (#7825).
- **Slack**: backfills thread context on first agent encounter (#6428).
- **Email**: XOAUTH2 auth, observer mode, and read-only IMAP tools (#7021).
- **WhatsApp**: Web media markers are sent natively (#7811).
- **Voice pipeline facade** for channels that want to plug into audio I/O (#7394).

### Providers and runtime

- Cached input token pricing surfaces for OpenAI-compatible providers (#7492); OpenRouter system prompts are cached (#7634).
- NEAR AI Cloud provider added (#6842); Azure OpenAI gains a dedicated `reasoning_effort` wire-up (#7350); `extra_body` is supported on any OpenAI-compatible provider (#7163); custom inference providers can specify `tls_ca_cert_path` (#5797).
- Provider Manifest is re-added under the schema v3 provider system (#7791).
- MCP auto-reconnects on stale session or dropped stream (#7351).
- The three legacy agent turn engines consolidate onto a single `run_tool_call_loop` path (#7540).

### Gateway, web, and CLI

- Inbound webhooks route per channel alias (#7367).
- Web chat input supports slash commands (#7223).
- Agents/providers/channels CRUD lands in the CLI with skill-bundle cascade (#7842).
- Gateway: agent owned-state rename/delete cascade and wiring (#7838, #7841).

### zerocode and security

- New doctor pane in zerocode (#7802); session controls and provider diagnostics improvements (#7515).
- `models list`/`doctor` lists configured models, adds `--check`, and collapses probe rows (#7450).
- Agent posture status command (#7321).
- Shell tools see the current session id via the `ZEROCLAW_SESSION_ID` environment variable (#7813).

### Skills, plugins, and memory

- Background review fork plus the `skill_manage` tool (agentskills.io SKILL.md) (#6667).
- Frontmatter tags surface in the skill editor with a slash-command toggle (#7734).
- Skill-directory loads are cached, content-validated, and audit-correct (#7786).
- Wasmtime dependency and feature flags land for the plugin host (#7429).
- Memory hygiene extends to pruning daily and core DB rows (#7081).
- Runtime memory ops are instrumented with OTel GenAI spans (#7570).

### Build, install, and docs infrastructure

- Canonical install spec + `cargo generate installers` drives `setup.bat`, AUR PKGBUILD, Scoop manifest, `flake.nix`, Dockerfile/Containerfile feature sets, and `dev/ci/docker-tags.toml`, with an Installer Drift CI gate (#7558).
- gh-pages becomes ephemeral with bounded clone size (#7676); rustdoc is published once and deduped per locale, with print pages dropped (#7754).
- macOS and Windows build legs in CI run as `cargo check` to keep the matrix fast (#7669).

### Improvements (refactor)

- Extract duplicate domain/URL validation into shared helpers in `zeroclaw-tools` (#7340).
- `before_llm_call` now takes mutable borrows, simplifying provider implementations (#7667).

### Documentation

- First-party extension architecture guide for contributors (#7788).
- Fluent strings and mdbook translation catalogs refreshed for v0.8.1 (#7939).
- Discussions stewardship and issue intake forms for governance (#7654, #7652).
- API events stream semantics for the gateway (#7474); ACP wire shape pinned to v1 spec (#7595); MCP tool control boundaries clarified (#7473).
- Rootless Debian compose example for the container (#7475); Linux uninstall script in setup docs (#7715); OpenAI Codex over a ChatGPT subscription documented for providers (#7116).
- README installer options synced (#7708) and the providers routing link fixed (#7185).

### Performance

- Rustdoc published once with per-locale asset dedup (#7754).
- WeChat markdown regex compilations cached via `LazyLock` (#7494).
- Skill-directory loads cached with content validation (#7786).

## Bug Fixes

| Area | Fix |
|---|---|
| runtime | Route stdout diagnostics through logs (#7934); replay ACP session history in `session/messages` (#7903); credential redaction moved to the rendering layer (#7826); skip response-cache for `[IMAGE:]` multimodal markers (#7779); honor profile tool iteration limits (#7583); forward trimming budgets through the `agent_turn` wrapper (#7780); auto-include discovered MCP tools in `risk_profile.allowed_tools` (#7547); thread shared `CanvasStore` into WS chat and ACP sessions (#7678); resolve runtime profiles for direct turns (#7792); restore native sandbox hardening (#7683); detect no-progress loops across interleaved tool calls (#7681); redact unquoted base64 credentials (#7660); recover poisoned activated-tool lock reads (#7755); keep pruned tool markers out of live context (#7775); condition anti-narration on `show_tool_calls` (#7722); return no heartbeat runs for zero limit (#7721); route native tool narration to stderr (#7773); unify `SopEngine` construction (#7546); honor empty delegate allowed-tools (#7574); expose deferred MCP tools to delegates (#7608); route service commands to named instances (#7579); suppress skill suggestions for installed tools (#7406); clarify peer send targets (#6993); preserve delegate memory namespace (#7465); inherit ACP session cwd into `spawn_subagent` and `delegate` (#7517). |
| zerocode | Refresh agent picker on Code/Chat tab re-entry (#7834); enforce RPC-only boundary by dropping `zeroclaw-providers` (#7850); grapheme-cluster cursor movement and backspace (#7765); localize config status messages (#7713); avoid hardcoded `vi` editor fallback (#7476); theme markdown body text (#7482); keep Cmd-C separate from quit on macOS (#7477). |
| providers | Normalize Azure OpenAI credentials (#7703); retry email OAuth refresh failures (#7745); stop `reasoning_content` from leaking into response text (#7725); preserve structured Bedrock tool results (#7712); redact reliable error endpoints consistently (#7682); preserve compatible reasoning field name on multi-turn replay (#7423); cache OpenRouter system prompts (#7634); fail loudly for unusable fallback providers (#7419); resolve Gemini OAuth project from object and current-project shapes (#7560); remove dead pre-normalization image trim (#7782); honor configured OpenAI `timeout_secs` (#7504); trace native tool delivery decisions (#7933). |
| channels | Dedupe QQ voice redeliveries (#7648); add per-channel `ack_reactions` override for Lark/Feishu (#7495); make Slack `bot_token` optional and resolve from env (#7726); persist Matrix thread history after first reply (#7705); require IRC mention boundaries (#7710); add clear-session command (#7671); Telegram base URL validation (#7697); stabilize missing email message ids (#7767); make Slack empty-peer-set rejections self-diagnose (#7567); skip reply-intent classifier on direct messages (#7564); WhatsApp as a cron delivery channel (#7562); select WhatsApp Web backend from personal/pairing config (#7565); WhatsApp Web forward quoted media attachments (#7536); telegram delivery prompt no longer discourages tool use (#7438); route cron email delivery through the no-registry fallback (#7561); AMQP `health_check` actually probes the broker (#7596); render web integration category labels (#7662); persist Discord slash reconcile state and read shared stores from `data_dir` (#7784); inject Lark/Feishu cron delivery defaults from chat context (#7448); render web agent's own model on agent chat page (#7381). |
| acp | Surface history-pruner and turn-cancel as visible events (#7684). |
| gateway | Suggest available port on bind conflict (#7801); surface pairing-code recovery when already paired on an alternate port (#7798); fail fast on free-form `ask_user` for the WS approval channel (#7551); route chat workspaces per agent (#7573); point paircode recovery hint at loopback for non-loopback binds (#7207). |
| config | Align serde defaults with struct `Default` to prevent save round-trip loss (#7532); expose global channel settings in dashboard (#7651); trim legacy typed scalar strings (#7714); gate test-only `EnvValueGuard` on `cfg(unix)` (#7777); warn on extra-nested provider alias tables (#7580); redact bearer tokens in file-transfer header maps (#7566). |
| memory | Drop postgres client on OS thread to avoid nested runtime panic (#7668); include recall windows in retrieval cache key (#7656); disable hot response cache at zero capacity (#7704); `purge_agent` resolves alias→id before deleting (#7781). |
| tools | Reject binary/image files in default `file_read` (#7522); fix Claude Code execution on Windows (#7214); reject excessive round precision (#7716); default zero-minute polls (#7707); encode weather location path input (#7717); localize file download tool strings (#7480). |
| skills | Parse skill review summary from tool history (#7730); distinct error when `skill_manage` patch is disabled (#6684); sanitize skill tool names to satisfy provider name regex (#7399); respect `timeout_secs` from `SKILL.toml` (#7552); update background review loop call (#7653). |
| cron | Honor `session_target=main` with stable session path (#7731); correct weekday range normalization for Sunday-alias endpoints (#7208). |
| i18n | Auto-detect system locale when none is configured (#7760); route remaining CLI status output through Fluent (#7499); localize remaining CLI quickstart strings (#7650) and provider selector prompts. |
| install | Detect musl libc for Linux target triple (#7614); install web dashboard on source installs (#7554); validate agent/provider aliases at input time (#7609); bail with a clear error on non-TTY stdin (#7513). |
| update | Accept `.zip` release assets on Windows targets (#7530); use `.tar.gz` for Windows assets in gnu-vs-android test (#7528). |
| security | Create per-agent workspace dir in `for_agent`, plus Android shell support (#7284). |
| docker | Include `zerocode` binary in release Docker images (#7789); add `g++` to web build layer for cc-rs C++ detection (#7534); add `vim-tiny` to runtime images for editor support (#7503). |
| doctor | Surface `Config::collect_warnings` in diagnostics (#7727); ensure workspace doctor checks support multi-agents (#7544). |
| daemon | Allow any configured channel as `heartbeat.target` (#7680). |
| agent | Emit pending `tool_call` at dispatch so live cards render (#7778). |
| sop | Wire audit logger into registered SOP tools (#7593). |
| plugins | Align install/discovery paths and add legacy migration (#7549). |
| whatsapp | `whatsapp_storage` returns `HashState::default()` when no app-state version row exists (#7806). |
| cli | Preserve UTF-8 erase handling (#7672); flag configured channels missing from the binary (#7848). |
| ci | Centralize Node.js version to `.nvmrc` with LTS 24 (#7550); narrow Kilo provider path label (#7812); unbreak master quality gate (#7696); cover Windows path and shell portability in tests (#7906). |
| web | Force `js-yaml >=4.2.0` to patch merge-key DoS (#7761); correct chat model switcher resolution and switch timeout (#7191). |
| docs | Clear rustdoc warnings surfaced by mdbook build (#7751); add `.nojekyll` so Pages serves `_shared/` chrome (#7568). |
| web/integrations | Tighten Nextcloud Talk draft updates (#7572). |

## Breaking Changes

- **`feat(providers)!`: span attribution — model provider calls now route through the attribution dispatcher** (#7748). The dispatcher centralizes per-provider span attribution and is the only sanctioned path for model provider calls; downstream consumers of the provider call API may need to update if they were bypassing it.

## Contributors

@77382104
@alanpjohn
@alexandme
@Alix-007
@andybab
@Audacity88
@bheatwole
@chengzhichao-xydt
@ConYel
@crh-code
@danielO99
@dvgamerr
@dwc1997
@fanchanghu
@IftekharUddin
@ilteoood
@itripn
@JieZeng1993
@jokemanfire
@JordanTheJet
@jstar0
@metalmon
@mn13
@mov-xound-glitch
@Nillth
@NiuBlibing
@perlowja
@Pick-cat
@PierreLeGuen
@rareba
@rezigned
@rifuki
@RyanHoldren
@sbenedetto
@SebConejo
@sentinel-nnet
@silas-qiao
@singlerider
@theonlyhennygod
@tidux
@tmigone
@tw-360vier
@whiteyzy
@xianshishan
@Yyukan

---

**Full diff:** [`v0.8.0...v0.8.1`](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.8.0...v0.8.1)
