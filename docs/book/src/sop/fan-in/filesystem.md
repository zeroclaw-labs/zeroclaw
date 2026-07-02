# SOP Fan-In: Filesystem

Filesystem changes can start SOP runs. The watcher monitors one or more paths with a recursive `notify` watcher, debounces and settles each change, builds a SOP event per change, and dispatches it to the engine. This path is gated by the `channel-filesystem` build feature (default on).

> The transport side (watched paths, include and exclude globs, broad-root and symlink safety) is configured on the [Filesystem channel](../../channels/filesystem.md). This page covers the trigger.

## Trigger

{{#sop-trigger filesystem}}

## Matching

The `path` supports glob patterns (`*`, `**`, `?`); a bare directory matches any change at or under it. The optional `events` list narrows by change kind; an empty list matches all kinds. Each change is lifted into a structured payload that an optional trigger `condition` is evaluated against and that the matched run sees in step context.

## Fire it

With a SOP loaded and the filesystem channel watching a path, write to that path to produce a change the watcher delivers: create, modify, rename, or delete a file inside a watched root. The watcher debounces and settles the change, then dispatches it. A run starts for every loaded SOP whose `path` glob and `events` filter match, and whose `condition` (if any) holds against the change payload.

If nothing starts, confirm the path is inside a watched root (not excluded by a glob or rejected by symlink or broad-root safety), the change kind is in `events`, and the `condition` matches. See the [fan-in overview troubleshooting table](./overview.md#troubleshooting).

## Approve and observe

Runs that hit a checkpoint pause as `WaitingApproval`. Clear or inspect them with the CLI (`zeroclaw sop list`, `zeroclaw sop approve`) or out-of-band over the [gateway API](../../gateway/api.md) approval endpoints (`GET /admin/sop/pending`, `POST /admin/sop/approve`, `POST /admin/sop/deny`).

## See also

- [Filesystem channel](../../channels/filesystem.md): watched paths, globs, symlink safety
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md): the SOP file format
