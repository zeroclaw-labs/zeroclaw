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

## Transcript times

The transcript uses dividers before prompts after a day change or at least
15 minutes of inactivity. A finished turn that lasts at least one minute has
a duration footer. Times use the viewer's local timezone, a 24-hour clock
(`HH:MM`), and ISO dates (`YYYY-MM-DD`) in every locale. Labels such as
"Today" and "Yesterday" and duration units use the selected language.

Live entries use the client's receive time. After an ACP history reload,
the original prompt uses the daemon's recorded turn start time, and other
rows use the finalization time. A later user row in the same checkpointed
turn also uses the finalization time because its arrival time is not recorded.
Clock skew between a remote client and daemon can therefore move dividers
or footers after a reload. Recovered turns have no duration footer because
the recovery time does not describe when the turn stopped.

Chat history omits times until the backend preserves individual message
timestamps across transcript rewrites. History from older daemons without
timestamps also has no time marks.
