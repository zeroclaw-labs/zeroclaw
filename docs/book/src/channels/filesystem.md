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

The broad system roots `/`, `/home`, `/etc`, `/var`, `/proc`, `/sys`, `/dev`, and `/tmp` are rejected at config validation unless `allow_broad_roots` is set. On Linux and other Unix systems except macOS the same check also rejects:

- each home directory in `/home` (`/home/<name>`) and the superuser's home, `/root`
- `/mnt`, `/media`, and `/run/media`, which hold mounted filesystems such as removable media and, under WSL, the Windows drives; each user's folder of mounts in `/run/media` (`/run/media/<user>`); and `/run`, which holds `/run/media`
- `/var/home`, each home directory in it, `/var/roothome`, and `/var/mnt`, where ostree-based systems such as Fedora Silverblue keep what `/home`, `/root`, and `/mnt` link to

On these systems, repeated and trailing `/` are ignored, so `//etc` and `/home//<name>` are rejected as well, and names match case-sensitively, as their file systems compare them. Mounted filesystems below those folders, such as `/mnt/<name>`, `/media/<label>`, or `/run/media/<user>/<label>`, are accepted: a path cannot tell a whole disk mounted there from a folder mounted for the listener, so check what one holds before you watch it. Under WSL, `/mnt/c` is the whole `C:` drive, including `/mnt/c/Users/<name>`, which Windows itself rejects. On Debian and Ubuntu, `/media/<user>` holds every volume that user mounts; it is accepted too, because elsewhere `/media/<name>` is often a single mounted filesystem.

On macOS the same check also rejects:

- `/Users` and each folder in it: every home directory (`/Users/<name>`) and `/Users/Shared`
- `/Volumes` and each mounted volume in it (`/Volumes/<name>`), including the startup disk's entry, which links to `/`
- `/private`, and `/private/etc`, `/private/tmp`, and `/private/var`, the directories that `/etc`, `/tmp`, and `/var` link to

On macOS, paths match case-insensitively, as a default macOS volume resolves them, and repeated or trailing `/` are ignored, so `/TMP` and `//Users` are rejected as well.

On Windows the same check also rejects:

- every drive root (`C:\`, `C:`), volume root, and network share root (`\\server\share`)
- beneath a drive root: `Windows`, `Windows\Temp`, `Users`, each user profile in `Users` (`C:\Users\<name>`), `Program Files`, `Program Files (x86)`, and `ProgramData`

Windows paths match case-insensitively with either separator and with or without trailing separators, and the `\\?\` and `\\.\` device spellings (`\\?\C:\`, `\\?\UNC\server\share`) match the drive or share they name. On every platform the check reads the path as written: it does not resolve `..` segments, links, or substituted drives.

The check never reads `$HOME`, so its result does not depend on the account the daemon runs as. Each platform instead rejects home directories by their location: `/home/<name>` and `/root` on Linux and other Unix systems, `/Users/<name>` on macOS, and `C:\Users\<name>` on Windows. A home directory holds private files such as SSH keys and shell history, and the daemon account's home also holds the `.zeroclaw` directory where the daemon writes its own state, so point `paths` at the folder your SOPs need, such as `/home/<name>/Inbox`, rather than a whole home directory. A home directory elsewhere, such as a service account's home under `/var/lib`, is not recognized.

Every path in `paths` must be absolute. A relative path such as `inbox`, `.`, or `~/Inbox` (`paths` does not expand `~`) is rejected unless `allow_broad_roots` is set, because the watcher resolves it against the daemon's working directory, so the check cannot tell what it names. Write the full path of the folder you mean instead, such as `/home/<name>/Inbox`. The working directory depends on how the daemon was started:

- `zeroclaw daemon` started by hand: the shell's current directory
- the systemd user service that `zeroclaw service install` writes on Linux: your home directory, so `.` would watch all of it
- the OpenRC service it writes, or a systemd system service without `WorkingDirectory=`: `/`, so `.` names the whole file system and `root` names `/root`
- the launchd agent it writes on macOS: `/` as well, except that a Homebrew install runs in Homebrew's `var/zeroclaw` directory, which holds the daemon's own configuration and logs
- the scheduled task it writes on Windows: `%windir%\System32`
- the container images: `/zeroclaw-data`, which holds the daemon's configuration and data

On Windows an absolute path starts with a drive root such as `C:\` or a share such as `\\server\share`. A path such as `\inbox` or `/inbox` starts at the current drive's root, and `C:inbox` at the current folder of drive `C:`, so these are rejected as relative too.

Symlink event paths are rejected before any metadata, hash, or content read by default; `follow_symlinks` opts in but still requires the canonical target to resolve inside a watched root.

## Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Listener does not start | a broad root or a relative path was rejected at validation | Narrow `paths` away from the broad roots and write each one as an absolute path, or set `allow_broad_roots` |
| Change ignored | excluded by glob, or outside `events` kinds | Check `include`, `exclude`, and `events` against the changed file |
| SOP not starting | trigger `path` glob does not match | Verify the [trigger](../sop/fan-in/filesystem.md) `path` matches and the file is in watch scope |

## See also

- [SOP Fan-In: Filesystem](../sop/fan-in/filesystem.md): trigger syntax and path matching
- [Channels overview](./overview.md)
