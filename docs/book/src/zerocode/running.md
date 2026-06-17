# Running zerocode

## Local setup

On the same machine as the daemon, no extra configuration is needed:

<div class="os-tabs-src">

#### sh

```sh
zerocode
```

</div>

zerocode finds the daemon's local endpoint automatically: `<data_dir>/data/daemon.sock`
on Unix, `\\.\pipe\zeroclaw-<hash>` on Windows. If the daemon isn't running,
zerocode spawns an ephemeral one.

## Switching sessions

In the **Chat** and **Code** panes you can load or switch existing sessions without restarting zerocode:

| Action | Shortcut |
|--------|----------|
| Open the session picker | `Ctrl+S` |
| Move selection up/down | `Up` / `Down` |
| Switch to the selected session | `Enter` |
| Start a new session | `Ctrl+N` |

Chat/Code sessions and ACP-backed sessions use different stores. If you use the ACP protocol directly, use `session/load` when you need transcript replay and `session/resume` when you only need the server-side session state restored. See the [ACP documentation](../acp/index.md) for protocol-level details.

## CLI flags

| Flag | Description |
|------|-------------|
| `--connect <url>` | Connect to a remote daemon via WSS (e.g. `wss://host:9781`) |
| `--tls-skip-verify` | Skip TLS certificate verification. Required for self-signed certs |
| `--config-dir <path>` | Override the config directory |
