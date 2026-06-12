# Unreleased

## Runtime: one turn engine for every transport (#7415)

The three agent turn engines are consolidated onto `run_tool_call_loop`, now housed in `crates/zeroclaw-runtime/src/agent/turn/` (one file per step). Channels, CLI, cron, delegate, and SubAgent paths are unchanged — they already ran on it. The streaming paths (gateway WebSocket, RPC/zerocode, ACP — via `Agent::turn_streamed*`) and the embedded `Agent::turn` path are rebuilt as thin wrappers over the same loop.

Behavior changes on the streaming and embedded `Agent` paths:

- **Gained from the shared engine**: malformed tool-protocol retry, context-overflow recovery, per-iteration history maintenance and pruning, and the `fire_llm_input` hook.
- **Task-local scoping is now uniform**: `TOOL_LOOP_THREAD_ID`, `SESSION_KEY`, and `TOOL_CHOICE_OVERRIDE` are scoped around every entry path, closing the `TOOL_CHOICE_OVERRIDE` provider leak on the streaming/embedded paths.
- **Deliberately preserved**: tool-call dedup stays **off** on these paths (RPC relies on idempotent retries); `Agent::turn` still **errors at the iteration cap** (an embedder control signal); pattern-based loop detection and `step_timeout` pacing stay off here — the machinery is shared, but these paths keep their existing contracts (an embedder's identical-args tool chain completes; model text with tool-call-looking content is returned verbatim when no tools are in play).
- **One known granularity change**: mid-turn steering messages on the WebSocket path are now folded in **between tool-call rounds** (a round ends when the model produces a tool-free response) rather than before every tool iteration — the wrapper owns steering so each accepted message persists as its own enriched user turn. A steering message sent mid-tool-chain takes effect when the chain completes; cancellation remains responsive at every iteration. The loop has an iteration-level steering drain for a follow-up that restores the old granularity.
- **Wire schema unchanged**: `TurnEvent`, `StreamDelta`, and the channel/gateway/ACP message shapes are untouched.

# ZeroClaw v0.8.0

ZeroClaw v0.8.0 is the big one. One daemon now runs many named agents, each with its own workspace, memory, model provider, security policy, channels, and personality, coordinated by a rewritten configuration schema that migrates your existing setup automatically. The release spans 439 commits (not including the squashes) from over 100 contributors since v0.7.5, and also introduces the zerocode terminal UI, a unified logging and attribution pipeline, and a hardened security posture across channels, tools, and the gateway.

With the big sweeping code changes behind us, expect the v0.8.x series to ship at a more regular pace from here.

## Highlights

- **[Multi-agent runtime](https://docs.zeroclawlabs.ai/master/en/agents/overview.html)**: run any number of named agents from one daemon, each with isolated workspace, memory, skills, model provider, and risk profile. Channels, cron jobs, webhooks, and ACP sessions all dispatch to a specific agent.
- **Schema V3 with automatic migration**: existing configs migrate in place on first load. Provider entries support multiple named aliases per provider, and a single malformed entry no longer wipes its neighbors.
- **[zerocode](https://docs.zeroclawlabs.ai/master/en/zerocode/overview.html)**: a new terminal UI with dedicated Chat, Code, Dashboard, Config, Quickstart, and Logs panes. **One daemon, many zerocode TUIs:** attach from as many terminals as you want, each with its own identity and sessions. The Code pane speaks ACP with no configuration required.
- **[Security hardening](https://docs.zeroclawlabs.ai/master/en/security/overview.html)**: per-agent tool allowlists enforced at every dispatch path, bearer-token revocation on device rotation and deletion, secret redaction extended to nested config shapes, private-host allowlists for outbound HTTP tools, and a fix for a Canvas iframe token-theft advisory (GHSA-f385-f6h2-3gqj).
- **[Observability rework](https://docs.zeroclawlabs.ai/master/en/ops/observability.html)**: a unified structured logging pipeline carries agent, model, and session attribution through every span, streaming to a new Logs page in the dashboard and a gateway logs endpoint.
- **[Lean default channel bundle](https://docs.zeroclawlabs.ai/master/en/channels/overview.html)**: prebuilt binaries now ship the core channels by default with the rest available as opt-in build features. Check the docs if a channel you use is missing after upgrade.

## Since v0.7.5

### Multi-agent (the star of the show)

You can now run a [whole team of agents](https://docs.zeroclawlabs.ai/master/en/agents/overview.html) from a single daemon (#6398): a coding agent with full shell access in one workspace, a locked-down family assistant on Telegram in another, a research agent with its own memory and a cheaper model in a third. Each agent is a first-class identity through the entire stack:

- **Per-agent everything**: [workspace directories](https://docs.zeroclawlabs.ai/master/en/agents/filesystem.html), memory stores with per-agent isolation across the SQLite, Postgres, and Qdrant backends, skill bundles, persona files, and per-agent model-provider resolution in every chat path.
- **[Channels](https://docs.zeroclawlabs.ai/master/en/channels/overview.html)**: each channel binds to an owning agent, with per-agent runtime contexts, structured session routing, identity injection so an agent knows who it is on each channel, and media uploads routed into the owning agent's workspace.
- **[Security policy per agent](https://docs.zeroclawlabs.ai/master/en/security/autonomy.html)**: each agent's policy can allow or exclude individual tools, enforced at the dispatch site; every risk-profile setting now propagates into the live policy; SubAgents are depth-capped and gated by the parent agent's risk profile.
- **Cron**: scheduled jobs are bound to a specific agent with delivery-channel scoping, and the old implicit default-agent fallback is gone.
- **[ACP](https://docs.zeroclawlabs.ai/master/en/channels/acp.html)**: sessions select an agent explicitly, auto-select when only one is configured, respect configurable session limits, and the bridge accepts a custom config location.
- **[Dashboard](https://docs.zeroclawlabs.ai/master/en/gateway/web-dashboard.html)**: a multi-agent rework with per-agent status and memory views, RAM and CPU widgets, a Memories tab, and a sandboxed per-agent workspace file explorer.
- **[Cost tracking](https://docs.zeroclawlabs.ai/master/en/ops/cost-tracking.html)**: rates are organized per provider and model, cached-input tokens are captured, and the dashboard shows per-agent and per-model cost splits with an editable rate sheet.

### zerocode terminal UI

v0.8.0 ships [zerocode](https://docs.zeroclawlabs.ai/master/en/zerocode/overview.html), a full terminal UI for ZeroClaw (#6848). It talks to the daemon over a new RPC transport with [persistent sessions](https://docs.zeroclawlabs.ai/master/en/zerocode/running.html) (#7182) and rides out daemon restarts by reconnecting in place (#7158).

#### Connection: local socket or remote WSS

Locally, zerocode connects over a Unix domain socket (a named pipe on Windows) with no setup at all. For a daemon on another machine, a [TLS WebSocket transport](https://docs.zeroclawlabs.ai/master/en/zerocode/remote.html) connects your workstation to a Raspberry Pi, home server, or VPS with a self-signed certificate and a token. **One daemon serves many zerocode instances at once:** each connecting TUI gets its own signed identity and environment snapshot in the daemon's registry, sessions are tmux-style persistent (#7182) so closing your laptop and reconnecting an hour later from a different network finds your session intact.

#### Shell environment pass-through

A daemon started as a service has a stripped-down environment; your terminal has the real one. On a local socket connection, zerocode [forwards its full shell environment](https://docs.zeroclawlabs.ai/master/en/zerocode/environment.html) to the daemon at handshake, and every shell subprocess your agents spawn gets it overlaid on top of the daemon's safe baseline, with zerocode's values winning on conflict. Your PATH, SSH agent socket, and credential helpers just work, no configuration required.

#### Themes

[Named color themes](https://docs.zeroclawlabs.ai/master/en/zerocode/themes.html) with per-agent overrides in the Code and Chat panes, inline palette previews before you apply, automatic adaptation to your terminal's color depth, and a `terminal` theme that inherits your shell's own colors (#7249).

#### Keybindings

Four built-in presets (default, vim, emacs, arrows-only) plus per-action rebinding through an in-app capture modal. Changes apply live, no restart. **Press `?` on any pane for context-aware help showing the bindings that apply right where you are.**

#### Chat

Conversational sessions with any of your agents: full memory, skills, channel-style context, live model and provider switching mid-conversation (#7209), and an outbound message queue so you can keep typing while a turn is in flight (#7190).

#### Code

A coding workspace built on [ACP](https://docs.zeroclawlabs.ai/master/en/channels/acp.html), the same protocol that powers editor integrations, with zero configuration required: open the pane and it just works. Code sessions are tuned for the job: memory tools are excluded server-side (#7177) so your coding agent stays focused on the tree in front of it instead of consolidating chatter, sessions persist and survive kills cleanly (#7258), and the working directory, branch, and commit are always visible (#7159).

#### Dashboard

Live per-agent status with RAM and CPU for the daemon process, distinguishing loading, error, and live states at a glance.

#### Config

The entire configuration in a split-pane editor with built-in, registry-driven help for every field. Edit agents, providers, channels, and policies without touching a text editor.

#### Quickstart

Guided agent creation end to end: pick a provider, paste a key, choose a model from the live catalog, set a personality, and land directly in Chat with your new agent.

#### Logs

The daemon's structured log stream, filterable and attribution-aware, in the same terminal you work in.

### Providers

- [New providers](https://docs.zeroclawlabs.ai/master/en/providers/catalog.html): Kilo AI Gateway, GitHub Models (#6445), Morph (#6440), Manifest (#6268), atomic-chat (#6513), a dedicated llama.cpp provider (#6417), seven more OpenAI-compatible providers (#7260), and MiniMax split into Global and China entries (#6758).
- Native extended thinking for Anthropic and Bedrock (#5652), prompt caching for OpenRouter (#6008), native Responses-protocol tool calls for Codex (#6117), [automatic fallback to a secondary provider on failure](https://docs.zeroclawlabs.ai/master/en/providers/routing.html) (#7178), and the Responses wire protocol honored across custom, OpenAI-compatible, and llama.cpp providers (#7180, #7172, #7418).

### Channels

- [New channels](https://docs.zeroclawlabs.ai/master/en/channels/overview.html): Twitch chat (#7275), WeCom AI Bot (#6680), AMQP with mutual TLS (#7369), and multi-tenant Linq with per-agent routing (#7041).
- Per-recipient reply pacing across nine channels (#6389), a configurable in-flight message budget (#7391), webhook retry with exponential backoff (#5838), a reply-intent precheck that can route to a cheaper per-agent classifier model (#6068, #6945), and selective channel builds behind the new lean default bundle (#6866, #6904).

### Tools and plugins

- [New tools](https://docs.zeroclawlabs.ai/master/en/tools/overview.html) for downloading remote files into the workspace (#6957), uploading single files and bundles over HTTP (#6773), sending messages to channels directly from the agent loop (#6665), reading and writing binary files (#7004), and authenticated outbound HTTP requests (#7354). Jina AI joins as a web-search backend (#6833).
- [WASM plugin interfaces](https://docs.zeroclawlabs.ai/master/en/developing/index.html) are now formally defined for tools, channels, and memory (#7060), with new plugins for Office document extraction (#7454), LanguageTool grammar checking (#7326), and self-hosted Stable Diffusion image generation (#7325).

### Memory

A [pluggable memory strategy](https://docs.zeroclawlabs.ai/master/en/agents/internals.html) now routes agent-turn, gateway, and channel consolidation through one path (#6907, #7053, #7234); time-windowed recall works correctly for markdown-backed memories (#7192); Postgres backend initialization is fixed (#7451).

### Voice, desktop, hardware, docs

- [Voice notes](https://docs.zeroclawlabs.ai/master/en/channels/voice.html) transcode to OGG/Opus for Telegram and WhatsApp (#7050).
- Desktop onboarding wizards for macOS, Linux, and Windows permissions (#6506, #6710).
- Smart-room named-device tools and ESP32 simulator harnesses (#7045, #7048, #7363).
- [Versioned documentation](https://docs.zeroclawlabs.ai/master/en/introduction.html) with a version selector (#7023), a full book rework that derives provider and config reference pages from source (#7365), a [FreeBSD setup guide](https://docs.zeroclawlabs.ai/master/en/setup/freebsd.html) with service files (#7161), [Podman guidance](https://docs.zeroclawlabs.ai/master/en/setup/container.html), a [NixOS module](https://docs.zeroclawlabs.ai/master/en/setup/nixos.html) (#6562), and a fully static container build pipeline (#7176).

## Security

- **Canvas token theft (GHSA-f385-f6h2-3gqj)**: the dashboard Canvas iframe sandbox is tightened so injected content can no longer exfiltrate the pairing token (#6942).
- **Bearer-token revocation**: rotating or deleting a paired device now actually invalidates its token at the gateway (#6988).
- **Secret handling**: redaction now covers every credential-shaped config surface, including nested shapes (#6918, #6982, #7261), and the channel orchestrator no longer falls back to another provider's credentials (#7066).
- **Outbound request guards**: private-host allowlists for the HTTP request tool (#6981) and for fetches whose DNS resolves to private addresses (#6974).
- **Tool gating**: per-agent tool allowlists are enforced when channels start (#7064) and on every message (#6960); scheduled agent jobs can no longer modify the scheduler itself by default (#7189); internal telemetry is blocked from the chat WebSocket by default (#7221).
- **[Sandboxing](https://docs.zeroclawlabs.ai/master/en/security/sandboxing.html)**: tighter library binds in the Linux sandbox (#6902), and interactive subprocesses can no longer hijack the controlling terminal (#7120).
- **Dependency advisories**: mail library updated for RUSTSEC-2026-0141 (#6662), the web frontend router bumped to clear five advisories (#7198), and Groq API keys added to the leak scanner (#6812).
- A partially invalid config can no longer silently default security-critical settings during daemon startup (#7160).

## Since v0.8.0-beta-2

For those tracking the beta line, the 184 commits since beta-2 concentrate on stabilization:

- **Config correctness**: the unbounded and yolo quickstart presets actually allow what they advertise (#7371), incremental saves no longer leave a stale schema-version label (#7274), the webhook channel gets a sensible default port (#7193), one malformed provider entry no longer wipes the rest of your providers on load (#7491), config browsing no longer leaks similarly named sibling agents (#7471), and a crash on multi-byte characters in CLI flags is fixed (#7154).
- **Channel delivery**: only the final assistant turn reaches channels (#7239), internal tool-result markup is stripped from replies (#5796), truncated tool-call fragments are dropped at delivery (#7370), webp images are normalized for vision models (#7135), Telegram preserves code fences across message splits (#6701) and restores forwarded-message attribution (#7251), WhatsApp delivery and reply-mentions work for the new ID format (#7008, #7226), and Matrix keeps separate session state per configured account while repairing key backups (#7388).
- **Cron**: disabling startup catch-up actually skips overdue jobs (#7348), one-shot reminders can be scheduled relative to now (#7188), schedules set in the past get a clear diagnostic (#7165), and DingTalk is available as a delivery channel (#7091).
- **Runtime**: history trimming can no longer empty the conversation entirely (#7403), parallel SubAgents and delegates return reliably (#7442), writing files into a container with no mounted workspace fails loudly instead of silently (#7129), and the gateway survives transient connection-accept errors instead of crashing (#7402).

## Breaking changes

- **[Schema V3](https://docs.zeroclawlabs.ai/master/en/reference/config.html)** (#6398): configs migrate automatically on first load. Profile settings are split between runtime behavior and risk policy, cost rates are reorganized per provider, and scheduled jobs must name the agent they run as.
- **[Lean default channel bundle](https://docs.zeroclawlabs.ai/master/en/channels/overview.html)** (#6904): prebuilt binaries ship a core channel set; social channels move to opt-in build features.
- **[Logging](https://docs.zeroclawlabs.ai/master/en/architecture/logging.html)** (#6398 train): the legacy trace-event path is retired in favor of the unified structured logging pipeline; third-party logging macros are banned workspace-wide.

## Contributors

### Join in

Over 100 people landed work in this release, most of them for the first time. If ZeroClaw does something almost right for you, that gap is a great first PR: a fix, a doc correction, a channel quirk, a provider you wish existed. Start with the [contributing guide](https://docs.zeroclawlabs.ai/master/en/contributing/index.html), open an issue if you want a sanity check first, and we will review it. The review queue moves daily.

### One of us

@0disoft, @abhinavmathur-atlan, @AbubakrSamsodien, @alex-nax, @alexandme, @aliasliao, @Alix-007, @arucil, @ATECHPCS, @Audacity88, @BernardKuo, @bheatwole, @bitscrafts, @casualjim, @chengzhichao-xydt, @ConYel, @DaBlitzStein, @drbparadise, @eastriverlee, @easyteacher, @eldar702, @fanchanghu, @flyin1600, @forsakenyang, @fresh-fx59, @FTDGRT, @g0ne150, @garacio, @guitaripod, @h03-xydt, @hanZeng-08, @IftekharUddin, @ilteoood, @itripn, @jeffjhunter, @joe2643, @johnrspeer83-png, @jokemanfire, @JordanTheJet, @jstar0, @kanmars, @kapelame, @kitswas, @kmsquire, @kristofferkoch, @locnh-ssid, @LuaniiMM, @markuman, @mastwet, @metalmon, @michidk, @mminkus, @mn13, @mov-xound-glitch, @nebullii, @Nillth, @ninenox, @NiuBlibing, @nixosclaw, @ozzyfly, @patrickzzz, @perlowja, @Project516, @puneetdixit200, @r4mmer, @rareba, @Rhoahndur, @rifuki, @roywong10, @RyanHoldren, @sbenedetto, @SebConejo, @sentinel-nnet, @silas-qiao, @SimianAstronaut7, @singlerider, @spacelobster88, @SpectreMercury, @Stealinglight, @Tenith01, @TeoConnexioh, @TheBlindM, @theonlyhennygod, @theredspoon, @thezillo, @tidux, @TJUEZ, @tmigone, @tylerjenningsw, @vernonstinebaker, @vrurg, @WareWolf-MoonWall, @whtiehack, @xianshishan, @XiaoliangWang1991, @xydigit-sj, @xydigitLybnnnn, @yanalialiuk, @yexca, @yijunyu, @Yyukan, @zwffff

---

Full diff: https://github.com/zeroclaw-labs/zeroclaw/compare/v0.7.5...v0.8.0
