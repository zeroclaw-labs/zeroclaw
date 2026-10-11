# Native provider onboarding

`native-onboard` creates a separate instance, authorizes its provider, applies
the canonical Quickstart presets, and checks one bounded engine completion.
The destination directory must not exist. Its parent must exist, and
`--config-dir` must be an absolute path. An interrupted run can resume only
the same owned directory with the identical arguments.

```sh
zeroclaw --config-dir /path/to/new-instance native-onboard \
  --client chatgpt-plan \
  --provider-alias subscriber --agent-alias assistant \
  --model '<account-visible-model-slug>' \
  --risk-preset balanced --expected-billing subscription \
  --auth-profile subscriber
```

Open the printed authorization URL on the same machine and review the ChatGPT
plan-use grant. The callback validates signed identity and stores the encrypted
registration in this instance before any agent configuration is committed.
The configuration binds the provider to that exact registration. It does not
import Codex credentials or select an API-key fallback.

YOLO requires both `--risk-preset yolo` and `--accept-yolo` on each invocation.
It applies the existing canonical YOLO preset, including full autonomy and its
approval and sandbox settings. Balanced is the canonical supervised preset.
Onboarding enables the built-in local CLI and creates the agent workspace. The
memory backend is disabled; embeddings,
model routes, classifier overrides, and alternate summarizer providers cannot
introduce an external API billing path.

The dependent native-provider build exposes `--client claude-code`, `--native-config-dir`, and
`--expected-billing subscription|api|cloud_or_gateway` for the dependent native
provider implementation. A build without that typed provider omits the Claude
client from help and refuses it before creating a directory or starting authorization. API billing
also requires `--accept-api-billing`. ChatGPT plan onboarding accepts only
subscription billing and does not accept `--native-config-dir`.

## Interrupted runs and readiness

`native-onboard.json` is a private ownership receipt with these outcomes:

| Outcome | Meaning |
|---|---|
| `pending_auth` | The command owns this fresh directory; authorization or configuration has not completed |
| `configured` | The canonical configuration was committed; engine validation is still pending |
| `ready` | One bounded real engine completion and effective policy were verified, with a recorded timestamp and model |
| `failed` | Authorization, configuration, cancellation, or engine validation did not complete; no readiness was claimed |

A ready receipt describes the last successful verification. It is not ongoing
account, quota, credential, or policy authority. Normal requests resolve their
current configuration and provider authentication again.

Cancel with Ctrl-C. The command retains its owned authorization and any completed
configuration commit, and never marks a cancelled attempt ready. Rerun the
identical command to resume. If a process dies after configuration commits but
before the receipt advances, the saved intent fingerprint permits verification
and reuse of that configuration. Resumption does not replace an existing
configuration or delete credentials, unrelated files, or the instance.

The receipt is bound to the directory the command created. A copied receipt,
arbitrary precreated directory, request mismatch, or symlink cannot establish
fresh ownership. A kernel lease excludes simultaneous onboarding, while the
existing config-lifecycle ownership guard excludes supported config writers.
Keep lock files in place while any process uses the instance.

This implementation requires macOS or Linux, including WSL, and local
filesystem support for exclusive directory publication and kernel locks.
It installs no global service, copies no native credentials, and changes no
system settings. Actual account entitlement and model availability are checked
by the authorized engine request. A provider build that cannot handle the
engine's request reports failure instead of claiming readiness.
Environment overrides that change the staged configuration are rejected before
persistence, so an ambient override cannot silently replace the accepted preset
or provider binding.
