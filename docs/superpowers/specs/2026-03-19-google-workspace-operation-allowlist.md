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

Gmail uses a 4-segment gws command shape (`gws gmail users <sub_resource> <method>`),
so `sub_resource` is required for all Gmail entries. Drive and Calendar use
3-segment commands and omit `sub_resource`.

```toml
[google_workspace]
enabled = true
default_account = "owner@company.com"
allowed_services = ["gmail"]
audit_log = true

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "messages"
methods = ["list", "get"]

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "threads"
methods = ["get"]

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "drafts"
methods = ["list", "get", "create", "update"]
```

Semantics:

- If `allowed_operations` is empty, behavior stays backward compatible:
  all resource/method combinations remain available within `allowed_services`.
- If `allowed_operations` is non-empty, only exact matches pass. An entry matches
  a call when `service`, `resource`, `sub_resource`, and `method` all agree.
  `sub_resource` in the entry is optional: an entry without `sub_resource` matches
  only calls with no sub_resource; an entry with `sub_resource` matches only calls
  with that exact sub_resource value.
- Service-level and operation-level checks both apply.

## Operation Inventory Reference

The first question operators need answered is not "where is the canonical API
inventory?" It is "what string values are valid here?"

For `allowed_operations`, the runtime expects `service`, `resource`, an optional
`sub_resource`, and `methods`. The values come directly from the `gws` command
segments in the same order.

3-segment commands (Drive, Calendar, Sheets, etc.):

```text
gws <service> <resource> <method> ...
```

```toml
[[google_workspace.allowed_operations]]
service = "<service>"
resource = "<resource>"
# sub_resource omitted
methods = ["<method>"]
```

4-segment commands (Gmail and other user-scoped APIs):

```text
gws <service> <resource> <sub_resource> <method> ...
```

```toml
[[google_workspace.allowed_operations]]
service = "<service>"
resource = "<resource>"
sub_resource = "<sub_resource>"
methods = ["<method>"]
```

Examples verified against `gws` discovery output:

| CLI shape | Config entry |
|---|---|
| `gws gmail users messages list` | `service = "gmail"`, `resource = "users"`, `sub_resource = "messages"`, `method = "list"` |
| `gws gmail users drafts create` | `service = "gmail"`, `resource = "users"`, `sub_resource = "drafts"`, `method = "create"` |
| `gws calendar events list` | `service = "calendar"`, `resource = "events"`, `method = "list"` |
| `gws drive files get` | `service = "drive"`, `resource = "files"`, `method = "get"` |

Verified starter examples for common supervised workflows:

- Gmail read-only triage:
  - `gmail/users/messages/list`
  - `gmail/users/messages/get`
  - `gmail/users/threads/list`
  - `gmail/users/threads/get`
- Gmail draft-without-send:
  - `gmail/users/drafts/list`
  - `gmail/users/drafts/get`
  - `gmail/users/drafts/create`
  - `gmail/users/drafts/update`
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

1. Extract `service`, `resource`, `method` from args (required).
2. Extract and validate `sub_resource` if present (type check, character check).
3. Check rate limits.
4. Check `service` against `allowed_services`.
5. Check `(service, resource, sub_resource, method)` against `allowed_operations`
   when configured. Unmatched combinations are denied fail-closed.
6. Validate `service`, `resource`, and `method` for shell-safe characters.
7. Build optional args (`params`, `body`, `format`, `page_all`, `page_limit`).
8. Charge action budget (only after all validation passes).
9. Execute the `gws` command.

This must be fail-closed. A missing operation match is a hard deny, not a warning.

## Data Model

Config type:

```rust
pub struct GoogleWorkspaceAllowedOperation {
    pub service: String,
    pub resource: String,
    pub sub_resource: Option<String>,
    pub methods: Vec<String>,
}
```

Added to `GoogleWorkspaceConfig`:

```rust
pub allowed_operations: Vec<GoogleWorkspaceAllowedOperation>
```

## Validation Rules

- `service` must be non-empty, lowercase alphanumeric with `_` or `-`
- `resource` must be non-empty, lowercase alphanumeric with `_` or `-`
- `sub_resource`, when present, must be non-empty, lowercase alphanumeric with `_` or `-`
- `methods` must be non-empty
- each method must be non-empty, lowercase alphanumeric with `_` or `-`
- duplicate methods within one entry are rejected by validation
- duplicate `(service, resource, sub_resource)` entries are rejected by validation

## TDD Plan

1. Add config validation tests for invalid `allowed_operations`.
2. Add tool tests for allow-all fallback when `allowed_operations` is empty.
3. Add tool tests for exact allowlist matching.
4. Add tool tests that deny unlisted operations such as `gmail/users/drafts/send`.
5. Implement the config model and runtime checks.
6. Update docs with the new config shape and the Gmail draft-only pattern.

## Example Use Case

For `email-assistant`, the safe Gmail-native draft profile is:

```toml
[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "messages"
methods = ["list", "get"]

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "threads"
methods = ["get"]

[[google_workspace.allowed_operations]]
service = "gmail"
resource = "users"
sub_resource = "drafts"
methods = ["list", "get", "create", "update"]
```

Operations denied by omission: `gmail/users/messages/send`, `gmail/users/drafts/send`.

This is not a credential-level send prohibition. It is a runtime boundary inside
the ZeroClaw wrapper.

## Follow-On Work

Future credential-hardening work tracked separately:

1. Declared credential profiles in `google_workspace` config.
2. Startup verification of granted scopes against declared policy.
3. Multiple credential files per trust tier.
4. Optional profile-to-operation binding.
