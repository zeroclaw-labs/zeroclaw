# zerocode

zerocode is ZeroClaw's terminal interface for managing configuration,
chatting with agents, and monitoring your daemon. It connects over a local
IPC stream, a Unix domain socket on Unix or a named pipe on Windows, or
over WebSocket Secure (WSS) for remote use.

It is the primary way to operate a running ZeroClaw: the [Config](./config.md)
pane is the preferred path for changing settings, the Code and Chat panes drive
agents, and the connection works the same whether the daemon is local or on a
remote host.

## Local setup

On the same machine as the daemon, no extra configuration is needed:

```bash
zerocode
```

zerocode finds the daemon's local endpoint automatically: `<data_dir>/data/daemon.sock`
on Unix, `\\.\pipe\zeroclaw-<hash>` on Windows. If the daemon isn't running,
zerocode spawns an ephemeral one.

## In this section

- [Config pane](./config.md): the preferred way to change settings
- [Themes & terminal colours](./themes.md): named palettes and per-agent themes
- [Remote setup (WSS)](./remote.md): connect to a daemon on another machine
- [Environment pass-through](./environment.md): how env vars reach agent shells

## CLI flags

| Flag | Description |
|------|-------------|
| `--connect <url>` | Connect to a remote daemon via WSS (e.g. `wss://host:9781`) |
| `--tls-skip-verify` | Skip TLS certificate verification. Required for self-signed certs |
| `--config-dir <path>` | Override the config directory |
