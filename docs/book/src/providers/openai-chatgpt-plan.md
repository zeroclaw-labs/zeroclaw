# ChatGPT Plan Usage

This opt-in provider uses OpenAI's open-source Sign in with ChatGPT flow.
It is separate from [Codex subscription authentication](./openai-codex-subscription.md).
The provider supports text and client-side function tools in ZeroClaw's normal
agent loop. `auth plan-check` remains a tools-disabled text check. Custom tools,
hosted tools, credential imports, device login, and managed hosting are
unsupported. Synthetic tests establish the protocol integration;
live account entitlement and model availability require a separately authorized
smoke test.
Native Windows is unsupported in this slice; use Linux, macOS, or WSL.

## Sign in and bind a provider

Use a separate instance directory. Login creates a stable opaque host identifier
in its existing auth profile store before displaying the authorization URL.
Open the URL in a browser on the same machine, review the ChatGPT plan-use grant,
and complete the loopback callback while the command is running.

```sh
zeroclaw --config-dir /path/to/new-instance auth login \
  --model-provider chatgpt-plan --profile subscriber
```

Add an explicit provider reference to that instance's `config.toml`:

```toml
[providers.models.openai.subscriber]
kind = "chatgpt-plan"
model = "<account-visible-model-slug>"
wire_api = "responses"

[providers.models.openai.subscriber.chatgpt_plan_auth]
registration = "chatgpt-plan:subscriber"
```

The credential store owns the issued client ID, verified subject, scopes,
expiry, and encrypted tokens. Config contains only the reference. A different
account or workspace needs a separate profile label. Reauthorizing a saved
profile reuses its issued client ID; a different client or subject is rejected
without replacing its credentials. A missing plan-use scope fails closed.

Global `auth use` selection cannot redirect a plan provider. Keep
`requires_openai_auth` unset: it retains its existing Codex meaning. API keys,
custom endpoints and headers, provider fallbacks, reliability API-key rotation,
temperature, and output-token limits cannot be combined with this provider.

## Verify one text response

Select a model visible to this account; the normal model-catalog command reads
the bound account's catalog:

```sh
zeroclaw --config-dir /path/to/new-instance models refresh \
  --model-provider openai.subscriber
zeroclaw --config-dir /path/to/new-instance auth plan-check \
  --model-provider openai.subscriber --message "Reply READY"
```

The check consumes the selected account's ChatGPT allowance. It uses public
`https://api.openai.com/v1/responses`, sends `store:false` and `stream:true`,
and succeeds only on `response.completed`. The HTTP request and SSE body share
a 300-second total deadline. Truncation, incomplete responses, revocation and
quota failures surface as errors; no metered fallback is selected.
This check establishes text inference for that request; it does not exercise
the agent's tool registry or approval policy.

## Run the normal agent loop

Run `zeroclaw --config-dir /path/to/new-instance quickstart`, select the existing
`openai.subscriber` provider, and choose the normal risk and runtime presets for
the new agent. The agent references that provider explicitly. Select YOLO only
for an instance where full autonomy is intended; it removes approval gates and
workspace scoping. The ordinary tool registry, scoped policy, approvals,
receipts, and cancellation path continue to own execution.

```sh
zeroclaw --config-dir /path/to/new-instance agent \
  --agent <configured-agent-alias> --message "Read the disposable fixture in your workspace"
```

Function schemas are grouped in the `zeroclaw` namespace. The provider does not
execute tools: it returns admitted function calls only after a completed
Responses stream. The runtime then runs the available local tools and sends
their results in the next request. Calls to an unoffered tool, the wrong
namespace, incomplete or malformed calls, and unsupported output types fail
closed. Setting `native_tools = false` disables native function negotiation and
rejects directly supplied structured tool specs. The normal loop may use its
existing prompt-guided protocol in that mode, but tool follow-ups without
provider-issued provenance cannot be resumed. Tool availability and approvals
remain owned by the agent's scoped policy; this transport flag does not narrow
the registry. The text check remains available.

Every follow-up resends local history, including function-call IDs, corresponding
outputs, and opaque reasoning items. No `previous_response_id` or hosted
conversation is used. Replay is bound to the originating issued client and
validated account identity, using an opaque digest from the same canonical
credential snapshot as the request's bearer. Account identifiers stay in the
auth store. Each returned call carries this digest in local history metadata,
so the check survives retained history that omits opaque reasoning. History
without these provider-issued stamps requires a fresh compatible conversation.
Cross-registration, mismatched, duplicate, or orphaned tool history is rejected
before inference egress. Completed call IDs cannot be reused in later rounds,
and a new response cannot reuse an ID from supplied history. Switching accounts
requires an explicitly chosen registration and fresh compatible history.

Local MCP wrappers and the local discovery function remain ordinary client-side
functions under the same policy. The provider does not emit Responses
`tool_search`, hosted MCP, custom tools, programmatic tool calls, or other hosted
execution features. Tools are serialized from the effective per-request registry,
with `strict:false` so their existing parameter schemas are preserved.

An executed harmless local tool followed by a completed final response verifies
that agent/tool path for the selected account and model. A model catalog entry
alone does not establish entitlement. Both text and tool checks consume ChatGPT
allowance; quota or revocation never changes the billing route.

## Credential lifecycle and rollback

Tokens are always encrypted for this grant, including when the legacy
`secrets.encrypt` preference is disabled. Profile writes use private atomic
temporary files and reject credential leaf symlinks. Kernel-held refresh locks
coordinate processes sharing an instance, reread current state after acquisition,
and retain replacement refresh tokens. Separate roots remain independent;
directory aliases to the same root coordinate together.

On Unix, canonical store operations also hold a kernel lease on
`auth-profiles.guard`. While held, they publish `auth-profiles.lock` as a hard
link to that gate so older releases still observe their existing exclusion
protocol. After a process dies, a current release can reclaim only a sentinel
whose inode matches the locked gate, including death during marker or token
replacement. Normal release removes the sentinel, so a clean downgrade can
acquire the legacy lock. Keep the gate file in place; deleting or replacing lock
files while processes use the instance breaks this coordination.
Lock interoperability does not preserve plan fields in older serializers. Keep
the separate instance on a plan-aware release; an older credential writer can
discard registration metadata and refresh uncertainty if it rewrites that store.

A PID-only sentinel left by a pre-upgrade release cannot be safely reclaimed
automatically: older writers do not participate in the kernel lease. Such a
sentinel remains fail-closed whether its PID is live or dead. Stop all processes
using the instance before an operator repairs that legacy lock. A filesystem
without hard-link or kernel-lock support fails closed. These guarantees cover
process death on one host, not power loss or distributed filesystem locking.

Refresh automatically near expiry, or use `auth refresh --model-provider
chatgpt-plan --profile subscriber`. CLI `auth logout` for these registrations
is explicitly unsupported in this slice; disconnect the app in ChatGPT settings.
Remove the explicit provider binding before retiring the instance. Do not turn
it into an API-key profile implicitly. Keep `kind = "chatgpt-plan"` with the
binding: older factories reject this implementation marker, protecting against
accidental API-key billing after a downgrade.

An unresolved refresh transaction blocks reuse until fresh sign-in. The store
records a refresh-start marker before egress and clears it only together with
the committed replacement tokens, including after interrupted processes.

OpenAI documents [registration](https://developers.openai.com/siwc/token-sharing-open-source/sign-in),
[sessions](https://developers.openai.com/siwc/token-sharing-open-source/profiles-and-sessions),
[token metadata](https://developers.openai.com/siwc/token-sharing-open-source/token-reference),
[inference](https://developers.openai.com/siwc/token-sharing-open-source/models-and-inference),
and [preview restrictions](https://developers.openai.com/siwc/token-sharing-open-source/preview-limitations).
The public API documents [function namespaces](https://developers.openai.com/api/docs/guides/function-calling)
and [stateless reasoning replay](https://developers.openai.com/api/docs/guides/reasoning).
