# ZeroClaw v0.8.6

ZeroClaw v0.8.6 is a plugin, identity, and runtime-ownership release spanning **573 commits** from **74 contributors**. The seven 64-bit CLI release archives now carry the WebAssembly plugin host, and the plugin registry offers channel and tool packages built for this release. It also adds authenticated principals with OIDC across RPC and the gateway, moves live configuration, events, and agent lifecycle under the daemon, and adds an opt-in minimal built-in tool set while full stays the default. Several security tightenings change defaults; review [Breaking Changes](#breaking-changes) before upgrading gateway pairing integrations, remote ZeroCode connections, browser automation, private-network downloads, or plugins built against an earlier WIT, and read [Known issues](#known-issues) before relying on a plugin channel.

## In brief

Plugins now run in the release CLI: install Discord, Telegram, Matrix and ten more by name.

Plus groundwork for OIDC sign-in, multi-session ZeroCode, daemon-owned live config, and new providers and web search.

## Highlights

- **Plugins work from a stock CLI download:** install a channel or tool plugin by name, with no Rust toolchain.
  The seven 64-bit CLI archives for Linux, macOS and Windows carry the WebAssembly plugin host, and
  each one is smoke-tested with a real plugin before publication. The registry offers 13 packages built for
  this release: the Discord, Matrix, Telegram, Bluesky, Gitea, Mattermost, Mochat, Notion, Reddit,
  X (Twitter), WeChat and WeCom channels, and the redact-text tool. Discord passed an end-to-end
  acceptance run from release-candidate CLI archives on Linux, Windows and macOS. Installs grant egress
  up front, check that the plugin loads, bind channel instances, and recover after an interruption;
  updates are verified and staged (#11347, #10999, #11262, #11261, #11236, #11309, #11302, #11098, #10746).
- **Tool selection:** Full stays the default; minimal is an opt-in `[tools]` setting.
  Set `optional = []` for minimal: eleven built-ins in Chat and eight in Code/ACP,
  or name extras to add to it. Setup and config generation write `optional = ["*"]`,
  and configs without `[tools]` or `optional` load full. Existing explicit
  selections are preserved. Prerequisites, permissions, approvals and Code/ACP
  memory exclusions still apply.
- **Authenticated principals reach every entry point:** RPC, sessions, tools, and memory resolve to canonical principals.
  `oidc.<alias>` verifies tokens with device, client-credentials, and browser enrollment;
  pairing codes are stronger by default, and reading or minting them needs an owner-only
  admin token (#11082, #10248, #10259, #10263, #10265, #10255, #10307, #11177).
- **The daemon owns live state:** atomic live config revisions, one event firehose, and coordinated agent lifecycle.
  Configuration publishes atomic revisions with batch writes and reports whether each running
  target applied a change; the observer event firehose and a bounded, replayable subscription hub
  live in the daemon; and agent lifecycle, background delegates, and cost ceilings are coordinated
  in one place (#10911, #10823, #11466, #11131, #11167, #10621, #11292, #10804).
- **Chat surfaces and providers expand:** multi-session ZeroCode, new channel delivery modes, two providers, three web search backends.
  ZeroCode gains multi-session panes, a combined Sessions/Queue/Plan dock, and standard composer
  editing; Telegram, Matrix, WhatsApp Web, Mattermost, Slack, and QQ gain new delivery modes;
  Crusoe and Hailo-Ollama become providers; and Serply, AnySearch, and Keenable join web search
  (#9739, #10879, #11175, #8561, #10489, #10984, #10358, #10622, #10799, #9338, #9109, #10402, #10356, #10679).

## What's New

### Plugins

- Ship the WASM plugin host with Cranelift in the seven 64-bit CLI release archives for Linux, macOS and Windows, gate publication on a per-archive plugin smoke test, and raise the release binary size cap from 64 MiB to 80 MiB; the ARM32 and Android archives carry no plugin host. On these archives `zeroclaw plugin` is a command, and `zeroclaw status`, `/api/plugins` and the dashboard report the plugin host as available; plugins stay disabled until the operator enables them. The smoke test does not cover the desktop app bundles (#11347).
- The plugin registry lists 15 versions of 13 packages built for this release's plugin contract. Published versions that do not install on v0.8.6 were withdrawn, so the registry no longer offers a package that fails the install-time load check.
- Add `zeroclaw plugin update` with verified, staged replacement of an installed package, and recover incomplete installs through `plugin remove` (#11262, #11261, #11236).
- Install and activate tool plugins from `zeroclaw quickstart`, bind channel instances and seed their grants at install, and grant egress at install with a report of whether the plugin loads (#11309, #11302, #11098).
- Load-verify a plugin at install, add `plugin install --no-verify`, and give egress denials a ready-to-run fix (#10746, #11107).
- Add host-mediated sockets, WebSocket, named TLS profiles, and durable plugin state, and harden TLS profile materialization (#11081, #11228).
- Sign a manifest document in one step, admit exact component payload bytes, and open admitted payloads from the retained package root (#11310, #9134, #11232).
- Add the gateway and dashboard capability catalog, the package catalog in `plugin list`, typed event routing, and a durable scheduler outbox (#8909, #8908, #9138, #9139).
- Add governed plugin webhook ingress with typed challenge replies, admit channel mirrors declared by manifest `provides`, govern channel plugin egress, and report a plugin channel's own health in `/health` (#8862, #8949, #11178, #10750, #11348).
- Refuse plugin tool names in the MCP `<server>__<tool>` namespace, bound registry requests, read the machine trust store for plugin HTTPS, and replace a channel plugin's instance after a trap (#11785, #11581, #10491, #11356).

### Security and Identity

- Land the OIDC principal stack: canonical principals and shared grant resolution, authenticated principals on RPC with native and peer-credential providers, principal tool selectors in agent sessions, principal-owned sessions, private principal memory, and the `oidc.<alias>` token-verification provider with device, client-credentials, and browser PKCE enrollment (#11082, #10248, #10259, #10263, #10265, #10255).
- Retire the unwired Nevis integration and remove a leftover `[security.nevis]` table on the next incremental save (#11191).
- Require an owner-only admin token to read or mint pairing codes, use one shared pairing-code policy with a stronger default, and share one inbound-auth state between the gateway and RPC (#11177, #10307, #11202).
- Bind RPC sessions to the canonical authorized workspace, keep session environments immutable, revalidate forwarded environments on reuse, and fence remote session resumes and `session/configure` on the authorized incarnation (#11112, #11222, #11133, #10246, #11330).
- Gate `file_download` against SSRF with an explicit private-host opt-in, confine filesystem mutations to the workspace, and honor allowed roots for Git operations and in Seatbelt (#10070, #9977, #10337, #10556).
- Make full browser automation a separate opt-in from `browser_open`, keep `always_ask` in force under Full autonomy, enforce bounded child tool approvals, and apply the caller's tool-name filter to background bounded delegation (#9830, #9724, #10937, #11797).
- Require sender authorization for LINE groups, Bluesky, and Reddit (#9427, #9428).
- Harden Git shell policy arguments and the risk classifier, resolve host launchers before the workspace cwd, and stop presenting command execution as audited (#9678, #9635, #11029, #10381, #9410).
- Patch Wasmtime, Rustls, quinn-proto, and negentropy advisories, remove vulnerable `ruzstd` through probe-rs 0.32, and make the Docker sandbox image configurable (#11755, #11475, #11253, #11795, #11678, #11715, #10745).

### Runtime, RPC, and Daemon

- Publish atomic live configuration revisions, add `config/set-many` for atomic batch writes, and report each running target's application result for a changed path (#10911, #10823, #11466).
- Own the observer event firehose, the pricing refresher, and the gateway-start hook in the daemon, and serve subscriptions from a bounded, replayable hub (#11131, #11164, #11167).
- Bound the local RPC transport, add chunked uploads, and reach turn parity over RPC for steering, totals, and session operations (#11171, #11132).
- Deliver mid-turn steering at the next step instead of after the final answer, and keep each steering message's own source facts for auto-save and the transcript (#11725, #11181).
- Coordinate agent lifecycle mutations, persist peer-agent inbox turns, report live progress for background delegates, and route independent child approvals to the target operator (#10621, #11045, #11292, #11462).
- Enforce execution-tree iteration budgets, scope cost tracking into delegated sub-loops with per-agent ceilings, and price cache writes at the configured write premium (#10351, #10804, #10716).
- Add opt-in single-tool provider rounds with `runtime_profiles.<profile>.single_tool_rounds` (#11467).
- Add opt-in effort-aware routing with `runtime_profiles.<profile>.effort_routing`: simple and ambiguous turns stay on the configured local route, a complex turn uses the cloud route only with `cloud_escalation = "auto"`, and every provider attempt for the turn, including fallback and vision routing, stays within the chosen route (#11516).
- Anchor context compaction to the model window ratio, trim history to a low-water target, enforce a compact local prompt budget, and estimate context usage when a provider omits token counts (#9535, #10696, #10465, #9453).
- Separate cron run outcomes behind an internal-principal envelope, and add a typed stop taxonomy for turn-path aborts (#10425, #10504).
- Detect the platform default shell and shell output encoding, initialize PowerShell output as UTF-8, and expose the shell profile in status (#10956, #10955, #10954, #10354).
- Add entry-count log rotation with multi-segment queries, and stamp daemon and relay binaries with their build commit (#10214, #11196).
- Fingerprint the system and tools prompt prefix on `llm_request` events, expose token accounting on history-trim events, and attribute cost ledger records to the chat conversation (#10990, #9713, #10718).

### Tools, Skills, and Memory

- Select built-in tools with `[tools].optional`: full (`["*"]`) stays the default and is what setup and config generation write; `optional = []` selects minimal, eleven built-ins in Chat and eight in Code/ACP; named entries add extras to minimal (#11221).
- Native tool names take precedence over conflicting plugin tools in executable and captured tool views (#11221).
- Add a typed built-in tool inventory with tier ratchets, and defer built-in schemas through `tool_search` (#11308, #11473).
- Give tools a cooperative cancellation context through `Tool::execute_with_context`; existing `execute` implementations keep working (#11465).
- Add the Serply, AnySearch, and Keenable web search providers (#10402, #10356, #10679).
- Install skills from pinned well-known discovery indexes (#10944).
- Export an agent to a portable bundle with `zeroclaw agents export` (#9986).
- Add an outbound A2A client with a shared wire model and tools, and category-scoped cross-agent memory grants (#9324, #10252).
- Stamp recalled memory entries with their storage date (`[from YYYY-MM-DD]`); set `memory.recall_date_stamp = false` for the previous format (#10567).
- Decompress gzip, brotli, and deflate `web_fetch` responses, prefer attachments for large generated artifacts, and declare tool attachments explicitly (#9283, #11509, #10938).
- Materialize MCP image tool results into the multimodal pipeline and render one summary line per tool in the deferred MCP index (#10566, #10845).

### Providers

- Add Crusoe Managed Inference as an OpenAI-compatible provider family and native Hailo-Ollama support (#9338, #9109).
- Pass Anthropic extended thinking and prompt-cache markers through OpenAI-compatible gateways, add a configurable Anthropic `cache_ttl`, and forward `reasoning_effort` through an opt-in passthrough (#10605, #10623, #10724, #10916).
- Handle Anthropic refusals with fallback notices, carry live provider identity on usage events, and resolve the context window from the serving provider (#9272, #8966).
- Default `multimodal.max_image_size_mb` to the 20 MiB ceiling (#10589).

### ZeroCode, Dashboard, and Web

- Add multi-session panes with an agent sidebar and sidebar-launched Quickstart, combine Sessions, Queue, and Plan in one dock, and drag session rows or switch sessions by number (#9739, #10879, #11573).
- Add standard composer editing, add selected text to chat, make transcript URLs clickable, and expand tool transcript cards (#11175, #10553, #10386, #10295).
- Show whether a Config change was saved and whether each running target applied it, and rename provider aliases in Config (#11506, #11460).
- Inspect foreground subagent tasks, make session roots explicit, and report turn state to the terminal over OSC title and progress (#11446, #11044, #9876).
- Edit secret key/value maps in ZeroCode and the dashboard, add the Oxocarbon dark theme, and add `/upload` to open the image picker (#11419, #11719, #10578).
- Paginate persisted ACP transcripts and select a standalone default ACP agent (#10596, #9638).
- Surface Code session-history versus persistent-memory isolation, and localize runtime integration info, ZeroCode config metadata, daemon readiness diagnostics, status fragments, and chat setup guidance (#9341, #10472, #10378, #10848, #8546, #11496, #11028).

### Channels and Gateway

- Add Telegram `multi_message` streaming, a secure model picker, a `per_user_session` toggle for shared group chats, and passive group context (#8561, #9997, #9772, #10640).
- Deliver Matrix voice replies as MSC3245 voice notes and answer a mirror peer's voice message with a voice note (#10489, #11007).
- Post native WhatsApp Web polls, attach PDF and image previews, and render outbound Markdown in WhatsApp's dialect (#10984, #10980, #10982, #10475).
- Add Mattermost approval prompts and inject the channel purpose, accept Slack bot and workflow messages on request, and deliver one-off sends and cron output to QQ (#10358, #10946, #10622, #10799).
- Expose opt-in model fallback notices, stamp external ingress provenance, and show tool-call batch position on approval cards (#11464, #10907, #10004).
- Stream webhook chat turns over Server-Sent Events, and keep the agent turn running when the chat WebSocket disconnects (#10450, #10538).

### SOP

- Choose conditional steps with the decision model, and gate triggers and execution mode with selectable decision models (#11134, #11085).
- Add interoperable run logs and trigger deduplication, return full step results from `sops/run-detail`, and rename a SOP atomically from the web editor (#10155, #9930, #10233, #10527).
- Drive headless SOP runs and webhook-started SOP agent steps to completion (#9841, #10813, #11083).

### ZeroRelay

- Add self-serve enrollment with `relay claim` and a relay-terminated browser enrollment front door that prefills from `?node=&code=` links (#10592, #10525, #11089).

### Installation, Documentation, and Release Engineering

- Standard `dist` builds add the plugin host and Cranelift on the seven native 64-bit targets. Native compatibility adapters stay compiled in, plugin activation, trust and grants remain explicit, and the ten-target archive matrix and names are unchanged; Android remains experimental (#11347, #11221).
- Verify crates before the GitHub Release, block version bumps whose crates cannot publish, recover crates.io publishes with current tooling, and fail closed during version preparation (#11091, #11095, #11105, #10817).
- Compose the X and Discord announcements from the release notes, and measure release binary bytes per footprint profile (#10727, #11306, #10372).
- Add a canonical release target registry, build the web UI as a Nix package, and restore the `nix run` installation path (#10590, #11041, #10936).
- Gate WebSocket, email, IRC TLS, and tool dependencies behind the channel features that need them, so builds without those channels compile less; share one config snapshot across the tool registry build; and update matrix-sdk 0.19, rusqlite 0.40, and OpenTelemetry 0.33 (#10467, #10493, #11032, #10771, #11065).
- Generate `llms.txt` and `llms-full.txt` in the docs build and promote stable docs metadata without rebuilding (#10840, #10818, #11109).
- Document the WeCom channel, local model selection with llmfit, Astra API and Codex setup, and Parallel Search configuration (#10639, #11008, #10945, #10631).
- Document Build Remote Agent phone pairing, reload-refusal recovery by platform, experimental WIT version skew, the webhook-ingress capability flag, tool tiers, and the supported release line; move the multi-agent guide under Agents; correct plugin name-conflict and lean-build guidance, restore the private memory plane section, define the webhook audit argument byte limit, and show how ZeroClaw identifies itself to Parallel (#10283, #10847, #10507, #10839, #11305, #10765, #11114, #11284, #11190, #10806, #10917).

## Bug Fixes

| Area | Fix |
|---|---|
| Runtime and agents | Cancel a turn's delegate children with the turn, recover delegate settlements after worker exit, bind background delegate rows to their delegation chain, close the steering channel before a turn finishes, let an approved shell call rerun in the same turn, honor `delegation_policy` for bounded targets, cap the no-progress loop detector, keep operational paths and agent dispatch panic-free, serialize prepared file mutation batches, and make `TaskRecord` the single background lifecycle owner (#11298, #11450, #11043, #11617, #11653, #10601, #10503, #10133, #10134, #11201, #9726) |
| Context and prompts | Count retained history in whole turns, distinguish trim targets from model capacity, stop clamping explicit context budgets to the 32k fallback, cap the structured Agent system prompt and refresh it after a streamed model switch, keep the current date out of the cached system prompt, sort tool specs for a stable cache prefix, include the weekday in the per-turn preamble, and surface bootstrap-file and tool-result truncation (#9368, #11067, #11260, #11532, #11575, #10862, #10959, #10856, #11033, #10655) |
| Tool calls and parsing | Recover structured native tool arguments, preserve browser and search tool semantics, normalize tool aliases across text formats, preserve string arguments during fallback parsing, keep quoted tool-result prose out of the streaming protocol guard, and deliver pre-tool narration and terminal fallbacks live (#11479, #11188, #11025, #10914, #10935, #10722, #10417) |
| Providers | Retry stream failures before send, give single-candidate recovery its retry budget, keep OpenRouter streams alive, keep signed reasoning intact, let `timeout_secs` raise the streaming idle bound, keep the Anthropic cache breakpoint after a trailing image, send the OpenCode session header, resolve the Grok executable before workspace spawn, derive context windows from the family registry, tolerate non-object pricing, attribute reliable stream errors to the served model, warm compatible connections through the models endpoint, preserve configured provider profiles and aliases in model discovery, remove arbitrary Hailo prompt caps, and harden the Copilot credential cache and Grok ACP frames (#10415, #10651, #10172, #10882, #10902, #11170, #10803, #10442, #10953, #10886, #10895, #10604, #10864, #10829, #9854, #11627, #10370, #10830) |
| Multimodal | Recover from rejected image requests, validate images at the pixel level, evict images in batches past the per-request cap, keep tool-result images live for the current turn, pass marker-free text through verbatim, preserve attached images after source removal, thread operator multimodal settings into provider adapters, and stop promoting path listings into image markers (#10480, #9819, #11582, #10903, #11200, #10088, #10552, #10555, #10904, #10894, #10582, #10838, #10890, #11163) |
| RPC, ACP, and MCP | Retire a local connection when its writer fails, register live channels for every session tool, honor the agent parameter on memory methods, report denied batch entries, preserve live sessions when mode replacement fails, close connections on daemon reload, persist interrupted, failed, and cancelled ACP turns, and send MCP tool results as text blocks (#11291, #11452, #11151, #10824, #10775, #10262, #10197, #9378, #10810, #11445, #10397) |
| ZeroCode | Requeue a busy-refused message, keep failed-turn status across reconnect, exit on terminal loss, keep the TUI alive on RPC timeout, avoid duplicate streamed responses, preserve undo history, make Ctrl+N additive, handle the Delete key, restore macOS Control aliases, keep composer and completion keys and clicks out of the transcript, make scrollbar seeking continuous across loaded history, scope Config filters and restore a cancelled selection, start local sessions in the expected directory, render transient frames as a viewport slice, and wrap oversized table cells (#9399, #11619, #11609, #11528, #11607, #10672, #11290, #10849, #10846, #10809, #11219, #10565, #9317, #11295, #11700, #11288, #11504, #10801, #11493, #11449, #10648, #10473, #10964) |
| Channels | Restore supervised shell approval routing, route every outbound HTTP client through the runtime proxy, connect CLI Ctrl+C to the supervisor, batch Telegram media groups and resolve Telegram voice peers, accept Signal UUID senders and report Signal DMs as direct, persist the WeChat cursor only after enqueue, honor WhatsApp Web `suppress_voice`, bind WhatsApp transcription to the owning agent, and keep system notices out of TTS (#10241, #10748, #9197, #8955, #10942, #9777, #10833, #9313, #11057, #10692, #10827, #10266, #10899, #10239, #10958, #10627, #10620, #10733, #10798) |
| Gateway | Confine dashboard reads to opened file handles, recheck config-write authority under the lock, preserve config write invariants, sanitize public health errors, consolidate WebSocket memory on the agent's own provider, and pin and correlate webhook audit destinations (#11455, #11206, #10455, #10664, #10637, #10678, #10016, #10638, #10375, #10714) |
| Configuration | Refuse unproven full saves over existing files, lock the loader's data dir and resume interrupted splits, distinguish absent from empty `allowed_tools`, map the V2 `openai-chat` kind to the OpenAI family, bind degraded remediation to the running executable, and make model-provider iteration deterministic (#11527, #11370, #9753, #10686, #10630, #10612, #10865, #10819) |
| Security and redaction | Recognize the null device on every host, redact proxy credentials and web-search query URLs, sign TUI identities with the install key, bound skill HTTP calls with one deadline, confine datasheet downloads, isolate manifest-installed hardware plugin subprocesses, and create Edge TTS artifacts owner-only (#11469, #11026, #11147, #11576, #11512, #10015, #10093, #10449) |
| Tools, SOP, and cron | Detach shell children from the controlling terminal, stop inlining base64 into screenshot results, expose owned ACP sessions to session tools and clarify `sessions_send` append semantics, reuse configured provider references for agents and `llm_task`, preserve omitted fields and unrelated config in targeted agent upserts, preserve the Windows PowerShell analysis cache, settle rejected headless SOP runs, return the retained failure reason from `sops/run-detail`, and guard cron manual triggers (#11543, #11046, #10468, #11461, #10554, #11693, #10519, #10811, #11031, #11435, #10877, #10414, #10296) |
| Memory and storage | Reject unsafe SQLite storage entries, harden audit hygiene admission and response cache permissions, route the CLI memory factory through the storage-aware resolver, and preserve the audit chain through log rotation (#10835, #11458, #10091, #10652, #10463) |
| Plugins | Report a full connection budget as connection-limit-reached, cap root config schema properties, reject expired dial budgets, keep resumed legacy plugin recovery discoverable, and discard a memory plugin's instance after a trap or other failed call instead of reusing it (#10751, #10524, #10658, #11520, #11402) |
| Daemon, Windows, and macOS | Replace open SOP files and match directory triggers on Windows, refuse directory attachments by type, protect Windows key files at creation, bound Windows task and desktop capture logs, recognize exited Windows task owners, fail fast on macOS SIGBUS after executable loss and on over-long IPC socket paths, and preserve supervised error chains (#11362, #11363, #11361, #11365, #11451, #10931, #10236, #10928, #11424, #10737, #10245, #10732, #10671, #11499, #11137) |
| Cost, tunnel, and logs | Publish WSS and enrollment through `tailscale serve` with tailnet-valid certificates, apply `config/set` cost limits to the live tracker, count a revisited alias once in delegation ceilings, surface pricing-unavailable, count By Period costs in UTC, and drain config patch notices before exit (#11530, #11531, #11587, #11287, #9939, #11695, #11629, #11453, #11454, #11184, #11518, #10628) |
| Install, Nix, and release | Select the Android binary on Termux, set `meta.mainProgram` and build ZeroCode with its own features in the flake, publish the dashboard bundle that preflight verified, stream crate metadata into publish ordering, order versioned dev dependencies, break the zerorelay/runtime dev-dependency cycle, check Apple notarization before compilation, and stage the signer example during source container prefetch (#10183, #11072, #11040, #11086, #10642, #10815, #11066, #10816, #11738) |
| crates.io consumers | Build `zeroclaw-api` standalone with tokio's `time` feature, and move `zip` to 9.0.3 so a fresh `cargo install zeroclaw` resolves without `--locked` (#11285, #11790) |

## Breaking Changes

- **Pairing codes are longer and admin-gated.** Codes default to 32 case-sensitive alphanumeric characters instead of six digits; treat them as opaque, variable-width strings, or set `[gateway.pairing_code] length = 6, charset = "numeric"` for the old shape (#10307). The dashboard no longer pre-fills the first-run code: read it from the startup banner or run `zeroclaw gateway get-paircode` on the gateway host as the gateway's user, and send `x-zeroclaw-admin-token` from `<data_dir>/gateway-admin.token` when calling `/admin/paircode` directly (#11177).
- **Remote WSS requires authentication.** A remote ZeroCode connection must present `auth_token` (or `ZEROCLAW_AUTH_TOKEN`, or `auth_token_file`); pair with the gateway to obtain one. `[security.nevis]` is ignored and removed on the next incremental save; move to `[oidc.<alias>]` and `[permission_profiles]` (#11082, #11121, #11191).
- **Browser automation is a separate opt-in.** `[browser] enabled = true` no longer implies the `browser` automation tool; set `browser.automation_enabled = true`. `browser_open` is unchanged (#9830).
- **`file_download` refuses private-network hosts.** Add internal endpoints to `[file_download] allowed_private_hosts`; cloud metadata hosts stay blocked, and proxy-only egress needs a direct path (#10070).
- **Filesystem and Git tools stay inside allowed roots.** Writes outside the workspace and to special files such as `/dev/null` are rejected, and Git operations that escape an allowed root are refused (#9977, #10337).
- **Approvals tighten.** Full autonomy no longer overrides `always_ask` (#9724). A distinct approval route that sets `approver_channel` must also set `approver_recipient`, and an `Always` approval no longer carries into later turns (#10241). Bounded delegate children need retained tools in the target profile's `auto_approve`, and a background bounded delegate gets only the tools its caller may use (#10937, #11797).
- **The shell follows the platform default.** ZeroClaw now detects the platform's default shell; set `runtime.shell = "sh"` to keep a prior Unix choice or `runtime.shell = "cmd"` to keep Windows cmd behavior (#10956).
- **Sandboxes fail closed.** A Seatbelt initialization failure now stops the command instead of running it unsandboxed (#10556, #11499). Firejail and Bubblewrap shell-identity preservation needs a validated GNU `env` helper on the host `PATH`, and manifest-installed hardware plugins no longer inherit arbitrary environment variables (#10381, #10093).
- **Unsafe SQLite files are refused.** On Unix, memory, response-cache, and audit databases (and their `-wal`/`-shm` files) that are symlinks, hardlinked, foreign-owned, or group/world-accessible are rejected; stop ZeroClaw, make each file a private regular file owned by the service user, then restart (#10835).
- **Windows service installs need a one-time reinstall for bounded logs.** Disable the task (`schtasks /Change /TN "ZeroClaw Daemon" /Disable`), reboot, then run `zeroclaw service install` again (#10931).
- **Upgrade ZeroCode together with the daemon.** An older ZeroCode cannot read `config/list` once it contains secret-map entries (#11419).
- **Peer-agent sends need a writable data directory.** Sends now register a durable task before dispatch and fail closed without one (#11045).
- **Grok resolves from an absolute path.** Configure an absolute path to the Grok executable, or keep it in an absolute `PATH` directory (#10829).
- **Matrix answers voice with voice.** Set `output_modality = "text"` on a group to keep text-only replies (#11007).
- **LINE groups, Bluesky, and Reddit require sender authorization.** Add allowed senders to a `[peer_groups.<name>]` entry for that channel (#9427, #9428).
- **`max_history_messages` counts whole turns.** Review values that were set as raw-row caps; raising `max_tool_iterations` no longer raises the derived history cap (#9368).
- **Plugins built for v0.8.5 may need a rebuild.** Channel components must be rebuilt against this release's experimental `wit/v0` (#8862, #8949); precompiled `.cwasm` artifacts must be recompiled for Wasmtime 48 (#11475); `plugin install` refuses a component that does not instantiate unless `--no-verify` is passed (#10746); strict-mode manifests require `wasm_sha256` (#9134); and `plugin info` exits non-zero for an unknown plugin or one that does not load (#11098). The experimental `/api/plugins` response now returns package rows (#8909).
- **Registry channel packages installed before v0.8.6 do not load on it.** Those versions were withdrawn from the registry. Remove each one, install the current version by name with `--channel-alias`, and set its values again under the instance key: the new versions no longer read `[channels.<name>.*]`, and credentials go through the masked `zeroclaw config set` prompt. Each package's README has the exact commands.
- **Inbound A2A uses the v1 method.** The legacy `message/send` method is renamed to `SendMessage` with the v1 payload shape (#9324).
- **Gateway config PATCH needs real secret values.** Send the actual secret for add or replace, and a remove operation to clear one (#10455).
- **ZeroCode's Primary+A and Control+A select all composer text.** Use `/attach` or a custom chord for the file browser (#11175).
- **Dormant DORA telemetry is retired.** Remove queries for the five retired Prometheus metric families; Rust consumers must drop `DoraCollector` and `DoraSnapshot` (#9451).
- **Rust API changes in published crates.** cargo-semver-checks against crates.io 0.8.5 finds public-API breaks in 15 of the 24 published crates. Notable ones: `zeroclaw-api` drops `Principal`'s `user_id`, `roles`, `scopes` and `allowed_aliases` fields and its builder methods, drops the `AuthOutcome::Authenticated`/`Trusted` variants and the DORA observer variants, and makes `DenyReason` and `TurnEvent` `#[non_exhaustive]` (#10248, #9451, #8966). `zeroclaw-config` removes `NevisConfig` and changes the `PairingGuard` constructor and code-generation signatures (#11082, #10307). `zeroclaw-runtime`, `zeroclaw-gateway` and `zeroclaw-channels` remove or re-sign internal-facing modules and functions, and `RpcTransport` gains a required `kind` method. The `zerocode` library now exports only its keymap (#10120). Many public structs and exhaustive enums gained fields or variants, so downstream struct literals and exhaustive matches must update. Direct `zeroclaw-config` dependents that need WebSocket transport must enable `ws-transport` (#10467).

## Known issues

These issues ship in v0.8.6 and are tracked for the next release. None of them weakens a security check; where behaviour is affected, it fails closed.

- **The daemon can refuse a group-writable data directory.** Under umask `0002`, which many Linux distributions set for user-private groups, ZeroClaw creates its data directory as `0775` and the daemon then refuses it with "config lifecycle directory <path> is writable by other users (mode 775)". A data directory left `0775` by v0.8.5 is refused the same way. Remove group and other write access from the directory the message names, then start again: `chmod go-w ~/.zeroclaw/data` for the default location, or `chmod go-w <path>` with the path from the message. A fix is in review (#11803).
- **A silent first address can stall a plugin's connection.** The plugin host tries a destination's resolved addresses one at a time, so when the first one never answers, typically an IPv6 route that drops packets, the connection waits out its timeout and the other addresses, including IPv4, are never tried. Requests then fail as call timeouts while other programs reach the service. Fix or disable the broken route on the host (#11813).
- **The host does not record refused WebSocket and socket connections.** A plugin's WebSocket or socket connection to a destination its instance has not been granted is refused, but only HTTP refusals write a `plugin_egress_denied` record with the host and a remedy. For the other two transports only the plugin's own log shows the refusal; Discord logs `gateway connect failed {error: destination-denied, ...}`. The host record follows in v0.8.7 (#11304).
- **A plugin channel that never reaches its service can stay quiet.** A channel package such as telegram that cannot reach its server, or whose credential is refused, logs nothing about it at the default level. `/health` reports its component (`channel:plugin.<alias>`) as `starting` with `last_ok: null` for as long as no exchange has succeeded, so treat a component that stays `starting` as unreachable or misconfigured, and check its egress grant and credential.
- **Ten registry channel packages have not run against their live services.** bluesky, gitea, mattermost, mochat, notion, reddit, telegram, twitter, wechat and wecom were ported to this release's plugin contract and checked to install, load, bind, and validate their configuration on a release-candidate build; telegram also ran in a daemon against a local test server. None has run against its live service with a real account yet. Discord has (#10999).
- **`config set` masks every plugin setting.** `zeroclaw config set plugins.entries.<key>.config.<name>` treats every plugin setting as a secret, so it ignores a value given on the command line and opens a masked prompt, and fails without a terminal. Pass `--no-interactive` with the value for non-secret settings such as `guild_ids`; keep the masked prompt for credentials.
- **`plugin remove` can delete the wrong directory for a renamed package.** Removing a loaded plugin deletes `<plugins_dir>/<name>`, not the directory the package loaded from. This only matters when a package's directory name differs from its manifest name, and it is unchanged since v0.8.5. `plugin update` replaces the loaded directory itself and is not affected (#11357).
- **Plugin package lock files follow the process umask.** The lock and lease files used by install, update and remove can be readable by another local account that can read the plugins directory. Such an account can hold the lock and make those commands fail after 60 seconds. This affects availability only; owner-only (`0600`) lock files follow in a later release (#11360).
- **Discovery that overlaps `plugin update` can briefly pair old and new files.** Discovery reads a package's manifest and component separately, so a discovery during an update can pair the old manifest with the new component until the next discovery. Packages whose manifest declares `wasm_sha256` are protected, because admission checks the digest in every signature mode (#11562).
- **`plugin info` and `plugin list --verify` check only the WIT contract.** A plugin whose retained config entry holds values while its manifest declares no `config_schema` is reported as `[loads]`, but the runtime refuses to load it. `plugin update` already re-validates retained entries against the new manifest (#11336).
- **Install seeds tool egress grants only for `http_client`.** A tool package that requests only `websocket_client` or `socket_client` gets no seeded grant and is denied, and `plugin list` diagnoses gaps only for `http_client`. No registry tool package is affected (#11552).
- **Installing over an installed package is refused.** `plugin install` of a package that is already installed refuses and prints the matching `plugin update` command. Re-seeding an installed package in place is not available in this release (#10162).
- **Registry packages have no versioned WIT compatibility check yet.** The registry lists only versions built for this release, and the install-time load check refuses an incompatible package, but a package does not yet declare which host versions it supports (#9624).
- **Tool names that register after plugins are not yet checked against plugin names.** Plugin tool names in the MCP `<server>__<tool>` namespace are refused (#11785), and native tool names take precedence over conflicting plugin tools (#11221), but peripheral and skill-defined tools that register after plugins are not yet covered (#11327).
- **Slack no longer shows "is thinking…" in ordinary channel threads.** Since v0.8.5, an agent mentioned in a channel thread shows only the 👀 acknowledgement reaction until it replies; the status line now appears only in Slack's AI-assistant pane. With `stream_drafts = true`, progress appears as a draft message that the final answer replaces. The fix is deferred to the next release (#11416).
- **The v0.8.6 `dist` container image follows later.** Its arm64 build runs out of memory on the release runners, so this release is not expected to publish `dist-v0.8.6`, and the floating `dist` tag stays on v0.8.5 until the image is rebuilt. The `v0.8.6`, `latest`, `v0.8.6-debian` and `debian` images, for amd64 and arm64, carry the same features, repackaged from the release archives; use one of them in the meantime (#11828).

## Binary size

Release binary bytes per target, as printed by each build job's binary size check. v0.8.5 comes from its release run; v0.8.6 from a `dist` Cross-Platform Build of a release candidate (run 39001999557, `61a2c951d1`). The seven 64-bit targets gain the WebAssembly plugin host with Cranelift; the ARM32 and Android binaries carry no plugin host. The release size cap rises from 64 MiB to 80 MiB (#11347); the largest binary, x86_64 Linux musl, is 76,241,040 bytes, 7.29 MiB under it.

```text
target                               v0.8.5      v0.8.6       delta  delta %  plugin host
x86_64-unknown-linux-gnu         61,537,880  76,200,840 +14,662,960   +23.8%  yes
x86_64-unknown-linux-musl        61,572,272  76,241,040 +14,668,768   +23.8%  yes
aarch64-unknown-linux-gnu        44,667,752  55,584,592 +10,916,840   +24.4%  yes
aarch64-unknown-linux-musl       41,238,784  51,438,816 +10,200,032   +24.7%  yes
x86_64-apple-darwin              51,919,432  63,993,384 +12,073,952   +23.3%  yes
aarch64-apple-darwin             34,434,096  42,561,168  +8,127,072   +23.6%  yes
x86_64-pc-windows-msvc           51,867,648  64,100,352 +12,232,704   +23.6%  yes
armv7-unknown-linux-gnueabihf    47,397,720  52,747,620  +5,349,900   +11.3%  no
arm-unknown-linux-gnueabihf      47,741,784  53,087,588  +5,345,804   +11.2%  no
aarch64-linux-android            41,166,488  45,264,216  +4,097,728   +10.0%  no
```

## Contributors

@Aarlington
@abhinavmathur-atlan
@alucryd
@AngryPacifist
@aprint2008-sys
@ATECHPCS
@atirna
@Audacity88
@be-student
@chekolyn
@ConYel
@cryptoduke01
@drbparadise
@egorchenkov
@eugeneb50
@Franklin-Qi
@georgeatparallel
@ggettert
@googio
@grrowl
@h03-xydt
@iceHub82
@IftekharUddin
@ilya-bogin-keenable
@joalvaradon
@JordanTheJet
@joshuavetos
@jstar0
@jxxralf
@kckylechen1
@kingstar001
@kizito917
@kouhe3
@Leon-SK668
@LinespottingPrivate
@linhongyu510
@LiuwqGit
@lucaszhuang1210
@ludflu
@Lusitaniae
@MannXo
@metalmon
@minato32
@morningstarnasser
@mov-xound-glitch
@NiuBlibing
@pederbe
@perlowja
@Project516
@realadme
@rifuki
@rtimush
@RustLangLatam
@sbenedetto
@sebkraemer
@SheaHawkins
@singlerider
@Stalesamy
@sunlit-deng
@thorstenweber83
@tidux
@tmielika
@tmigone
@tunglambk
@ump45nose
@vikng-dev
@VladimirLewisII
@volodkindv
@vrurg
@wangmiao0668000666
@wromansky
@Xixiartemis
@Xscaperrr
@ZiBibro

## Full Changelog

The 573-commit range contains 132 feature, 283 fix, 14 refactor, 12 performance, 56 documentation, 19 CI, 44 test, and 13 chore commits. Internal maintenance entries are grouped above when user-visible and remain available individually in the full comparison.

**Full diff:** [Compare v0.8.5...v0.8.6](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.8.5...v0.8.6)
