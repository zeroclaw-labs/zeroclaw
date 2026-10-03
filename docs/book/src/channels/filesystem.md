# Filesystem

The `filesystem` channel watches one or more paths and feeds each change into the agent loop or the SOP engine. It is gated by the `channel-filesystem` build feature (default on).

> **This is a SOP event source.** For trigger syntax and path matching, see [SOP Fan-In: Filesystem](../sop/fan-in/filesystem.md). This page covers what is watched and the safety scoping.

## Configuration

The full field list, derived from the live schema. For a basic watcher you set `paths`.

{{#config-fields channels.filesystem}}

Full field reference: [config reference](../reference/config.md#channels).

## Scoping what is watched

`paths` lists the roots to watch; `recursive` controls whether subdirectories are included. `include` and `exclude` globs narrow which paths emit events, and `events` narrows by change kind. `debounce_ms` and `settle_ms` collapse bursts of rapid changes into a single settled event.

## Safety

The broad system roots `/`, `/home`, `/etc`, `/var`, `/proc`, `/sys`, `/dev`, and `/tmp` are rejected at config validation unless `allow_broad_roots` is set. On macOS the same check also rejects:

- `/Users` and each folder in it: every home directory (`/Users/<name>`) and `/Users/Shared`
- `/Volumes` and each mounted volume in it (`/Volumes/<name>`), including the startup disk's entry, which links to `/`
- `/private`, and `/private/etc`, `/private/tmp`, and `/private/var`, the directories that `/etc`, `/tmp`, and `/var` link to

On macOS, paths match case-insensitively, as a default macOS volume resolves them, and repeated or trailing `/` are ignored, so `/TMP` and `//Users` are rejected as well.

On Windows the same check also rejects:

- every drive root (`C:\`, `C:`), volume root, and network share root (`\\server\share`)
- beneath a drive root: `Windows`, `Windows\Temp`, `Users`, each user profile in `Users` (`C:\Users\<name>`), `Program Files`, `Program Files (x86)`, and `ProgramData`

Windows paths match case-insensitively with either separator and with or without trailing separators, and the `\\?\` and `\\.\` device spellings (`\\?\C:\`, `\\?\UNC\server\share`) match the drive or share they name. On every platform the check reads the path as written: it does not resolve `..` segments, links, or substituted drives.

The check never reads `$HOME`, so its result does not depend on the account the daemon runs as. macOS and Windows reject every home directory by its location (`/Users/<name>`, `C:\Users\<name>`). Linux and other Unix systems accept a home directory such as `/home/<name>`, so point `paths` at the folder your SOPs need rather than a whole home directory.

Symlink event paths are rejected before any metadata, hash, or content read by default; `follow_symlinks` opts in but still requires the canonical target to resolve inside a watched root.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Listener does not start | a broad root was rejected at validation | Narrow `paths` away from the broad roots, or set `allow_broad_roots` |
| Change ignored | excluded by glob, or outside `events` kinds | Check `include`, `exclude`, and `events` against the changed file |
| SOP not starting | trigger `path` glob does not match | Verify the [trigger](../sop/fan-in/filesystem.md) `path` matches and the file is in watch scope |

## See also

- [SOP Fan-In: Filesystem](../sop/fan-in/filesystem.md): trigger syntax and path matching
- [Channels overview](./overview.md)
