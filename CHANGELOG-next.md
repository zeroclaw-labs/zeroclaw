# ZeroClaw v0.8.7

ZeroClaw v0.8.7 is a security and maintenance follow-up to v0.8.6 spanning **81 commits** from **19 contributors**. It moves to patched Wasmtime, Rustls certificate verification, `tokio` and `ctutils` releases for advisories that affect v0.8.6, rebuilds the container images with CA bundles that no longer trust the distrusted emSign chains, and fixes five of v0.8.6's plugin known issues and narrows two more, as well as the daemon's refusal of a group-writable data directory. Precompiled plugin components must be rebuilt for Wasmtime 48.0.7; read [Breaking Changes](#breaking-changes) and [Known issues](#known-issues) before upgrading.

## In brief

Security fixes: Wasmtime 48.0.7 and patched TLS verification. Rebuild precompiled plugins.

Plus five plugin known-issue fixes, Telegram rate-limit handling, ZeroCode cost warnings, and chat that reconnects after a reload.

## Highlights

- **Wasmtime and TLS security advisories are fixed.**
  - The plugin host moves to Wasmtime 48.0.7, with the matching Pulley 48.0.7 and Cranelift 0.135.7 (RUSTSEC-2026-0375 to RUSTSEC-2026-0379).
  - TLS certificate verification uses `rustls-webpki` 0.103.16, `webpki-roots` 1.0.10 and `webpki-root-certs` 1.0.10. Affected versions could accept certificate paths through distrusted emSign intermediates whose private keys are known (RUSTSEC-2026-0381, RUSTSEC-2026-0382, RUSTSEC-2026-0383).
  - The optional MQTT channel moves from `rumqttc` 0.25 to `rumqttc-v4-next` 0.34, a maintained MQTT 3.1.1 client on the same patched verifier. That removes the last copy of the legacy `rustls-webpki` 0.102.8 and the four audit exceptions it needed.
  - Precompiled `.cwasm` plugin components built for an earlier Wasmtime must be rebuilt for 48.0.7.

  (#11924, closes #11923; includes the original Wasmtime commit from #11906)
  - `ctutils` moves to 0.4.5 for RUSTSEC-2026-0385 (non-constant-time mask generation and `u128` comparisons), a fix from @mov-xound-glitch (#11998), and `tokio` to 1.53.3 for RUSTSEC-2026-0386 (`AsyncFd::new` and related constructors violate I/O safety) (#12010, closes #12009).
  - Container images are rebuilt from refreshed base images and patched CA packages. Their CA bundles, which native tools such as curl, git and OpenSSL-linked programs read, no longer trust the distrusted emSign chains (#11925, closes #11932).
- **Five of v0.8.6's plugin known issues are fixed, and two more are narrowed.**
  - A plugin connection no longer stalls when the first resolved address never answers, typically an IPv6 route that drops packets. The host tries the next address after 250 ms, alternating address families, as browsers do (#11814).
  - Tool packages that connect over WebSocket or raw sockets get their declared egress seeded at install, and `plugin list` reports what their row is missing, as it already did for HTTP tools (#11808).
  - `zeroclaw config set` takes a non-secret plugin setting, such as Discord's `guild_ids`, as given on the command line. Only settings the package marks secret use the masked prompt (#11812).
  - `plugin remove` removes a loaded plugin from the directory it was loaded from, instead of deleting `<plugins_dir>/<name>` (#11809).
  - Peripheral, skill-defined and MCP capability tool names are reserved before plugin tools register, so a plugin tool can no longer take their calls (#11851).
  - Narrowed: new package lock and lease files are created owner-only on Unix (#11807), and `plugin info` and `plugin list --verify` report a tool whose retained configuration the runtime refuses as not loading (#11811). [Known issues](#known-issues) says what remains.
- **The daemon starts on a group-writable data directory again.** Under umask `0002`, v0.8.6 created `~/.zeroclaw/data` group-writable and then refused it. v0.8.7 accepts the data directory as it is, as v0.8.5 did, and keeps its config lifecycle lock in an owner-only `data/.lifecycle` directory instead. Read the [Breaking Changes](#breaking-changes) before upgrading a running daemon (#11803).
- **ZeroCode warns before the budget runs out and shows what each session is using.**
  - A `warn_at_percent` cost threshold now reaches ZeroCode as a one-time line in the chat, instead of the first sign being a turn refused at 100%.
  - Overview shows the focused session's agent, model, workspace and whether work is running or queued, next to the other session's.
  - Config explains how to add provider profiles and routing hints, and opens the missing required field.

  (#11692, #11513, #11508)
- **Chat stays connected and responsive.**
  - An open dashboard chat reconnects on its own after a daemon reload, instead of answering every message with an error until the page is refreshed.
  - ZeroCode redraws as updates arrive, at up to about 60 frames a second, instead of stalling and jumping when several sessions stream at once.

  (#11431, #11407)
- **Telegram respects rate limits.** When Telegram says "retry after N seconds", ZeroClaw waits and resends the same message, instead of resending at once and lengthening the penalty. A wait over a minute stops the send, with a log line saying why (#11650).

## What's New

### ZeroCode, Dashboard, Desktop and Web

- Surface `[cost] warn_at_percent` warnings in ZeroCode, shown once as a line in the chat; nothing is blocked and no setting changes (#11692).
- Show the focused session's runtime context in Overview. Matching Code and Chat values share a labelled row, different values stay separate, and each context shows whether work is running or queued (#11513).
- Guide Config alias setup:
  - the empty list explains its purpose;
  - the name form says an entry is saved immediately;
  - the editor opens the first missing required field, supplied by the daemon from typed configuration.

  (#11508)
- Desktop app: closing the dashboard window hides it instead of destroying it, so the tray, **Show Dashboard**, **Agent Chat** and relaunching the app bring it back, and tray **Quit** now exits the app (#11802, closes #11087).

### Runtime, Providers and Cost

- Log what filled the context window when a request cannot fit: request and window sizes, and the shares taken by instructions, tool descriptions and history (#11578).
- Honor `native_tools` for every OpenAI-compatible chat-completions family: `true` selects native tool calling, `false` prompt-guided tools; unset keeps the family default (#11599).
- After `model_switch`, tell the model to carry on and deliver the result in the same reply, instead of announcing the switch and stopping (#11385).
- Report torn or unreadable cost-ledger lines in cost summaries, instead of silently undercounting session, daily and monthly spend (#11557).

### Security and Sandboxing

- Apply a risk profile's `firejail_args` to the Firejail wrapper. Entries that would undo ZeroClaw's own Firejail hardening, or are not single `--option` / `--option=value` flags, are rejected, and the sandbox then refuses every command rather than running without them (#11656, closes #11594).
- Name the cause when a requested bubblewrap backend is rejected: a missing or broken launcher, or a build without `sandbox-bubblewrap` (#11559).
- Correct the sandboxing documentation's description of Linux `auto` backend selection and its fallbacks to match the selector (#11591).
- Narrow the memory threat scanner's credential-URL check to URL user info and credential query or fragment parameters, so text that merely contains a link and an unrelated credential word is no longer flagged (#11144).

### Memory

- Push `since`/`until` time bounds into Qdrant vector search, so out-of-window high-scoring points no longer take the requested result slots (#11143).

### Plugins

- Document the official Discord plugin: install and bind by its registry name, the `discord.com` and `*.discord.gg` egress grant, the masked token prompt, routing to an agent, and a maintainer acceptance runbook (#11329).

### Documentation and Dependencies

- Quickstart help for a provider's `api_key` now says the key is encrypted into `config.toml`, with its encryption key in `.secret_key` beside it, instead of claiming the OS keyring (#11840).
- Bump `js-yaml` to 4.3.2 in the dashboard build, outside advisory GHSA-2883-xcg3-v3hh (#10729).
- The arm64 `dist` container image publishes again: its build now gets 8 GiB of swap and a serial build, as the release workflow's Linux ARM64 build does (#11833, closes #11828).

## Bug Fixes

| Area | Fix |
|---|---|
| Channels | Send `initial_prompt` to Groq and OpenAI transcription as the Whisper `prompt` field, as the config documents (#11258) |
| Channels | Parse flat inbound Lark/Feishu `post` messages, so rich-text messages such as typed lists are no longer dropped (#11669) |
| Gateway | Close chat sockets that a reload leaves on a retired instance, so the dashboard chat reconnects (#11431) |
| Plugins | Race a destination's resolved addresses, starting the next one after 250 ms without an answer, so a silent first address no longer holds the connection until it times out. A request that sets no connect timeout now gets 10 s for the TCP connect instead of 600 s (#11814, closes #11813) |
| Plugins | Seed and diagnose declared egress for tool packages that request only `websocket_client` or `socket_client`, as for HTTP tools; Quickstart offers the same grant (#11808, closes #11552) |
| Plugins | `config set` takes a non-secret plugin setting as given on the command line, or prompts with echo when none is given; only properties the package's `config_schema` marks `x-secret` use the masked prompt (#11812) |
| Plugins | `plugin remove` of a loaded plugin removes it from the directory it was loaded from, instead of deleting `<plugins_dir>/<name>` (#11809, closes #11357) |
| Plugins | Reserve peripheral, skill-defined and MCP capability tool names (`mcp_resources`, `mcp_prompts`) before plugin tools register; a plugin tool with one of those names is dropped with a `plugin_tool_name_conflict` warning instead of receiving its calls (#11851, closes #11327) |
| Plugins | Create the package lock and transaction lease files owner-only (`0600`) on Unix, so another local account can no longer hold the package lock, and name the lock file when it cannot be opened (#11807) |
| Plugins | `plugin info` and `plugin list --verify` report a tool whose retained configuration the runtime refuses as "does not load", with the runtime's reason and the row that holds the values (#11811) |
| Plugins | Name the sender in `plugin_channel_sender_denied` records, so an operator can tell which ID to add to the peer group (#11848, closes #11821) |
| Sessions | A daemon's first start on a fresh data directory no longer comes up without its RPC session store: concurrent first opens of `sessions.db` wait for each other, and a failed open is logged and retried once (#11900, closes #11898) |
| Security | An `oidc.<alias>` provider no longer refuses a valid token as misconfigured (`-32012`) while another check is fetching the JWKS; concurrent checks share one fetch. This hit fresh providers after a start or config reload, and every 5-minute key-cache expiry (#11903, closes #11902) |
| Gateway | A stopped gateway admits no new request: after a daemon reload, a connection the old gateway accepted gets `503` with `Retry-After: 1` instead of being served with the old configuration, in-flight work drains within a bound, and the port is released at once (#11901, closes #11899) |
| SOP | A finished run's record is final, and an out-of-date copy of a run can no longer revive or overwrite it: a step result, gate decision or cancel from a stale copy is refused or decided against the stored run, so a completed run no longer starts again (#11861, closes #10066) |
| Tools | After a `model_routing_config` update, probe the saved alias with the effective reloaded configuration, including environment overrides for credentials, endpoint and model; probe errors are sanitized, and a failed probe rolls the alias back (#10034, closes #9592) |
| Config | The daemon accepts an existing data directory as it is and creates a new one owner-only, so a group-writable data directory, such as one created under umask `0002`, is no longer refused (#11803) |
| Prompts | Channel and `zeroclaw agent` system prompts no longer carry the date, so a long chat keeps its prompt cache across midnight (#11801) |
| Providers | Stop over-estimating JPEG decode memory, so ordinary 12 MP phone photos are no longer refused as too large (#11637) |
| Providers | Recognize the `context_length_exceeded` overflow code, so trim-and-retry recovery runs instead of a generic refusal (#11659) |
| Quickstart | Ask for a new provider's endpoint and key before its model, list the models that endpoint serves, and accept a typed model ID the list does not show (#11852, fixes #11819) |
| Tools | Reject malformed AnySearch responses instead of treating them as successful searches, keep meaningful spacing, and route AnySearch errors through Fluent (#11106) |
| Config | Emit static map-key section paths in the generated schema metadata, with no leaked allocations (#11616, closes #11614) |
| Cost | Keep one cost tracker per ledger path, so resolving another ledger or a disabled config no longer resets or stops a ledger's tracker (#11350) |
| Tunnel | Supervise the `tailscale serve` process and print the gateway URL only once Tailscale is actually serving it (#11726) |
| Skills | The open-skills sync never prompts for git credentials, so a daemon started from a terminal no longer hangs on a missing repository (#11804, closes #11796) |
| CLI | Enable UTF-8 erase mode in the interactive REPL so Backspace removes a whole multibyte character (#11148) |
| ZeroCode | Render live session updates promptly under load and re-render only the changed tool card (#11407) |
| ZeroCode | The paused-queue hint names Enter as well as the Alt shortcut, which macOS Terminal types as a character (#11775) |
| Windows | Saving a JSONL session transcript, refreshing the model cache, and saving ZeroCode sidebar changes no longer fail with "Access is denied" while another reader has the file open (#11398, #11400, #11399) |

## Breaking Changes

- **Rebuild precompiled plugin components.** `.cwasm` files compiled for an earlier Wasmtime must be rebuilt for 48.0.7 (#11924).
- **MQTT `keep_alive_secs` must be 0 to 65535.** A larger value now fails listener startup instead of being silently truncated to the protocol's 16-bit field (#11924).
- **MQTT credentials:** when both `username` and `password` are set, empty strings are now sent as explicit empty credentials with their CONNECT flags, instead of being left out. For anonymous access, leave both unset (#11924).
- MQTT broker URLs, `use_tls`, QoS and certificate/hostname verification against the system trust store are unchanged.
- **Plugin connect timeout:** a plugin HTTP request that sets no `connect_timeout` now gets 10 s for the TCP connect instead of 600 s. A timeout the plugin sets is still used as given (#11814).
- **Stop ZeroClaw before upgrading from v0.8.6.** The config lifecycle lock moves from `data/config-lifecycle.lock` to `data/.lifecycle/config-lifecycle.lock`, so v0.8.6 and v0.8.7 processes do not exclude each other. Stop any running `zeroclaw daemon`, `zeroclaw agent` or `zeroclaw acp` before installing v0.8.7, then start it again. The old `data/config-lifecycle.lock` is ignored and can be deleted once no v0.8.6 process is running. The v0.8.6 `chmod go-w` workaround is no longer needed (#11803).
- **SOP runs:** a finished run that an earlier version revived is stored as active, so v0.8.7 restores it and drives its remaining steps. Cancel such runs before upgrading. Upgrade the daemon and any CLI that runs SOPs together: an older binary writing to the same `runs.db` can still reopen a finished run (#11861).
- **Gateway reloads:** a request that reaches a stopped gateway, for example on a connection kept open across a daemon reload, gets `503` with `Retry-After: 1` and `"reason":"gateway_restarting"`. Retry it (#11901).
- **Container images:** pull the v0.8.7 image and recreate containers to get the refreshed CA bundles. An image rebuilt from an earlier release's Dockerfiles keeps the old bundle (#11925).

## Known issues

These issues ship in v0.8.7. None of them weakens a security check; where behaviour is affected, it fails closed.

- **WebSocket routes do not connect over the gateway's built-in TLS listener.** When the gateway serves TLS itself, the server answers a WebSocket upgrade with `101 Switching Protocols` and then closes the connection, so dashboard chat and the other WebSocket routes never connect. This also affects v0.8.6. Terminate TLS in a reverse proxy or tunnel and serve plain HTTP to the gateway instead. The fix is planned for v0.9.0 (#11951). Separately, an upgraded socket outlives a gateway reload on either path (#11709).
- **The host does not record refused WebSocket and socket connections.** A plugin's WebSocket or socket connection to a destination its instance has not been granted is refused, but only HTTP refusals write a `plugin_egress_denied` record with the host and a remedy. For the other two transports only the plugin's own log shows the refusal (#11304).
- **A plugin channel that never reaches its service can stay quiet.** A channel package that cannot reach its server, or whose credential is refused, logs nothing about it at the default level. `/health` reports its component (`channel:plugin.<alias>`) as `starting` with `last_ok: null` for as long as no exchange has succeeded, so treat a component that stays `starting` as unreachable or misconfigured, and check its egress grant and credential.
- **Ten registry channel packages have not run against their live services.** bluesky, gitea, mattermost, mochat, notion, reddit, telegram, twitter, wechat and wecom were checked to install, load, bind, and validate their configuration on a v0.8.6 release-candidate build; none has run against its live service with a real account yet. Discord has (#10999).
- **Package lock files created before v0.8.7 keep their mode.** v0.8.7 creates the package lock and lease files owner-only (`0600`) on Unix but never changes an existing file. A lock created earlier under a permissive umask can still be held by another local account that can read the plugins directory, so install, update and remove fail after 60 seconds. Remove `<plugins_dir>/.zeroclaw-package-lock-v1` while no plugin command is running, and the next one creates it owner-only. A group-shared plugins directory is not supported (#11360).
- **Discovery that overlaps `plugin update` can briefly pair old and new files.** Discovery reads a package's manifest and component separately, so a discovery during an update can pair the old manifest with the new component until the next discovery. Packages whose manifest declares `wasm_sha256` are protected, because admission checks the digest in every signature mode (#11562).
- **An interrupted `plugin install` can leave a hidden stage behind.** If an install is killed after it starts staging the package and before it publishes it, a `.<name>.installing-v1-<id>` directory holding a `lease` file and an empty `package/` stays in the plugins directory, and no command removes it. It does not affect install, update, remove or discovery, and a re-install works. Delete it while no plugin command is running (#11792).
- **A reinstalled package can meet configuration it refuses.** `plugin remove` keeps a package's `[[plugins.entries]]` row and its values, and `plugin install` does not warn when the reinstalled package cannot accept them, so the runtime skips the tool. `plugin info` and `plugin list --verify` now report such a tool as "does not load" and name the row to fix; `plugin update` already re-validates retained entries (#11336).
- **Installing over an installed package is refused.** `plugin install` of a package that is already installed refuses and prints the matching `plugin update` command (#10162).
- **Registry packages have no versioned WIT compatibility check yet.** The registry lists only versions built for this release, and the install-time load check refuses an incompatible package, but a package does not yet declare which host versions it supports.
- **Slack no longer shows "is thinking…" in ordinary channel threads.** Since v0.8.5, an agent mentioned in a channel thread shows only the 👀 acknowledgement reaction until it replies; the status line appears only in Slack's AI-assistant pane. With `stream_drafts = true`, progress appears as a draft message that the final answer replaces (#11416).

## Binary size

Release binary bytes per target triple, as printed by each build job's binary size check with the release workflow's toolchain and target flags. v0.8.6 comes from its release run (run 39095722409, `c473221313`); v0.8.7 from a `dist` Cross-Platform Build of the freeze commit (run 40059806582, `4db7afdcfa`). The ARM32 and Android binaries carry no plugin host. The largest binary, x86_64-unknown-linux-musl, is 76,774,224 bytes, 6.78 MiB under the 80 MiB release cap.

```text
target                               v0.8.6      v0.8.7       delta  delta %  plugin host
x86_64-unknown-linux-gnu         76,621,800  76,715,464     +93,664    +0.1%  yes
x86_64-unknown-linux-musl        76,642,608  76,774,224    +131,616    +0.2%  yes
aarch64-unknown-linux-gnu        55,781,376  55,978,512    +197,136    +0.4%  yes
aarch64-unknown-linux-musl       51,627,424  51,685,280     +57,856    +0.1%  yes
x86_64-apple-darwin              64,339,064  64,408,904     +69,840    +0.1%  yes
aarch64-apple-darwin             42,810,832  42,860,816     +49,984    +0.1%  yes
x86_64-pc-windows-msvc           64,436,736  64,529,408     +92,672    +0.1%  yes
armv7-unknown-linux-gnueabihf    52,989,368  53,063,388     +74,020    +0.1%  no
arm-unknown-linux-gnueabihf      53,329,336  53,382,876     +53,540    +0.1%  no
aarch64-linux-android            45,448,392  45,540,096     +91,704    +0.2%  no
```

## Contributors

@Alix-007
@alucryd
@aprint2008-sys
@Audacity88
@cwahn
@GaijinSystems
@IftekharUddin
@jasen215
@JordanTheJet
@jxxralf
@Leon-SK668
@mov-xound-glitch
@RustLangLatam
@sebkraemer
@tidux
@tmielika
@tunglambk
@ump45nose
@VladimirLewisII

## Full Changelog

The 81-commit range contains 4 feature, 48 fix, 1 refactor, 5 documentation, 5 CI, 13 test, 1 style, and 4 chore commits. Not listed above, because they change nothing users see: test-only changes (#11080, #11140, #11154, #11155, #11157, #11195, #11312, #11337, #11595, #11683, #11688, #11863, #11929), formatting and lint (#11328, #11610, #11838), dependency bumps (#11480, #11563), internal groundwork with no behaviour change (#11352, #11438), CI and release-workflow maintenance (#11832, #11854, #11927, #11941, #11967), review tooling docs (#11141), and an edit to the v0.8.6 notes (#11841).

**Full diff:** [Compare v0.8.6...v0.8.7](https://github.com/zeroclaw-labs/zeroclaw/compare/v0.8.6...v0.8.7)
