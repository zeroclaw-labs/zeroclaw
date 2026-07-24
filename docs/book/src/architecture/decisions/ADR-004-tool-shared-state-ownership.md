---
id: ADR-004
title: Tool-held shared state follows daemon-owned identity and handle ownership
date: 2026-03-22
status: accepted
relates-to:
  - https://github.com/zeroclaw-labs/zeroclaw/issues/4057
  - crates/zeroclaw-runtime/src/tools/mod.rs
  - crates/zeroclaw-tools/src/canvas.rs
  - crates/zeroclaw-tools/src/reaction.rs
  - crates/zeroclaw-api/src/tool.rs
---

# ADR-004: Tool-Held Shared State Follows Daemon-Owned Identity And Handle Ownership

This is a restored retroactive record. The original ADR was added under
`docs/architecture/adr-004-tool-shared-state-ownership.md` and was
dropped during the mdBook migration. The code paths have moved into
workspace crates since the original record; this restored version keeps
the accepted decision intact while updating path references where useful.

## Context

ZeroClaw tools execute in a multi-client environment where a single
daemon process can serve multiple connected clients and agent sessions.
Some tools need long-lived shared state:

- delegate tools keep handles to parent tools;
- channel-facing tools keep handles to channel maps;
- canvas tooling keeps shared display state;
- future tools may keep rate limiters, connection pools, credential
  handles, or session-scoped caches.

Those states cannot all be treated the same way. Some are legitimate
shared display or registry state. Some are security-sensitive and must
be isolated per client or session.

Without a shared contract, new tools risk introducing duplicate state,
data leaks between clients, stale state after reloads, or blocking
startup validation in the wrong lifecycle phase.

## Decision

Tools may own long-lived shared state when they follow the handle
pattern and respect daemon-owned identity, isolation, lifecycle, and
reload rules.

### 1. Ownership

When a tool legitimately owns shared state, it uses a cloneable handle
passed at construction time, usually an `Arc<RwLock<T>>` or a narrow
wrapper around one.

Examples in the current workspace include:

| Handle | Current location | Purpose |
| --- | --- | --- |
| `DelegateParentToolsHandle` | `crates/zeroclaw-runtime/src/tools/mod.rs` | Parent-tool list for delegate agents |
| `PerToolChannelHandle` | `crates/zeroclaw-runtime/src/tools/mod.rs` | Per-tool channel map handle |
| `ChannelMapHandle` aliases | `crates/zeroclaw-tools/src/ask_user.rs`, `poll.rs`, `reaction.rs` | Tool-local channel maps |
| `CanvasStore` | `crates/zeroclaw-tools/src/canvas.rs` | Shared canvas frames |

Tools that need shared state must:

- define a named handle type or wrapper;
- accept the handle at construction time;
- document the concurrency and ownership contract;
- avoid global mutable state for per-request or per-client data.

### 2. Identity

The daemon owns client and session identity. Tools must not construct
their own durable client identity keys from transport details such as IP
addresses, headers, usernames, or channel-specific sender strings.

Tools that need per-client namespacing consume identity assigned by the
daemon or receive an already scoped handle. Tools that do not need
per-client isolation may ignore the identity surface, but they must not
invent a parallel one.

### 3. Lifecycle

Tool lifecycle has four phases:

1. Construction: instantiate with handles and config-derived inputs. Do
   not perform blocking network or filesystem validation.
2. Registration: register in the tool registry. A tool may perform
   startup validation if that validation is required before use.
3. Execution: handle a single request. Avoid blocking validation or
   registry rebuilds in this path.
4. Shutdown: clean up owned resources through `Drop` or an explicit
   shutdown method when the owner provides one.

Validation state derived from config, credentials, policy, or external
resources must be invalidated when the source changes. Non-security
display state may survive reloads only when the reload does not affect
its validity.

### 4. Isolation

State that can leak credentials, policy, quotas, user data, or session
data must be isolated per client, agent, or session according to the
owning surface. A shared handle must not store per-client secrets unless
the key space is scoped by daemon-owned identity.

State that is naturally shared, such as broadcast display state, read-only
registry data, or channel handles, may be shared across clients. When it
uses string keys, it should support namespace prefixing or trace metadata
so operators can still filter by client, agent, channel, or session.

### 5. Reload Semantics

Config-derived validation and caches are invalid after the relevant
config, credential, policy, workspace, or provider source changes. The
tool must either re-resolve from the source of truth at use time or
receive a fresh handle/config-derived value from the owner.

The reload rule is about validity, not registry mutation. A tool may
keep non-security display state across reloads only when the reload does
not affect that state's validity.

## Consequences

Positive consequences:

- Tool-owned state becomes discoverable and auditable.
- Security-sensitive data has a named isolation requirement.
- Runtime reload behavior has a clear invalidation rule.
- New tools can reuse the handle pattern without inventing global state.
- Reviewers can ask for the source of truth before accepting a new tool
  field or cache.

Negative consequences:

- Tools that look like simple singletons must still reason about client,
  agent, and session identity.
- Some older handles need migration when the daemon identity surface or
  reload model changes.
- The handle pattern is not enough by itself; ownership and canonical
  state still have to be named in review.

## References

- [Built-in tool inventory](../../developing/tool-inventory.md)
- [Issue #4057](https://github.com/zeroclaw-labs/zeroclaw/issues/4057)
- `AGENTS.md`
- `crates/zeroclaw-runtime/src/tools/mod.rs`
- `crates/zeroclaw-tools/src/ask_user.rs`
- `crates/zeroclaw-tools/src/poll.rs`
- `crates/zeroclaw-tools/src/reaction.rs`
- `crates/zeroclaw-tools/src/canvas.rs`
- `crates/zeroclaw-api/src/tool.rs`
- `crates/zeroclaw-gateway/src/lib.rs`
