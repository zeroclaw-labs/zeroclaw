# ZeroClaw v0.8.2

ZeroClaw v0.8.2 opens up two new front doors: **A2A agent discovery** for agent-to-agent interop and a richer **skills** story (user-configured extra registries, typed slash-command options). Underneath, the release sharpens ZeroClaw's security posture across plugins, channels, and the SOP runtime, lands a durable run/task control plane, and broadens channel surfaces (Discord interaction components, Slack attachments, WhatsApp group allowlists). It spans 152 commits from 31 contributors. Much of this is invisible at the surface and shows up as fewer leaks, fewer duplicate launches, and turns that behave the same on every transport.

## Highlights

- **A2A agent discovery** (#7763): agents can describe and discover one another over the gateway, opening up agent-to-agent interop.
- **Richer skills story**: user-configured extra skill registries via `registry:<name>/<skill>` (#7827) and typed slash-command options in SKILL.md frontmatter (#8021).
- Installation now adds `zeroclaw` to PATH automatically with a `--no-modify-path` opt-out (#8038).
- Untrusted inbound content is now framed and sanitized before a model ever sees it, both through the new universal ingress policy layer and SOP trigger-payload framing.
- A new durable run/task control plane backs SOP run-state, live run metrics, and delegate/subagent supervision in SQLite.
- Plugins gained an SSRF guard on `zc_http_request`, per-alias config scoping, and removal of raw environment access.
- Discord channels picked up interaction components (buttons, selects, modals, autocomplete, buttoned approval) and rich outbound embeds.
- The Telegram bot token and similar secrets are now redacted through the canonical global leak detector instead of channel-local regexes.

## Security

ZeroClaw treats every inbound payload as untrusted and tightens the seams an attacker would reach for.

- **Universal ingress policy layer** (#7997): every inbound turn passes one SOP-backed policy layer before a model sees it, on every transport including mid-turn steering injections. Always on, default disposition is Loop, behavior identical until a Gate is configured.
- **SOP trigger-payload framing** (#8215): MQTT and webhook trigger topics and payloads are capped, sanitized, and framed in untrusted-content markers behind a security notice, so an injected event cannot forge instructions into the step context.
- **Plugin SSRF guard** (#8128): `zc_http_request` now blocks SSRF, including DNS-rebinding and redirect bypasses, with the host classifier moved to infra.
- **Plugin config isolation** (#8137): plugin config is scoped per-alias, raw env access is removed, and caller-supplied `__config` is stripped before injection.
- **Telegram token redaction** (#8127): every Telegram error site routes through the canonical leak detector, which gained a `/bot<id>:<token>` pattern, closing token leaks via reqwest error Display.
- **MCP tool scoping** (#8120): MCP tools are scoped per-agent and the denylist is enforced across all connect sites, including the gateway.
- **Principal type and AuthProvider seam** (#8063): the shared authenticated-subject contract and pluggable inbound-auth seam from RFC #7141 land with no production call sites yet, so runtime behavior is unchanged.
- **HMAC tool receipts** (#8009): HMAC tool receipts are wired through the ACP, gateway WS, and CLI turn paths.
- **WhatsApp MAC storage** (#7912): app-state mutation MACs are stored raw rather than JSON-wrapped, fixing a verification regression.
- **Authenticated self-test probe** (#7732): the websocket handshake probe now authenticates instead of relying on an unauthenticated path.
- **A2A task auth** (#8274): A2A task invocation now requires auth while discovery cards stay public.
- **Delegate sub-tool gating** (#8284): delegate sub-tools run under the parent's SecurityPolicy.

## Gateway

- A2A agent discovery surface (#7763).
- xAI OAuth login support (#7945).
- Email-login subcommand for OAuth2 email channel auth (#8008).
- Device registration on legacy `/pair` with backfill of orphaned paired tokens (#7993).
- Agent rename is persisted before owned state is moved (#7940).
- The gateway drains before RPC reload (#8104).
- Dashboard Skills page reflects an agent's effective skills (#7963).
- Provider and channel alias deletes cascade through referencing surfaces (#8074).
- The reserved `default` agent cannot be created across operator surfaces (#8098).
- Option-backed tunnel providers surface in the picker (#8026).
- `enabled` is accepted on `CronPatchBody` for pause and resume, with the agent check scoped to shell-command patches (#7666).

## Skills

- User-configured extra skill registries via `registry:<name>/<skill>` (#7827).
- Typed slash-command options in SKILL.md frontmatter (#8021).
- `ZEROCLAW_SESSION_ID` exposed to skill shell tools (#8035).
- Plugin-bundled and bundled skills load via `read_skill` (#7245).
- `truncate_output` guards against UTF-8 char boundaries (#7962).

## Install and Update

- `zeroclaw` is added to PATH automatically, with a `--no-modify-path` opt-out (#8038).
- Windows self-update repaired and the update pipeline hardened (#7853).
- Intel versus Apple Silicon detection for the prebuilt target triple (#8096).

## Runtime and Engine

- Durable run/task control plane with delegate and subagent supervision (#8217).
- `ResolvedAgentExecution::resolve` routes the production turn paths (#8179), with per-agent ToolLoop fields bundled into it (#8156) and the loop args bundled into a ToolLoop struct (#7969).
- History pruning and compression were removed in favor of a single whole-turn trim with a visible RPC event (#8196).
- Self-contained context-compression summary provider (#7973).
- System prompt refreshes on tool dispatcher swap (#8126).
- Native and MCP tools are presented to reasoning models in the system prompt (#8053).
- Streamed narration no longer duplicates before native tool calls (#8014).
- Missing-skill suggestions are based on the effective tool set in the `process_message` path (#7819).
- Cached extra registry skills are now suggested (#8185).
- Agent-loop log events are categorized and verb-tagged (#8067).
- Path-listing tool results are gated from vision routing (#7345); the no-vision capability error is scoped to the latest user image (#8180).
- Config alias renames cascade safely across referencing surfaces (#8109).
- Channel, `agent_alias` and `turn_id` propagate to agent lifecycle observer events (#7771).
- Repeated shell approval loops are bounded (#7901).
- Auto-approved tools are allowed on channels at non-Full autonomy (#7959).

## SOP

- Durable SQLite run-state store with live run metrics (#8206).
- `SopRunStore` trait plus an in-memory backend as EPIC B scaffolding (#8001).

## Plugins

- Plugin docs aligned with the WIT target (#8061), alongside the SSRF guard and per-alias config scoping covered under Security.

## Channels

- **Discord**: interaction components including buttons, selects, modals, buttoned approval, and autocomplete (#7965); rich outbound embeds from `[EMBED:{...}]` markers (#7833); slash command localizations and guild scope (#7922).
- **Slack**: outbound attachment uploads (#7170).
- **WhatsApp**: per-JID `allowed_groups` group allowlist for Web mode (#7720).
- **Lark**: restored outbound media markers (#8113).
- Scope-selectable `/model` overrides (user or agent) for chat channels (#7998).
- Tool-result content is preserved when proactively trimming channel history (#8050).
- Bound channels are suppressed when their owning agent is disabled (#8051).
- Voice channels no longer cache config-derived `static_voice_peers` on the channel handle (#7982).
- **Matrix**: restored room management tool (#8068).
- Per-sender `/thinking` overrides restored (#8011).
- Re-loadable media refs preserved in cached history (#8153).
- `refreshed_new_session_system_prompt` loads bundled skills (#8203).

## Web and Dashboard

- Themed click-to-open config pickers via a Select primitive (#8086).
- Component-health fix-in-place modal (#8087).
- Config-alias rename plus delete cascade preview (#7919).
- Config drift conflict surfaced on the enable and disable toggle (#8042).

## ZeroCode and TUI

- Aliases and Costs tabs on the provider alias list (#8006).
- Registry-driven pane help, themed code-fence syntax highlighting, per-fence copy, and unified split-pane config navigation (#8282).
- Daemon version mismatch detection (#8192).
- MCP initialized for Chat TUI sessions (#8199).
- Active config directory surfaced in the Config header (#7999).
- Approval overlay background filled (#7823).
- Queue-paused hint skipped when the backlog is empty (#7857).
- Chat surface refresh: mode bar and code-block chrome, browse-mode badge, and mouse click-to-copy (#8000).
- Selected field is visually distinguished from the editable input (#7995).
- Queue pauses when a turn is cancelled (#8214).
- Browse mode enter/exit moved to alt+shift+up/down (#8166).

## Cost and Budget

- Budget config is reloadable instead of frozen at boot (#8004).
- Model cost captured for RPC, zerocode TUI, and standalone ACP turns (#7953).
- Agent turn costs are persisted (#7957).
- Logs correlate by `trace_id` with per-call `cost_usd` recorded (#8065).
- Opt-in LLM request payload capture, default off (#8066).

## Knowledge and Memory

- Client relationship graph actions restored (#8182).
- Embedding key decoupled from the chat provider, surviving embed failures (#7942).
- SQLite sessions are kept out of hygiene archives (#8318).

## Presets

- Balanced redefined as the trusted-local daily driver (#8133).
- The yolo preset is fully unrestricted (#8281).

## Bug Fixes

| Area | Fix |
|---|---|
| config | Gate Android shell import on non-Windows (#8189) |
| tools | Normalize Windows workspace-prefixed paths (#8114) |
| tools | Resolve external coding tool `working_directory` from project root (#7967) |
| tools/image | Expose stable attachment paths in image-generation output (#7985) |
| tools/git_operations | Add recovery hint and path context to non-repository error (#7835) |
| cron | Claim and release in-flight lock to prevent duplicate launches (#8107) |
| model_switch | Resolve `list_models` from the live models.dev catalog with the hardcoded list as offline fallback (#8097) |
| daemon | Handle file-descriptor exhaustion (EMFILE) in the IPC accept loop (#7983) |
| providers | Strip assistant reasoning on outbound replay for Groq (#7616) |
| providers | Enable vision support for the NVIDIA NIM provider (#8100) |
| providers | Update the Kimi Code endpoint to api.kimi.com/coding/v1 (#8163) |
| providers | Expose `replay_assistant_reasoning` and fallback tool-call handling (#8232) |
| providers | Coalesce stripped compatible history roles (#7931) |
| notion | Propagate header parse errors instead of unwrapping (#8147) |
| browser | Repair WebDriver snapshot returns and CSS selector escaping (#7908) |
| log | Make same-timestamp pagination deterministic via byte-offset cursor (#7921) |
| tools | Add a content_search internal fallback (#8060) |
| doctor | Pass Config to provider_validation_error for custom providers (#8084) |
| config | Warn when `a2a.exposed_skills` resolves no skills (#8283) |
| cli | Persist the model in config on `models set` instead of probing providers (#7094); refresh non-default channel guidance (#7955) |
| web_fetch | `allowed_private_hosts = ["*"]` covers DNS-resolved private hosts (#7412) |
| skills | Correct the "ClawhHub" typo in skill installer messages (#8262) |
| docker | Keep Node base policy in container TOML (#8112); correct Node 24 digest pins (#7932); drop stale aardvark-sys build.rs COPY (#8092) |

## Docs

- Define the external integration boundary (#8184).
- Rewrite and fix setup.bat known issues (#6102); fix dead Windows quick-start link breaking the docs build (#8085).
- Move translation catalogues to a git submodule (#8169); avoid stale placeholder warning translations (#8194).
- Align the extension point overview (#7880) and plugin docs with the WIT target (#8061).
- Standardize label spelling (#8111); remove stale guild override wording (#8108).
- Quiet rustdoc warning links (#8191).

## CI and Tooling

- Run the docs link gate in PR checks (#8197).
- Build base Dockerfiles from source on container changes (#8093).
- Drive container base pins from a canonical TOML (#8005).
- Add an advisory cross-platform clippy workflow (#7885).
- Stop the Kilo labeler matching shared provider files (#8106).
- Pass the provider-dispatch gate and `--all-features` build on master (#8019).
- Gate aardvark-sys behind the hardware feature (#8028); drop the unused rumqttc dependency (#8077); unyank bitcoin crates in Cargo.lock (#7992).

## Tests

- Pin hook panic recovery and cancellation propagation (#8041).
- Regression for poisoned activated-tool lock recovery (#7845).
- Cover blank-input turn rejection (#7859).
- Cover storage-reader timestamp and ordering edge cases (#7916).
- Make screenshot expectations platform-aware (#8183); make process fixtures portable on Windows (#7956).
- Pin the system prompt in the cache-hit test to kill a date flake (#8036).

## Contributors

@Alix-007
@Audacity88
@ConYel
@danielO99
@databillm
@drbparadise
@eldar702
@FTDGRT
@hanZeng-08
@IftekharUddin
@joe2643
@jokewithme110
@JordanTheJet
@legokichi
@MaHaoHao-ch
@mazhuima
@mov-xound-glitch
@Nillth
@NiuBlibing
@OmkumarSolanki
@perlowja
@Pick-cat
@RyanHoldren
@sbenedetto
@singlerider
@theredspoon
@tidux
@wangmiao0668000666
@xianshishan
@yuxuan-7814
@ZOOWH

**Full diff:** https://github.com/zeroclaw-labs/zeroclaw/compare/v0.8.1...v0.8.2
