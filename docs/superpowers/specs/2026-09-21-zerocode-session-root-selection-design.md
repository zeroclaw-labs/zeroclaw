# ZeroCode Session Root Selection Design

**Date:** 2026-09-21
**Follow-up issue:** [#10826](https://github.com/zeroclaw-labs/zeroclaw/issues/10826)
**Parent decision:** [PR #10565](https://github.com/zeroclaw-labs/zeroclaw/pull/10565)

## Context

PR #10565 was an interim compatibility repair for the regression tracked by #10609. It made fresh local Code sessions inherit the directory from which `zerocode` was launched, while preserving agent-workspace behavior for Chat and explicit directory selection for remote Code. The merged review comments deliberately deferred a shared root-selection contract.

The follow-up must make root selection explicit without changing the root of a running session. A session's saved directory is coupled to its transcript, sandbox boundary, and tool state, so moving an active session in place would create an ambiguous and unsafe state.

## Goals

1. Make the selected agent's configured workspace the default root for fresh sessions when no explicit directory is supplied.
2. Preserve the daemon's saved root when resuming an existing Code session, regardless of the current launch directory or later agent-workspace changes.
3. Let Code users explicitly select a directory for a new local or remote session.
4. Add `/change-directory` as a user-facing path that starts a distinct Code session in the selected directory rather than re-rooting the active session.
5. Keep explicit directory selection and session creation on the existing `session/new` RPC boundary, with the daemon's returned `workspace_dir` remaining the canonical session-root state.
6. Correct the ZeroCode running guide so Code resume behavior is distinguished from Chat reattachment behavior.

## Non-goals

- Keep the fail-closed posture of the #10565 CWD-capture fix: a selected root that cannot be represented is a reported error, never a silent fallback to another directory. The interim behavior it guarded, defaulting a fresh local Code session to the launch directory, is deliberately removed here, so this is not a promise to preserve that default, only its refusal to guess.
- Do not rewrite the saved root of an existing session.
- Do not introduce a second ZeroCode-side persisted root store.
- Do not broaden filesystem permissions or change sandbox policy.
- Do not redesign Chat session persistence or the daemon's workspace resolution rules.
- Do not require a directory picker for every fresh session; the selected agent workspace remains the default.

## User-visible contract

### Fresh sessions

A fresh session with no explicit directory sends an omitted `cwd` through the existing RPC client. The daemon resolves the selected agent's configured workspace. This applies to Chat and to local Code unless an explicit directory is selected. Remote (WSS) Code is never in that position: its fresh and restart paths open the daemon-side picker first, so an explicit directory always exists by the time `session/new` is sent.

An explicit directory is passed as `cwd` and wins over the agent workspace. The session response's `workspace_dir` is displayed and stored in the existing `ChatState`. Whether an explicit path is absolute is judged for the machine that will run the session: a remote selection is accepted in any wire form a daemon can report (POSIX, drive-letter, or UNC), while a local selection must be absolute on this machine.

### Resumed Code sessions

A resumed Code session sends no replacement `cwd`. The daemon rehydrates the session by its existing session ID and restores its saved directory. The current process directory, the current agent workspace, and any new-session picker state must not override that saved root.

### Changing directory

`/change-directory` is available in the Code pane. It opens a local directory picker rooted at the current Code session's directory when available, or the process directory as a fallback for a fresh local picker, falling back in turn to this machine's filesystem root when the process directory is not usable as a root. For remote Code it uses the existing daemon-backed picker, which starts at the daemon's protocol root because no remote root-discovery RPC exists.

Confirming a directory starts a new session with the same agent and the selected `cwd`. The existing session remains tracked at its original root and can be resumed later. Cancelling the picker returns to the existing session. If the selected path cannot be represented or session creation fails, the existing session remains active and receives a localized error notice; no silent fallback to the agent workspace is allowed.

The existing new-session/restart controls continue to create a replacement session under the default-root contract. They do not mutate the old session's root in place.

## Architecture

The implementation remains inside the ZeroCode chat/session boundary:

- `apps/zerocode/src/chat.rs` owns session phases, active/background session tracking, resume entries, `cwd` precedence, picker transitions, and replacement-session error handling.
- `apps/zerocode/src/input_bar.rs` owns the local slash-command registry and parses `/change-directory` into a pane action, without owning session state.
- `apps/zerocode/src/file_explorer.rs` remains the directory-selection UI and keeps local filesystem versus remote RPC listing behavior behind its existing constructors.
- `apps/zerocode/src/client.rs` remains the JSON-RPC transport boundary; no new persistence or wire field is needed.
- `apps/zerocode/locales/*/zerocode.ftl` owns localized command/help/error text.
- `docs/book/src/zerocode/running.md` documents the final root-selection and resume contract.

The daemon's session response (`workspace_dir`) is the canonical root after creation or resume. ZeroCode may retain it in the existing `ChatState` for display and for choosing a picker start directory, but must not create a parallel durable preference.

## State transitions and error handling

1. **Fresh default:** select agent → call `session/new` with `cwd: null` → adopt returned session and `workspace_dir`.
2. **Fresh explicit:** select agent/directory → call `session/new` with the selected path → adopt returned session and `workspace_dir`.
3. **Resume:** select saved session → call `session/new` with the saved session ID and `cwd: null` → replay durable history and retain returned `workspace_dir`.
4. **Change directory:** active Code → stash active state → picker → call fresh `session/new` with explicit `cwd` → keep old session tracked and focus the new one; on cancel/error restore the old focus.
5. **Invalid explicit path:** report a localized error and do not start or replace a session. A non-UTF-8 path is an error on platforms that can produce one; an empty command argument is not treated as a path.
6. **Session creation error:** retain the old active session and its root. If a new session was created but replacement cleanup fails, use the existing cleanup/error-notice path rather than changing ownership silently.

## Testing strategy

Use focused co-located tests at the JSON-RPC request and session-transition boundary:

- Fresh local Code and Chat sessions omit `cwd` and allow the agent workspace to win.
- Fresh and restarted remote (WSS) Code always goes through the mandatory daemon-side picker and sends the chosen path as `cwd`; there is no remote fresh-session path that omits `cwd`.
- Fresh local Code with an explicit path sends that path.
- Resuming a Code session sends no replacement `cwd` and adopts the daemon's saved `workspace_dir`.
- `/change-directory` parses, opens the correct picker, starts a distinct session with the selected path, and leaves the previous session tracked.
- Cancelling `/change-directory` restores the previous session without an RPC `session/new`.
- Invalid/non-UTF-8 explicit paths surface an error without an omitted-`cwd` fallback.
- Creation failure during a directory change preserves the old session and its root.
- Existing restart, reconnect, WSS picker, Chat workspace, and filesystem restriction coverage remains green.

The final validation should include the focused `zerocode` tests, formatter, strict Clippy, the full `zerocode` package tests, and the docs quality/link gates. Because this changes a TUI interaction, implementation reporting should also identify the supported-interface smoke path or its remaining environment-specific gap.

## Documentation and rollback

`docs/book/src/zerocode/running.md` will state that fresh sessions default to the selected agent workspace, explicit Code directory selection starts a new session, resumed Code sessions keep their own saved directory, and Chat reattachment may resolve against a changed agent workspace.

Rollback is a code revert. It does not migrate or rewrite stored sessions: existing sessions retain their saved roots, while fresh-session behavior returns to the prior interim default.
