# Google Workspace Operation Allowlist

Date: 2026-03-19
Status: Implemented
Scope: `google_workspace` wrapper only

## Problem

The current `google_workspace` tool scopes access only at the service level.
If `gmail` is allowed, the agent can request any Gmail resource and method that
`gws` and the credential authorize. That is too broad for supervised workflows
such as "read and draft, but never send."

This creates a gap between:

- tool-level safety expectations in first-party skills such as `email-assistant`
- actual runtime enforcement in the ZeroClaw wrapper

## Current State

The current wrapper supports:

- `allowed_services`
- `credentials_path`
- `default_account`
- rate limiting
- timeout
- audit logging

It does not currently support:

- declared credential profiles for `google_workspace`
- startup verification of granted OAuth scopes
- separate credential files per trust tier as a first-class config concept

## Goals

- Add a method-level allowlist to the ZeroClaw `google_workspace` wrapper.
- Preserve backward compatibility for existing configs.
- Fail closed when an operation is outside the configured allowlist.
- Make Gmail-native draft workflows possible without exposing send methods in the wrapper.

## Non-Goals

This slice does not attempt to solve credential-level policy gaps in Gmail OAuth.
Specifically, it does not add:

- OAuth scope introspection at startup
- credential profile declarations
- trust-tier routing across multiple credential files
- dynamic operation discovery

Those are valid follow-on items, but they are separate features.

## Proposed Config

```toml
[google_workspace]
enabled = true
default_account = "owner@company.com"
allowed_services = ["gmail"]
audit_log = true

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "messages"
methods = ["list", "get"]

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "threads"
methods = ["get"]

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "drafts"
methods = ["list", "get", "create", "update"]
```

Semantics:

- If `allowed_operations` is empty, behavior stays backward compatible:
  all resource/method combinations remain available within `allowed_services`.
- If `allowed_operations` is non-empty, only explicit `(service, resource, method)`
  combinations are allowed.
- Service-level and operation-level checks both apply.

## Operation Inventory Reference

The first question operators need answered is not "where is the canonical API
inventory?" It is "what string values are valid here?"

For `allowed_operations`, the runtime expects this exact shape:

- `service`: the same service identifier used in `allowed_services` and the
  first `gws` command segment
- `resource`: the Google API resource name used by that service
- `method`: the operation name used on that resource

Mental model:

```text
gws <service> <resource> <method> ...
```

maps to:

```toml
[[google_workspace.allowed_operations]]
service = "<service>"
resource = "<resource>"
methods = ["<method>"]
```

Examples:

| CLI shape | Config entry |
|---|---|
| `gws gmail messages list` | `service = "gmail"`, `resource = "messages"`, `method = "list"` |
| `gws gmail drafts create` | `service = "gmail"`, `resource = "drafts"`, `method = "create"` |
| `gws calendar events list` | `service = "calendar"`, `resource = "events"`, `method = "list"` |
| `gws drive files get` | `service = "drive"`, `resource = "files"`, `method = "get"` |

Verified starter examples for common supervised workflows:

- Gmail read-only triage:
  - `gmail/messages/list`
  - `gmail/messages/get`
  - `gmail/threads/list`
  - `gmail/threads/get`
- Gmail draft-without-send:
  - `gmail/drafts/list`
  - `gmail/drafts/get`
  - `gmail/drafts/create`
  - `gmail/drafts/update`
- Calendar review:
  - `calendar/events/list`
  - `calendar/events/get`
- Calendar scheduling:
  - `calendar/events/list`
  - `calendar/events/get`
  - `calendar/events/insert`
  - `calendar/events/update`
- Drive lookup:
  - `drive/files/list`
  - `drive/files/get`
- Drive metadata and sharing review:
  - `drive/files/list`
  - `drive/files/get`
  - `drive/files/update`
  - `drive/permissions/list`

Important constraint:

- This spec intentionally documents the value shape and a small set of verified
  common examples.
- It does not attempt to freeze a complete global list of every Google
  Workspace operation, because the underlying `gws` command surface is derived
  from Google's Discovery Service and can evolve over time.

When you need to confirm whether a less-common operation exists:

- Use the Google Workspace CLI docs as the operator-facing entry point:
  `https://googleworkspace-cli.mintlify.app/`
- Use the Google API Discovery directory to identify the relevant API:
  `https://developers.google.com/discovery/v1/reference/apis/list`
- Use the per-service Discovery document or REST reference to confirm the exact
  resource and method names for that API.

## Runtime Enforcement

Validation order inside `google_workspace`:

1. Check rate limits and action budget.
2. Check `service` against `allowed_services`.
3. Check `(service, resource, method)` against `allowed_operations` when configured.
4. Reject any invalid identifiers.
5. Build and execute the `gws` command.

This must be fail-closed. A missing operation match is a hard deny, not a warning.

## Data Model

Add a config type:

```rust
pub struct GoogleWorkspaceAllowedOperation {
    pub service: String,
    pub resource: String,
    pub methods: Vec<String>,
}
```

Add to `GoogleWorkspaceConfig`:

```rust
pub allowed_operations: Vec<GoogleWorkspaceAllowedOperation>
```

## Validation Rules

- `service` must be non-empty, lowercase alphanumeric with `_` or `-`
- `resource` must be non-empty, lowercase alphanumeric with `_` or `-`
- `methods` must be non-empty
- each method must be non-empty, lowercase alphanumeric with `_` or `-`
- duplicate methods within one entry are rejected
- duplicate `(service, resource)` entries are rejected

## TDD Plan

1. Add config validation tests for invalid `allowed_operations`.
2. Add tool tests for allow-all fallback when `allowed_operations` is empty.
3. Add tool tests for exact allowlist matching.
4. Add tool tests that deny unlisted operations such as `gmail/drafts/send`.
5. Implement the config model and runtime checks.
6. Update docs with the new config shape and the Gmail draft-only pattern.

## Example Use Case

For `email-assistant`, the safe Gmail-native draft profile is:

- allow:
  - `gmail/messages/list`
  - `gmail/messages/get`
  - `gmail/threads/get`
  - `gmail/drafts/list`
  - `gmail/drafts/get`
  - `gmail/drafts/create`
  - `gmail/drafts/update`
- deny:
  - `gmail/messages/send`
  - `gmail/drafts/send`

This still is not a credential-level send prohibition. It is a strong runtime
boundary inside the ZeroClaw wrapper.

## Follow-On Work

Future credential-hardening work should be tracked separately:

1. Declared credential profiles in `google_workspace` config
2. Startup verification of granted scopes against declared policy
3. Multiple credential files per trust tier
4. Optional profile-to-operation binding
