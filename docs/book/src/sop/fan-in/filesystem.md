# SOP Fan-In: Filesystem

Filesystem changes can start SOP runs. The watcher monitors one or more paths with a recursive `notify` watcher, debounces and settles each change, builds a SOP event per change, and dispatches it to the engine. This path is gated by the `channel-filesystem` build feature (default on).

> The transport side (watched paths, include and exclude globs, broad-root and symlink safety) is configured on the [Filesystem channel](../../channels/filesystem.md). This page covers the trigger.

## Trigger

{{#sop-trigger filesystem}}

## Matching

The `path` supports glob patterns (`*`, `**`, `?`); a bare directory matches any change at or under it. The optional `events` list narrows by change kind (`created`, `modified`, `deleted`, `renamed`); an empty list matches all kinds. The event payload carries the change kind, the path, size-capped file content, and a `sha256` digest, available to an optional trigger `condition` and shown in step context.

## See also

- [Filesystem channel](../../channels/filesystem.md): watched paths, globs, symlink safety
- [Fan-in overview](./overview.md)
- [Syntax](../syntax.md)
