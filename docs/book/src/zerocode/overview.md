# zerocode

zerocode is ZeroClaw's terminal interface for managing configuration,
chatting with agents, and monitoring your daemon. It connects over a local
IPC stream, a Unix domain socket on Unix or a named pipe on Windows, or
over WebSocket Secure (WSS) for remote use.

It is the primary way to operate a running ZeroClaw: the [Config](./config.md)
pane is the preferred path for changing settings, the Code and Chat panes drive
agents, and the connection works the same whether the daemon is local or on a
remote host.

- [Running zerocode](./running.md): local setup and CLI flags.
- [Config pane](./config.md): the preferred way to change settings.
- [Themes & terminal colours](./themes.md): named palettes and per-agent themes.
- [Remote setup (WSS)](./remote.md): connect to a daemon on another machine.
- [Environment pass-through](./environment.md): how env vars reach agent shells.
