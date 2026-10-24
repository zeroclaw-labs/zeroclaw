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

## Refresh a session

Press **F5** in Code or Chat to refresh the focused session after another authorized client completes a turn in it. This reads the daemon's current session state and durable transcript. It does not cancel a running turn or poll in the background. If the session is busy, wait for the turn to finish and press F5 again.

Refresh keeps your composer draft, attachments, queued messages, and pending approvals or questions. It retains selection and reading position by matching unchanged transcript entries. If selected content or a reading anchor no longer exists in the stored history, refresh keeps the current view and shows a retry notice instead of discarding it. Read failures also retain the last valid transcript. Refresh is a point-in-time read, not live mirroring of another client's output.
