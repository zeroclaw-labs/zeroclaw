# ZeroClaw Claude Code onboarding plugin

This local plugin provides `/zeroclaw:onboard`, read-only status/plan tools and a guarded apply tool. It previews named provider, agent and risk references, then creates a fresh instance through `zeroclaw native-onboard` and the distinct `claude_code_native` provider. Native Code owns login, refresh and billing. The legacy `claude-code` alias continues to invoke direct Anthropic HTTP.

## Prerequisites and loading

Use unmodified Claude Code **2.1.289+**, **Python 3.9+** as `python3`, and a ZeroClaw build containing `native-onboard` and `claude_code_native` on the same Mac, Linux or WSL host. Released binaries may lack these new capabilities. Status checks the installed command's required flags. The helper uses Python's standard library and downloads nothing. Native Windows probing is unsupported; select a separate WSL account/session explicitly. Missing Python permits written guidance only.

Native Code must be able to access its own credential store. A restricted macOS process can report logged out when the host's Keychain login is inaccessible; use the intended native host context rather than exporting or resetting credentials.

Native-default selection and an explicit directory override are distinct authentication choices. On macOS, setting `CLAUDE_CONFIG_DIR` even to the apparent default directory can select a different credential namespace. The default provider preserves native default selection and removes ambient overrides at invocation; an explicitly selected or inherited override remains its literal directory reference. Canonical paths are used for overlap checks rather than changing that native selector.

```sh
claude --plugin-dir /absolute/path/to/integrations/claude-plugin
```

Invoke `/zeroclaw:onboard`; `/mcp` should show `plugin:zeroclaw:preflight`. The package follows [native plugin layout][components] and [distribution][publish]. Session loading changes no persistent Claude settings. Web chat cannot run the local helper. Desktop/Cowork and account sync remain unverified. A synthetic bare-session check proved local skill/MCP discovery without real account access or inference.

## Read and plan contract

`bootstrap.status` accepts an optional existing absolute `claude_config_dir` reference and `expected_billing` (`subscription`, `api`, or `cloud_or_gateway`). It runs version/auth-status commands with literal argv and preserves the parent environment. It never reads credential files, logs in, switches the host conversation's account or prints identity/token fields. Only recognized enums, the login boolean and constructed billing facts leave preflight. Environment indicators disclose presence only; conflicts/custom endpoints keep billing ambiguous for native `/status` review.

Native auth status exits 1 when logged out. Only that exact command/exit pair with `loggedIn: false` is admitted, reconstructed as a minimal boolean payload. Other failures and malformed/unknown states remain sanitized failures or unknown billing. See [native authentication][auth] and the [CLI contract][cli]. It also probes `zeroclaw native-onboard --help`; missing/incompatible commands cannot report `bootstrap_cli_status: available`. Help and auth status prove neither applied configuration nor inference. Package version/minimum Code version come from the bounded packaged manifest; invalid metadata fails closed.

`bootstrap.plan` requires a fresh absolute `instance_root`, `provider_alias` and `agent_alias`. Optional inputs include `claude_config_dir`, `model`, `expected_billing`, `risk_preset`, `accept_yolo`, `engine_backend` and `accept_api_billing`. Roots must not exist, overlap the native account or have a symlinked/missing parent. Directory references reject traversal/control characters. Aliases begin with a lowercase letter, use lowercase letters/digits/underscores and have at most 48 characters. Model IDs are bounded and cannot become CLI options.

The native route returns `requires_configuration`, the real provider reference `claude_code_native.<provider_alias>`, and this argument-array terminal handoff:

```sh
zeroclaw --config-dir /absolute/fresh-instance native-onboard \
  --client claude-code --provider-alias personal --agent-alias assistant \
  --model default --risk-preset balanced --expected-billing subscription
```

`default` retains native model selection. Native API billing requires `expected_billing: api, accept_api_billing: true`; it keeps native Code. Explicit `engine_backend: anthropic_api, accept_api_billing: true` selects the separate direct-HTTP/interactive-Quickstart route. Fallback is never silent. Credentials belong in native login/masked terminal prompts, never plan/argv/chat.

Risk defaults to `balanced`. `yolo` requires the exact boolean `accept_yolo: true` and passes `--accept-yolo`. The helper references canonical `RISK_PRESETS` and copies no policy values. A preview always reports effective policy `unresolved`. The native-onboard transaction owns fresh-root admission, authentication and canonical Quickstart apply. Its receipt may be `pending_auth`, `configured`, `ready` or `failed`; `ready` requires bounded real inference and inspected effective policy. Recheck the fresh root before execution.

`bootstrap.apply` accepts the same native plan inputs plus the exact boolean `confirm_create: true`. It rejects the separate HTTP route, unknown arguments and missing creation/risk/billing choices before execution. Native authentication and expected billing must match; the installed binary must expose the canonical command and native client choice. Executable lookup uses absolute PATH entries, pins the canonical paths admitted by preflight and rechecks their filesystem identity before apply. These checks do not authenticate a binary's publisher. It invokes only fixed `native-onboard` arguments with stdin/stdout/stderr disconnected from chat. The command owns all configuration writes and verifies effective policy and a bounded engine reply.

The apply result reports `ready` only after exit 0 and a matching owner-only, regular, non-symlink receipt containing a fresh validation observation. During resume, the previous receipt descriptor remains open until the result is read; the canonical command must publish a new inode. This distinguishes same-second validations from an unchanged old receipt. Nonzero exit, stale/malformed receipts, cancellation or timeout cannot prove readiness. The observation describes that successful check; subsequent calls still resolve native authentication and live policy.

The apply child has a 150-second deadline; prerequisite probes retain their own bounds. MCP cancellation, connection closure and server termination interrupt the owned session. The supervisor reserves the session leader's PID without reaping it, signals normal native children even in separate process groups, and cleans the session before releasing its leader. PID-only system discovery and signal permission are checked before inference. Missing supervision support fails closed. Children deliberately creating another session remain outside this guarantee; this is normal child cleanup, not adversarial process isolation. Owned instance state is retained. To recover, explicitly select `resume: true` and repeat the same choices; canonical CLI admission rejects arbitrary pre-existing roots or request changes.

The helper installs no service and never reads credentials or writes config itself. Installation uses the canonical ZeroClaw installer with an operator-selected prefix and `--no-modify-path --skip-quickstart`; released binaries lacking the new command remain incompatible. It needs neither the historical `zeroclaw-bootstrap` binary nor control-MCP.

## Native inference and permissions

The provider sends the full ZeroClaw conversation and existing prompt-guided tool protocol over stdin to the installed, unmodified client. Each turn checks version/auth/billing, then uses `--safe-mode`, no built-in tools, all native/MCP tools denied, empty strict MCP configuration and no persisted/resumed native session. ZeroClaw remains the sole agent/tool loop: its normal tools enforce canonical risk policy. `yolo` never passes a native permission bypass flag. Administrator-managed policy/hooks and OS controls retain authority. `--bare` skips subscription OAuth/keychain credentials and cannot serve this account-backed path.

The adapter never opens credential files or moves native tokens to HTTP. Native authentication methods remain available; unknown/mismatched billing fails before inference. Native settings can change between probe and request, so Code remains authoritative for final account selection/billing.

Input is capped at 1 MiB and stdout at 4 MiB; each call has a configured deadline. Native stderr/raw errors are discarded. Cancellation kills the POSIX process group; descendants detaching into another group are outside that boundary. Native result text/token usage are returned. Model listing, vision, token streaming, exact replay and stable request identity are not claimed. Native cost estimates differ from actual subscription/API/cloud billing. The [native-client terms][terms] require an unmodified client, native sign-in and direct end-user billing without credential collection/brokerage.

## Verification and rollback

```sh
python3 -I -B -m unittest discover -s integrations/claude-plugin/tests -v
cargo test -p zeroclaw-providers --lib claude_code_native
claude plugin validate --strict integrations/claude-plugin
```

Synthetic tests cover process argv/stdin/account pointers, billing distinctions, hostile output, deadlines/cancellation, missing/incompatible binaries, real provider/factory/history paths and preserved existing config/account sentinels. Live login/inference/effective-policy acceptance is separate from these checks. End the session to unload the plugin. An instance created by native-onboard remains independently owned: stop its processes and retain its root for review or use canonical instance management to remove it. Plugin removal neither deletes that instance nor logs out Code.

[components]: https://code.claude.com/docs/en/plugins/components
[publish]: https://code.claude.com/docs/en/plugins/publish
[auth]: https://code.claude.com/docs/en/authentication
[cli]: https://code.claude.com/docs/en/cli-reference
[terms]: https://code.claude.com/docs/en/legal-and-compliance
