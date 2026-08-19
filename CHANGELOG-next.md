# ZeroClaw v0.8.4

ZeroClaw v0.8.4 is a maintenance and hardening release spanning **262 commits** from **49 contributors**. It expands the memory and SOP control planes, improves provider and channel reliability, strengthens sandbox and credential boundaries, and makes the desktop and release pipeline more dependable. This release also includes upgrade-sensitive changes for skill sources, Nextcloud Talk, and generic webhook authentication; review [Breaking Changes](#breaking-changes) before upgrading those integrations.

## Highlights

- **Memory controls** add opt-in retrieval caching, reranking, typed-fact extraction, migration-aware configuration checks, content scanning, and auditable recall paths.
- **SOP execution** gains per-SOP admission policies, quorum-based approval brokering, checkpoint editing, centralized fan-in adapters, and stronger deterministic gate handling.
- **Operator surfaces** add dashboard-driven upgrades, channel relinking, agent renaming, runtime-context display, searchable keybinding help, and a more reliable terminal experience.
- **Channels and ACP** gain Mattermost WebSocket listening, Telegram debounce, interactive reply events, signed Nextcloud Talk replies, agent-selectable ACP sessions, and cited blob delivery.
- **Provider and runtime reliability** improves OpenAI Responses setup, model context-window metadata, fallback visibility, streamed retries, multimodal fallback, lifecycle preservation, and whole-turn history trimming.
- **Release integrity** adds a self-contained notarized macOS desktop path, repaired Scoop and AUR publication workflows, consolidated attestations, required SBOMs, checksums, and an offline verification archive.

## What's New

### Memory and Retrieval

- Add opt-in retrieval caching, a gated rerank stage, typed memory classification and fact extraction, configuration validation, migration reindex hooks, and content scanning at write and recall boundaries (#8897, #8895, #8900, #8899, #8984).
- Add gated memory audit trails and restore semantic recall of durable global memories across sessions (#8893, #8898).
- Improve Lucid process control with configurable timeouts and more reliable ARM startup handling (#9105).

### SOP and Automation

- Add per-SOP admission policy, release execution slots while awaiting human approval, and broker approvals through group membership and quorum (#8848, #8880).
- Add checkpoint editing and revision for deterministic gate prompts, then centralize fan-in ingress adapters (#8979, #9205).
- Improve conditional steps, AMQP ingress, nested policy handling, structured output, and deterministic approval routing (#8771, #9183, #9027, #9030, #9375, #8903).
- Add raw stdout delivery for cron shell jobs through `shell_output_format` (#8438).

### Dashboard, Desktop, and CLI

- Add in-app upgrades with automatic restart, channel-owned relinking, agent renaming, active runtime context, skill-to-editor navigation, searchable keybinding help, and a unified risk-profile permission grid (#8173, #8734, #7954, #9011, #8558, #9356, #8879).
- Improve terminal chat, code-block copy, picker and overlay sizing, help behavior, and reasoning-only turns (#8920, #8767, #9173, #9008, #9169, #9279, #9292, #9234).
- Embed the dashboard in the universal macOS desktop sidecar and prepare the final DMG for notarization, stapling, and offline validation when Apple credentials are configured (#9032, #9014).
- Reduce the desktop webview's IPC authority by removing shell and store plugin permissions from gateway-served content (#9033).

### Channels, Gateway, and ACP

- Add Telegram inbound debounce, poll and interactive-reply events, structured QR-login lifecycle events, and channel-owned readiness probes (#8440, #6297, #8622, #8732).
- Add Mattermost WebSocket listener mode and LAN peer-discovery hints (#9141, #8325).
- Improve QQ and Matrix behavior, restore single-provider voice transcription, persist WhatsApp Web linked identities, and strengthen channel media and alias handling (#9180, #9153, #8735, #9145, #9495).
- Let ACP sessions select an agent with `?agent=` and accept blob resources with cited file delivery (#9026, #9195).
- Validate required credentials for enabled Signal and Voice Call aliases while continuing to allow incomplete disabled aliases (#9524).

### Providers and Runtime

- Native Anthropic requests now detect Fable safety-classifier refusals and fall back client- or server-side with a user-visible notice (#9262, #9263, #9265, #9266, #9268).
- The web dashboard agent chat now surfaces that safeguard-fallback switch as an inline notice before the answer, matching the messaging-channel footer (#9272).
- Default newly created persisted OpenAI model slots to the Responses API with native tool calling, without changing existing persisted entries or bare provider references (#9021).
- Propagate OpenAI Responses usage and carry model context-window metadata from the models.dev catalog (#9360, #9347).
- Add configurable model vision capabilities and improve provider timeouts, streamed retries, tool-call argument handling, multimodal fallback, and Gemini thought-signature preservation (#9099, #8947, #9113, #9372, #8931, #9102, #8935).
- Surface direct-turn fallback notices and isolate model and provider changes correctly across live sessions and turns (#8684, #8845, #9232).
- Nest memory and RAG spans under the turn trace, and classify web-search provider HTTP failures with precise `search_status` values (#8752, #8890).
- Preserve complete lifecycle events, trim history by whole turns, and prevent duplicate streamed narration (#9490, #9007, #8951).
- Recommend the capability-bounded `local_small` runtime profile for newly configured local model providers while leaving existing agent configurations unchanged (#8987).

### Configuration, Logging, and Tools

- Preserve partial configuration during salvage and improve dotted-map keys, nested values, dirty-path resolution, and patch error handling (#8836, #9309, #9297, #9243, #9310, #9296).
- Serialize configuration writes and reject unsafe or invalid inputs without panics (#9312, #9059, #9441).
- Align the inert `context_compression.enabled` schema default with actual runtime behavior and warn when obsolete compression fields are authored (#9299).
- Preserve JSONL rows during automatic schema migration, including mixed-schema files, malformed rows, unterminated tails, reloads, and concurrent accepted writes (#9449).
- Prevent `ask_user` hangs, improve MCP multiplexing, and normalize Windows paths passed to external search tools (#9452, #9418, #9497).

### Security and Supply Chain

- Apply feature-gated Linux Landlock restrictions in the spawned child rather than restricting the ZeroClaw daemon, with scoped workspace and system access and fail-closed required paths (#9233, #9114).
- Stop OAuth delegate targets from inheriting a coordinator's global credential, and reject constrained fulfillments that omit their merchant or payee subject (#8571, #9327).
- Fix Nostr denial-of-service handling for malformed NIP-04 IVs and relay verification-cache poisoning (RUSTSEC-2026-0219 and RUSTSEC-2026-0224) (#9531, #9622).
- Prevent shell injection through the release workflow's `workflow_dispatch` tag input (#9165).
- Consolidate release provenance around GitHub attestations, required SPDX and CycloneDX SBOMs, final checksums, and one offline verification archive (#9211).
- Repair Scoop and AUR publication paths and rely on Homebrew Core's external autobump service instead of a project-owned Homebrew publisher (#9295).
- **Known advisory posture:** RUSTSEC-2026-0222 remains waived for Wasmtime 45.0.3 because ZeroClaw uses one process-wide engine and does not expose the advisory's cross-engine store precondition. The dependency upgrade remains tracked separately (#9586, #8519).

## Bug Fixes

| Area | Fix |
|---|---|
| Runtime | Preserve complete native tool-call/result turns during retries, retain lifecycle events, trim history at complete-turn boundaries, and avoid duplicate streamed narration (#9372, #9490, #9007, #8951) |
| Providers | Improve timeout propagation, streamed retry handling, multimodal fallback, Gemini thought signatures, and direct-turn fallback reporting (#8947, #9113, #8931, #9102, #8935, #8684) |
| Configuration | Preserve partial aliases during salvage, support dotted map keys, serialize writes, surface dirty-path failures, and avoid panics on invalid input (#9309, #9297, #9312, #9243, #9441) |
| Channels | Send Nextcloud Talk replies through the signed bot API; restore voice transcription; improve QQ, Matrix, WhatsApp Web, media, and alias behavior (#9181, #9153, #9180, #8735, #9145, #9495) |
| Security | Correct Landlock ownership, credential fallback, fulfillment constraints, desktop IPC permissions, and Nostr advisory handling (#9233, #9114, #8571, #9327, #9033, #9531, #9622) |
| Logging | Preserve all accepted JSONL rows through automatic schema migration and propagate explicit flush failures (#9449, #9500) |
| Tools | Prevent interactive prompt hangs, improve MCP routing, and strip Windows verbatim prefixes before invoking external search tools (#9452, #9418, #9497) |
| Hardware | Resynchronize serial frames and bound robot audio subprocess waits (#9157, #9087) |
| Install and release | Repair package publication, embed the macOS dashboard sidecar, and prepare notarized and stapled DMG output (#9295, #9032, #9014) |

## Breaking Changes

- **ClawHub skill sources were removed.** The built-in `clawhub:<slug>` source and `clawhub.ai` URLs are no longer accepted. Existing installed skills are not deleted. Install new skills from a local path, registry name, ordinary Git URL, or use `<git-url> --skill <name>` to select a skill from a catalog repository (#8638).
- **Nextcloud Talk now requires the signed Talk Bot API.** Deployments need Nextcloud 27.1 or later with Talk 17.1 or later and must configure the installed bot's shared secret as `webhook_secret`. `bot_token` remains a deprecated alias for the same value; `app_token` is accepted for parsing but is unused. Missing or conflicting secrets reject inbound webhooks with `401` and suppress outbound sends. Draft streaming settings remain parse-compatible but replies are final-only (#9181).
- **Generic webhook channels require a secret.** An enabled `[channels.webhook.<alias>]` without `secret` now refuses to start, including deployments behind private binds or reverse proxies (#8725).

## Contributors

@alexandme
@Alix-007
@amrrs
@AngryPacifist
@Audacity88
@belumume
@bglusman
@chengzhichao
@ConYel
@Darren2030
@desertjinn
@Diwak4r
@drbparadise
@fanchanghu
@fjqz177
@IftekharUddin
@JordanTheJet
@jstar0
@Lusitaniae
@MannXo
@mazhuima
@metalmon
@minato32
@mwqgithub
@Nillth
@NiuBlibing
@octo-patch
@ozpool
@palomyates516-alt
@perillamint
@perlowja
@Project516
@Rhoahndur
@ryanlee486
@singlerider
@Stalesamy
@Super-Cabbage
@tidux
@tomatotomata
@tzy-17
@vrurg
@wangmiao0668000666
@WeeLi-009
@WilShi
@wm0018
@xianshishan
@xydt-juyaohui
@yanchenko
@yijunyu

## Full Changelog

**Full diff:** https://github.com/zeroclaw-labs/zeroclaw/compare/v0.8.3...v0.8.4
