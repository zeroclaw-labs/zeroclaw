# ZeroClaw v0.8.5

ZeroClaw v0.8.5 is a security, connectivity, and operator-experience release spanning **454 commits** from **73 contributors**. It introduces ZeroRelay and ZeroRouter, expands live chat and provider capabilities, hardens plugin, sandbox, webhook, credential, and file boundaries, and makes cross-platform builds and coordinated crate publication more reproducible. This release also removes or narrows several deprecated surfaces; review [Breaking Changes](#breaking-changes) before upgrading plugins, skills, legacy node integrations, WATI deployments, TodoWrite configuration, or contributor tooling.

## Highlights

- **ZeroRelay and ZeroRouter arrive together:** ZeroRelay provides blind forwarding with native mTLS enrollment, while ZeroRouter adds a first-class hosted routing preset and public model catalog (#10142, #9645, #10453).
- **Chat becomes more capable:** the dashboard can keep several conversations open per agent, open the same agent in multiple tabs, and attach or drop images through the new upload path (#9353, #9355, #10544).
- **Provider support expands:** Anthropic gains opt-in live thinking display, Grok Build ACP and Atlas Cloud become first-class providers, and compatible providers gain stronger response, attribution, image, proxy, and retry handling (#10542, #9104, #9200, #9404, #9974, #10448).
- **Plugin and skill boundaries get substantially tighter:** typed instance configuration, scoped secrets, host-owned egress, bounded exports, path-traversal protection, and fail-closed skill HTTP execution now cover the main package and network trust boundaries (#9126, #9128, #9137, #9403, #9582, #10369, GHSA-93f6-34w8-5g98).
- **Operator feedback improves across surfaces:** Matrix, Telegram, Slack, WhatsApp Web, SOP, MCP, logs, queued messages, and clipboard flows expose clearer progress and recovery behavior (#8443, #9822, #8985, #9385, #9476, #9196, #10057, #10096).
- **Release portability improves:** routine builders move to Rust 1.98 while source compatibility remains at Rust 1.96, release tools use pinned binaries, MUSL and Alpine coverage expands, and the coordinated 23-crate workspace gains protected crates.io publication (#9527, #10122, #10174, #9286, #9514, #10158).

## What's New

### ZeroRelay, Routing, and Providers

- Add ZeroRelay secure transport with blind forwarding, native mTLS enrollment, bounded admission, and per-node routing (#10142).
- Add the ZeroRouter provider preset and public catalog, then point new defaults at the hosted beta deployment (#9645, #10453).
- Add Anthropic `thinking.display` support so eligible models can stream readable thinking updates while retaining signed reasoning only for replay (#10542).
- Add Grok Build ACP and Atlas Cloud providers, accept data-wrapped compatible responses, and centralize provider endpoint metadata (#9104, #9200, #9404, #9747).
- Preserve OpenRouter attribution and streaming metadata, model context during Gemini requests, compatible tool-result image policy, and accurate served-model and lifecycle accounting (#9974, #9782, #10435, #10448, #10027, #10144).
- Improve terminal provider errors, model-list bounds, OAuth refresh behavior, proxy handling, and reasoning-tool fallback classification; retry replay-safe empty streams once and evict compatible-provider images individually (#10234, #10314, #10012, #9606, #9400, #10211, #10602, #10564).

### Dashboard, ZeroCode, and RPC

- Hold several independent conversations per agent, allow the same agent to remain open in multiple chat tabs, and add session-local Todo close control (#9353, #9355, #10584).
- Add image upload plus dashboard attach/drop UI through a bounded gateway endpoint (#10544).
- Add queued-message recovery, transcript copy actions, selectable log text, text-editor cursor navigation, and Option-Backspace word deletion (#10057, #9994, #10096, #9287, #10078).
- Add read-only SOP status and run views with live icons, authenticated cancellation, and responsive background refresh (#9694, #9688, #9692, #9476, #10392).
- Keep chat WebSockets alive, preserve persisted ACP transcripts and prompt completion, expose `keep_siblings` control, and describe the originating ZeroCode interaction context on new sessions (#9701, #10380, #10466, #9738, #10382).
- Keep input and inactive-chat refresh responsive during reconnects, make modifier intent explicit, show the active log path on payload fallback, and improve terminal restoration, clipboard errors, mouse decoding, paste ownership, file-search keys, and local-session fallback (#10374, #10393, #10479, #10474, #10184, #10443, #10444, #10278, #10065, #10541).

### Channels, Gateway, SOP, and MCP

- Add single-message progress drafts for Matrix, tool-progress drafts for Telegram, visible lifecycle progress for Slack, and first-interaction Slack thread hydration (#8443, #9822, #8985, #8969).
- Port WhatsApp Web to the registry-backed 0.7 stack, add approval requests and configurable push names, make an empty group allowlist fail closed, preserve storage integrity, and clean up cloud approval tokens on every exit (#10153, #9385, #10201, #9382, #10438, #9612).
- Preserve Telegram reply threads in main-chat history, self-destruct approval cards after use, cap command-menu bodies by bytes, and populate typed media envelopes (#10418, #10064, #10389, #9563).
- Resolve routed transcription providers and language hints from the owning live configuration across Discord, Matrix, Voice Wake, OpenAI, and Deepgram paths (#10494, #10487, #10032, #10431).
- Add authenticated operator cancellation for running SOP jobs and record structured failure causes (#9476, #9957).
- Materialize MCP resource blobs with an aggregate budget preflight, support per-server custom CA trust, and preserve persistent SSE event boundaries (#9196, #9405, #10459).
- Add tool-owned invocation triggers using the shared `send_via` vocabulary (#9766).

### Plugins, Skills, Configuration, and Tools

- Require typed instance schemas for plugins that read configuration and add coherent channel-config services (#9126, #9129).
- Add scoped tool-secret access, a shared plugin egress foundation, host-owned `wasi:http` policy, and logical channel instances (#9128, #9137, #9582, #10146).
- Bound plugin export calls by wall-clock time and execute live-config plugin regressions in required CI (#9403, #10181).
- Default skills to compact prompt injection, retain the v0.8.x compatibility path for full injection, and reduce skill hashing to observable bytes (#8313, #9913, #9837).
- Report the active native shell dialect, support PowerShell on Windows, and preserve coding-CLI environments across platform adapters (#9981, #9182, #10403).
- Warn about dangling channel references, expose workspace paths to config tooling, retain detailed tool output alongside short errors, and make config/provider path handling fallible and safer around bare paths (#9311, #9616, #10132, #10364, #10498).

### Security and Supply Chain

- Reject plugin `wasm_path` traversal before discovery or installation, closing the arbitrary-write issue tracked as **GHSA-93f6-34w8-5g98**.
- Patch the Wasmtime sandbox escape, upgrade the Wasmtime stack for RUSTSEC-2026-0222, and remove obsolete advisory exceptions (#10508, #9589, #8781).
- Enforce Landlock `allowed_roots` tiers, retain DNS and TLS configuration access, and apply `forbidden_paths` underneath allowed roots and workspaces (#10100, #10098, #9937).
- Resolve shell path arguments before execution, block symlink races during skill installation, and avoid nesting a Docker sandbox inside an existing Docker runtime (#9384, #10367, #9402).
- Bound skill-defined HTTP egress through one validated destination, pinned addresses, no ambient proxy or redirect following, and a 1 MiB response cap (#10369).
- Enforce host-owned plugin egress, contain private-network carveouts, and harden built-in HTTP tools on the shared network guard (#9582, #10250, #9580).
- Require authenticated webhook ingress, verify WhatsApp Cloud and Linq requests, enforce Linq alias ownership, and compare Lark verification tokens in constant time (#9744, #9569, #9604, #9110).
- Authorize channel approval responders independently, enforce delegate-specific approval policy, and make agent-scoped cron mutations atomic (#9574, #10188, #10177).
- Harden webhook audit exports and redact duplicate idempotency keys, Slack tokens, URL query secrets, Anthropic credential fragments, and TTS keys (#9995, #10256, #8918, #9435, #10092, #10433, #10191).

### Installation, Containers, Documentation, and Release Engineering

- Generate canonical installation documentation from the typed installer specification and include the Git channel in official artifacts (#9267, #10363).
- Add optional multi-architecture Alpine images and enforce non-root metadata across production container variants (#9514, #10095, #10176).
- Stop compiling release-only tools from source, verify their pinned assets on native runners, cache Windows release builds, and prevent the experimental Android leg from blocking stable releases (#10122, #10174, #10113, #10112).
- Add x86-64 and ARM64 MUSL measurement builds and stronger cross-platform release coverage (#9286, #9398).
- Move routine CI and container builders to Rust 1.98 while retaining the declared Rust 1.96 source floor (#9527).
- Publish the coordinated 23-crate workspace to crates.io through a tokenless tarball preflight and a protected, resumable upload job (#10158).
- Add reader-scale and Mermaid controls, generate SOP syntax documentation from runtime sources, and update the docs toolchain to mdBook 0.5.4 (#10515, #10383, #10517).
- Move the official website and documentation links to `zeroclaw.com` and `docs.zeroclaw.com` while retaining redirects from the legacy domain (#10616).
- Rehearse Scoop credentials before release, retry AUR publication through outages, detect stale packages, and bound shared apt installation attempts (#9785, #9787, #10156).

## Bug Fixes

| Area | Fix |
|---|---|
| Providers | Harden stream completion and structured upstream errors; retry replay-safe empty streams; evict images individually; preserve compatible, OpenRouter, Gemini, proxy, fallback, and lifecycle metadata; and report terminal causes accurately (#8838, #9968, #9782, #10435, #10448, #9606, #10234, #10027, #10144, #10602, #10564) |
| Runtime and agents | Detect context overflow through error causes, preserve session state from RPC prompts, retain detailed tool output alongside short errors, serialize live provider replacement, make streamed user turns conversational, clarify operator denials, and keep Docker runtime boundaries intact (#10416, #10030, #10364, #9748, #9325, #10490, #9402) |
| Channels | Repair health ownership, transcription routing, Matrix reasoning identity, Telegram history and media, WhatsApp group-policy and persistence behavior, inbound alias attribution, approval authorization, and cancellation-aware filesystem listeners (#10005, #10494, #10487, #10481, #10418, #9563, #9382, #10438, #10029, #9574, #10217) |
| ZeroCode | Clear disconnected dashboard state, restore ACP transcripts and prompt completion, keep reconnect input and inactive-chat refresh responsive, keep SOP navigation responsive, preserve terminal and paste ownership, surface clipboard failures, and repair mouse and search behavior (#10260, #10380, #10466, #10374, #10393, #10392, #10184, #10278, #10443, #10444, #10065) |
| Configuration | Honor `ZEROCLAW_CONFIG_DIR`, reject unsafe bare-path overwrites, complete required sections consistently, preserve cost-cache and period calculations, migrate legacy provider selectors, and roll back failed map-alias writes (#10521, #10498, #10476, #10482, #10462, #9707, #9281) |
| Security | Close sandbox, path, egress, webhook, secret-redaction, approval, and action-budget gaps across Wasmtime, Landlock, plugins, skills, shell, channels, and audit exports (#10508, #10100, #10098, #9937, #9582, #10369, #9384, #10367, #9569, #9995, #9996) |
| Plugins, skills, and tools | Enforce typed plugin configuration, scope secrets and egress, prevent install races, preserve skill-review history, keep coding environments intact, and replace panic-prone tool assumptions with ordinary errors (#9126, #9128, #9582, #10367, #9515, #10403, #10129) |
| Cron, SOP, and memory | Make agent-scoped cron updates atomic, preserve scheduler workspace and originating delivery aliases, map command patches to prompts, retain SOP failure causes, and preserve memory snapshot and pgvector initialization integrity (#10177, #10253, #9941, #10258, #9957, #10469, #10209) |
| Gateway, logging, and MCP | Require authenticated dispatch, redact sensitive keys, preserve JSONL validation and log-facade records, keep chat sockets alive, and preserve MCP SSE/resource boundaries (#9744, #10256, #9873, #10203, #9701, #10459, #9196) |
| Hardware and desktop | Clean Arduino temporary directories, keep platform paths panic-free, select Windows Common Controls v6, clean screenshot artifacts, and keep Unix-only cleanup off Windows (#9903, #10136, #10080, #9931, #9721) |
| Install, CI, and release | Align Rust 1.98 diagnostics and builders, include Git in artifacts, bound apt retries, strengthen generated-dashboard and PR-history validation, verify release tools and packaging across native targets, and add protected publication of the 23-crate workspace (#10204, #10353, #9527, #10363, #10156, #10399, #10514, #10174, #10158) |

## Breaking Changes

- **Typed plugin instance configuration is mandatory.** Plugins that read operator configuration must declare a schema and use the full instance key shown by `zeroclaw plugin info <package>`; legacy package- or binding-only entries are not consulted (#9126).
- **Skill HTTP requests are now fail-closed.** Placeholders are URL-component values and can no longer inject `/`, `?`, `&`, or `#`; redirects and ambient proxy variables are ignored; destinations must resolve directly to admitted public addresses. Update affected skill manifests to use direct URLs and data-only placeholders (#10369).
- **The legacy node transport is retired.** Delete `[node_transport]` from `config.toml`; `[nodes]` remains supported. External Rust users must remove imports of `NodeTransport`, `sign_request`, and `verify_request`, and should rotate the retired secret anywhere it was reused (#10289).
- **TodoWrite display configuration moved to ZeroCode.** Copy `[todotracker]` values into `<config-dir>/zerocode-config.toml`, replace recognized `ZEROCLAW_todotracker__*` variables with `ZEROCODE_todotracker__*`, then remove the daemon section (#9013).
- **The WATI channel was removed.** Move deployments to WhatsApp Cloud or WhatsApp Web; stale WATI configuration remains only long enough to emit migration guidance (#9571).
- **The Aardvark transport and legacy robot-kit crates were removed from the workspace.** Users that still need the in-tree integration should remain on v0.8.4; independently published library versions are unaffected (#9853).
- **The root Cargo package is now named `zeroclaw`.** The installed binary name is unchanged, but contributor scripts using `cargo ... -p zeroclawlabs` must switch to `-p zeroclaw` (#9835).

## Contributors

@Aarlington
@abhinavmathur-atlan
@aldoeliacim
@alucryd
@anagnorisis2peripeteia
@AngryPacifist
@arbelonson-source
@ATECHPCS
@Audacity88
@belumume
@binyangzhu000-sudo
@blockballr
@cakeni
@CedricConday
@DanielSwift1992
@Darren2030
@ggettert
@gregnazario
@h03-xydt
@huwei-xydt
@IftekharUddin
@JasonColapietro
@jmoneytech-stack
@joalvaradon
@JordanTheJet
@joshuavetos
@jstar0
@jxxralf
@Kryptonzy
@legokichi
@leomem
@linhongyu510
@lugu
@MannXo
@mazhuima
@metalmon
@minato32
@mov-xound-glitch
@nightcityblade
@Nillth
@NiuBlibing
@original4422
@ozpool
@perlowja
@Project516
@REL-mame
@RyanHoldren
@RyanSquared
@sbenedetto
@sebkraemer
@Shaurya2k06
@siabang35
@Silentpartnercoding
@singlerider
@sonytricoire
@sunlit-deng
@Super-Cabbage
@Sushanth012
@taljeon
@tidux
@tonsiasy
@tzy-17
@ump45nose
@vikng-dev
@VladimirLewisII
@vrurg
@wangmiao0668000666
@WeeLi-009
@wromansky
@yablokolabs
@yijunyu
@ZiBibro
@zyw02

## Full Changelog

The 454-commit range contains 57 feature, 277 fix, 13 refactor, 2 performance, 27 documentation, 20 CI, 29 test, 28 chore, and one prefixless security-fix commit. Internal maintenance entries are grouped above when user-visible and remain available individually in the full comparison.

**Full diff:** [Compare v0.8.4...v0.8.5](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.8.4...v0.8.5)
