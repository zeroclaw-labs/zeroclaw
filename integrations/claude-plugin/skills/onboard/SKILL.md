---
name: onboard
description: Check native Claude Code billing, preview a fresh ZeroClaw instance, and apply it with explicit permissions.
disable-model-invocation: true
---

# ZeroClaw onboarding

Guide the operator through a reviewable plan. This plugin supports local Claude
Code. ZeroClaw runs the unmodified native client through its distinct
`claude_code_native` provider. The existing `claude-code` provider alias still
uses direct Anthropic HTTP. Account login stays entirely inside native Code.

1. Check prerequisites: unmodified Claude Code 2.1.289+ and `python3` 3.9+ on
   a local Mac, Linux or WSL host. Code 2.1.289 is the tested minimum.
   Native Windows probing is unsupported and must return `unsupported_platform`
   without starting Claude. Offer written-plan guidance or an operator-selected
   Code session inside WSL; do not automatically launch WSL or copy credentials.
   The bundled helper uses only Python's standard library.
   If the MCP server is unavailable, explain the missing prerequisite and continue
   with a written plan. Do not claim preflight passed or automatically install a
   runtime. In web chat, Desktop Chat or Cowork, hand off to a local Code terminal;
   this package has not verified those execution surfaces.
2. Ask which native account directory the operator intends to use. Omit
   `claude_config_dir` to preserve the session's inherited selection/default.
   An explicitly selected directory must already exist. Call `bootstrap.status`
   on this plugin's `preflight` MCP server with that reference and the expected
   billing mode, normally `subscription`. Never ask for or paste credential
   contents. Report only the helper's sanitized fields, including unknown or
   mismatched billing. A different selected directory checks that directory;
   it does not switch the current host conversation to that account.
3. If native sign-in is needed, give the operator this terminal-only pattern,
   with the selected directory quoted as one argument:

   ```sh
   env CLAUDE_CONFIG_DIR='/absolute/operator-selected/account-directory' claude auth login --claudeai
   ```

   The operator completes Code's own browser/terminal flow. Never run login,
   `/logout`, `setup-token`, or `ant auth login` through this helper, and never
   collect their output. `/login` is also native. `setup-token` is a headless
   native-Code credential, not a ZeroClaw HTTP-provider credential supplied by
   this plugin. `ant auth login` is Console API billing, not included Pro/Max
   usage. Do not unset credential selectors, edit managed settings, or silently
   fall back to an API key. Have the operator inspect native `/status` when the
   helper reports ambiguity. See [native authentication][auth].
4. Ask for a fresh, nonexistent, absolute ZeroClaw root under an existing
   canonical parent, a provider alias, and an agent alias. Choose `balanced`
   as the default supervised risk preset. Select `yolo` only after explicit
   instance-specific consent, represented by `accept_yolo: true`. Explain that
   the preset expands ZeroClaw autonomy and reduces its safeguards; Claude host,
   administrator and OS restrictions retain their own authority.
5. Select `engine_backend: native_claude_code` for native inference. Choose an
   explicit native billing source and model (normally `subscription` and
   `default`, which retains native Code's model selection). Native API billing
   requires `expected_billing: api, accept_api_billing: true`; it keeps the native
   client. The independent direct-HTTP route is available only when explicitly
   chosen as `engine_backend: anthropic_api, accept_api_billing: true`.
   Call `bootstrap.plan` with the root, provider/agent aliases, model, billing
   and risk choice. Never silently change the route or reinterpret connector
   authorization as inference entitlement.
6. Show the returned plan and status. The risk reference is a preset name;
   `effective_policy_status: unresolved` means permissions have not been proven.
   Status and plan are read-only. If ZeroClaw is absent, use its
   [current releases][releases] and [Quickstart][quickstart] to select the canonical
   installer. With installation authorization, run the reviewed installer under
   an operator-selected prefix using `--no-modify-path --skip-quickstart` and
   make its binary available to this session. Do not install an OS service.
   Native setup requires a ZeroClaw build containing
   `native-onboard` and `claude_code_native`; a released binary may lack them.
   `bootstrap.status` reports `bootstrap_cli_status: available` only when the
   installed command exposes the required flags. Missing/incompatible status is
   an unmet prerequisite, never successful installation. This package needs
   neither `zeroclaw-bootstrap` nor control-MCP.
7. Show `terminal_handoff.argv` as an argument array and explain the selected
   billing and permissions. Once the operator has authorized this creation,
   call `bootstrap.apply` with the same plan inputs and `confirm_create: true`.
   Keep native billing and risk choices unchanged. This mutating tool calls
   only `native-onboard`, discards raw CLI output, and may consume model usage.
   Its child deadline is 150 seconds; cancellation cleans ordinary native
   process groups in the owned session. Unsupported supervision stops setup.
   A cancelled/failed instance remains available for inspection and recovery.
   Set `resume: true` only for an explicitly selected previous transaction and
   repeat identical choices; the CLI validates its ownership and saved request.
   If a terminal handoff is needed, quote every argument for the selected shell.
   `zeroclaw native-onboard` owns native auth/preflight and the canonical
   Quickstart apply transaction, including provider/agent references and risk
   preset materialization. Native Code owns login/refresh; the command stores
   only the selected account-directory reference and expected billing.
   For the explicitly selected HTTP route, interactive Quickstart owns
   alias/preset selection; `bootstrap.apply` rejects that route. Never construct
   or write a parallel config file.
   Credentials belong exclusively in native login or masked terminal prompts,
   never in argv, chat or this MCP.
8. Report the apply result accurately. `ready` requires a successful command and
   its matching private, newly published receipt with a bounded engine validation; it records
   the last successful check, not ongoing authentication or quota availability.
   `pending_auth`, `configured`, failed, cancelled and timed-out results are
   incomplete. The plugin's preview never proves inference or effective policy.
   ZeroClaw owns its sole tool loop; native child built-in/MCP tools and
   customizations are disabled for inference. `yolo` changes only the canonical
   ZeroClaw risk preset; native administrator and OS policies retain authority.
   The helper never reads credentials or writes configuration itself.

[auth]: https://code.claude.com/docs/en/authentication
[releases]: https://github.com/zeroclaw-labs/zeroclaw/releases/latest
[quickstart]: https://docs.zeroclaw.com/master/en/getting-started/quickstart.html
