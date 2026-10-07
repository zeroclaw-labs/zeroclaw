# Claude Code onboarding protocol

The package at `integrations/claude-plugin/` is a local Claude Code plugin with a user-invoked onboarding skill and Python standard-library stdio MCP helper. It reads status, previews a plan and applies an explicitly accepted native instance through `zeroclaw native-onboard`. The resulting `claude_code_native` provider runs the unmodified native client.

## Ownership and canonical sources

Native Code owns credential selection/storage, sign-in, renewal, managed policy and final billing. Neither helper nor provider opens credential files, collects account tokens or brokers credentials into an HTTP adapter. Directory inputs are references to existing native state. The native-client [terms][terms] permit an end user to authenticate in the unmodified binary using their own subscription, API or cloud credentials, with direct billing to that user. General third-party/API integration and native-client authentication remain distinct.

The legacy `claude-code` alias retains its direct Anthropic HTTP meaning. The new family is explicitly `claude_code_native`; its canonical typed config is `ClaudeCodeNativeModelProviderConfig` in `zeroclaw-config/src/schema.rs`, registered by `for_each_model_provider_slot!`. Fields are `binary_path`, `claude_config_dir` and required `expected_billing`, plus the common provider base. Generic `api_key`/`uri` are refused by the native factory. Native API authentication belongs to Code's own flow. Native billing mismatch/unknown state fails before a request; an API fallback requires an explicit choice.

Risk choices resolve from `zeroclaw_config::presets::RISK_PRESETS`. The helper copies no preset policy. Its preview always reports effective policy `unresolved`; `yolo` requires the exact explicit boolean `accept_yolo`. The shared command owns exclusive fresh-root admission and canonical Quickstart `stage_apply_checked`/`complete_staged_apply`, including materialized risk and named provider/agent references. This plugin therefore depends on the shared native-onboard foundation, not historical bootstrap/control-MCP code. Native host, administrator and OS permissions retain their own authority.

## Native provider and agent loop

`crates/zeroclaw-providers/src/claude_code_native.rs` is the inference adapter. The config's existing parent directory supplies subprocess cwd; it creates no second workspace policy. Each call verifies native version/auth/expected billing, sends the complete ZeroClaw conversation over stdin, and parses a structured native success result/token usage.

ZeroClaw owns conversation persistence, the sole agent loop, tool approval and tool dispatch. The adapter uses the existing prompt-guided tool protocol for normal ZeroClaw operations. Native Code receives no built-in or MCP tools: `--tools ''`, `--disallowedTools '*'`, empty strict MCP configuration and `--safe-mode` disable native customizations. Session settings disable unmanaged hooks; administrator-managed policy/hooks retain native authority. `yolo` changes canonical ZeroClaw risk, without native permission-bypass flags. `--bare` is unsuitable because it skips subscription OAuth/keychain credentials.

Native sessions are fresh and use `--no-session-persistence`, never resume/continue. Full history, including prior tool results, comes from the runtime on each request. Input is capped at 1 MiB, output at 4 MiB; the configured deadline is bounded to one hour. Concurrent stdin write/stdout capture/process wait prevent pipe deadlocks. Cancellation/error/timeout kills the POSIX process group; detached new groups are outside the guarantee. Native stderr and raw error payloads are discarded.

Native result text/token usage are exposed. Vision, token streaming, model listing, exact replay and stable model identity are not claimed. The factory retains unstable identity because native model selection/fallback remains opaque. Native cost estimates do not establish actual subscription/API/cloud charges. External native account/settings can change after a probe; Code remains authoritative for final authentication and billing.

## Plugin transport and handoff

The `.mcp.json` entry starts `python3 -I -B` with a packaged script path as one argv element. Python 3.9+ and local Code 2.1.289+ on Mac/Linux/WSL are prerequisites. The helper downloads nothing. Native Windows is rejected before discovery/spawn; missing Python permits written guidance only. Desktop/Cowork and account sync are unverified.

Manifest version and minimum native version are read from the bounded packaged `.claude-plugin/plugin.json`. Missing/invalid metadata produces fixed errors without raw file/parser output. The [stdio MCP transport][transport] uses newline-delimited JSON-RPC; stdout is protocol-only. `bootstrap.status` and `bootstrap.plan` are read-only; `bootstrap.apply` is mutating and may consume selected model usage. Only one apply runs at a time. A worker executes it while the transport accepts cancellation notifications; connection closure or server termination cancels the active operation.

Status runs native version and auth-status commands. Native logged-out exit 1 is admitted only for the fixed auth-status argv and exactly `loggedIn: false`, reconstructed as a minimal boolean payload. Identity/tokens/unknown fields are discarded. Conflicting selectors and unknown methods/providers retain unknown billing. It also probes `zeroclaw native-onboard --help`; only required flags establish an available handoff, never working inference.

Plans validate a nonexistent absolute root under an existing canonical parent, disjoint from the native account. Aliases/models are bounded and reject option injection; `yolo` and API billing require explicit acceptance. The native handoff supplies provider/agent/model/risk/billing choices to the shared command. The explicit HTTP alternative remains an interactive Quickstart handoff. Rendering shell commands must quote every returned argv element. Plans cannot reserve a root or undo a later human command.

Apply requires the same native plan inputs and exact boolean `confirm_create: true`. Native authentication/billing and command capabilities must pass preflight. Absolute-only executable discovery pins the admitted paths and rechecks filesystem identity before mutation; it does not authenticate their publisher. It launches fixed canonical CLI argv, with all raw streams disconnected from chat, preserving native-default selection or the selected literal directory override. It writes neither credentials nor a parallel configuration. The apply child has a 150-second deadline; each prerequisite probe retains its separate five-second bound.

Cancellation interrupts the owned POSIX session. The packaged supervisor uses fixed PID-only discovery and checks signal permission before inference, observes exit without reaping the session leader, and cleans ordinary native children in other groups before releasing the leader PID. Normal completion also cleans remaining session members. Deliberately detached sessions are excluded, and macOS membership rechecks do not provide adversarial PID-race isolation. Unsupported supervision fails closed.

Only command exit 0 plus a matching private CLI receipt and fresh engine observation can report `ready`. Receipt reads reject symlinks, nonregular files, foreign ownership, public permissions, excessive size and substituted requests. A held prior receipt descriptor prevents inode reuse; resume requires a new canonical atomic publication, even within the same second. Failure, invalid/stale receipt, timeout and cancellation cannot prove readiness. Explicit `resume: true` delegates recovery admission to the canonical CLI, which requires its exact owned transaction and unchanged choices. Existing configured roots are never admitted merely by the helper's resume flag. Owned state is retained after incomplete setup.

MCP input is capped at 32 KiB/frame and output at 16 KiB. Probe stdout is capped at 64 KiB, stderr goes to the null device, and each probe has a five-second deadline. Timeout/overflow kills the POSIX process group. No native Windows cleanup promise is made. The helper retains no raw command output in files/logs.

## Validation and rollback

Package tests spawn the actual stdio helper and synthetic native executables. Provider tests exercise real subprocess argv/stdin/account references, complete history/tool instructions, per-turn billing checks, result/usage parsing, output bounds, timeout/cancellation and factory/legacy-alias boundaries. Test fixtures preserve existing account/config sentinels. Mutation checks must prove each safety guard and restore exact source checksums. Native plugin validation and synthetic session discovery prove package loading; authenticated login, bounded inference and inspected effective policy require separate live acceptance.

The shared setup receipt distinguishes `pending_auth`, `configured`, `ready` and `failed`. Only live bounded engine inference plus inspected effective policy can establish `ready`; neither auth status nor a plan is sufficient. Unload the session plugin to remove its helper/skill. Instances already created through the canonical transaction remain independently owned; plugin removal does not delete their state, stop their processes or log out Code.

See the native [authentication contract][auth] and [CLI reference][cli] for sign-in, billing precedence and supported flags.

[transport]: https://modelcontextprotocol.io/specification/2025-06-18/basic/transports
[auth]: https://code.claude.com/docs/en/authentication
[cli]: https://code.claude.com/docs/en/cli-reference
[terms]: https://code.claude.com/docs/en/legal-and-compliance
