---
name: onboard
description: Create a new local ZeroClaw instance using a signed ChatGPT plan grant and an explicit risk preset. Use for ZeroClaw setup in ChatGPT Work or Codex with local terminal access.
---

# Set up ZeroClaw with a ChatGPT plan

Use the native ZeroClaw onboarding command. It owns the fresh instance, browser
grant, configuration commit, and recovery state. Plugin installation alone does
not grant inference or configure an instance.

Require a local terminal and browser on the same machine. In a surface without
local execution, explain that setup must continue in ChatGPT Work or Codex with
local access. This package does not create hosted infrastructure.

Check `zeroclaw --help` and `zeroclaw native-onboard --help`. The installed binary
must expose `native-onboard` and its ChatGPT plan client. If missing, use the
project's supported installer/release instructions; do not replace an active
installation with a source build or invent a package-specific config writer.

Choose a new absolute instance path, provider alias, agent alias, and the user's
requested model. Resolve the risk choice for this particular instance:

- `balanced` uses the normal canonical supervised preset.
- `yolo` uses the normal canonical full-autonomy preset: tools run without
  approval gates or workspace scoping. Supply `--accept-yolo` only when the user
  explicitly selected YOLO for this instance. A preference on another instance
  does not select it here.

Run the native command with arguments as separate argv entries, preserving the
chosen path and aliases:

```text
zeroclaw --config-dir ROOT native-onboard
  --client chatgpt-plan
  --provider-alias PROVIDER_ALIAS
  --agent-alias AGENT_ALIAS
  --model MODEL
  --risk-preset balanced
  --expected-billing subscription
  --auth-profile subscriber
```

For an explicitly selected YOLO instance, replace the risk value with `yolo` and
add `--accept-yolo`. Do not pass an API billing acceptance flag for this flow.

Let the command claim the root and perform Sign in with ChatGPT. Open its
authorization URL in the local browser, and let the user review and grant plan
usage. Do not perform a separate pre-login in an arbitrary root. Never read,
copy, export, or import Codex credentials, auth-profile contents, OAuth tokens,
or browser cookies. Do not send the callback URL into chat or logs.

Follow the native command's recovery instructions after cancellation or failure.
An owned incomplete transaction may be resumed, but is not a ready instance.
Do not delete locks, force an overwrite, switch billing, or run a second writer
to complete it. Preserve a committed configuration if a later side effect fails.

On success, read only the reported nonsecret references and effective policy.
Confirm that the provider is `openai.PROVIDER_ALIAS` with `kind = "chatgpt-plan"`,
the agent names that provider explicitly, and the reported risk matches the
selected canonical preset. Config contains the registration reference; the
canonical encrypted auth store owns the grant and account identity.

When validation is included in the request, use the bound account's model catalog
and run the smallest requested harmless check. A text check is:

```text
zeroclaw --config-dir ROOT models refresh --model-provider openai.PROVIDER_ALIAS
zeroclaw --config-dir ROOT auth plan-check --model-provider openai.PROVIDER_ALIAS --message "Reply READY"
```

For tool-loop validation, run the configured agent with an explicitly requested
harmless local action, such as reading a disposable fixture inside its workspace.
This uses the instance's normal tool registry and approval path. These checks
consume the selected account's ChatGPT allowance. A listed model alone does not
prove entitlement; require a completed inference response. Never select an API
key or fallback after a quota, revocation, or capability error.

Report the instance path, explicit provider/agent/risk references, configuration
outcome, and the checks actually completed. Keep configuration success separate
from live inference and tool verification. Link to ChatGPT Settings → Usage for
app usage and access management, without claiming a remaining balance.
