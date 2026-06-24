# zerocode

zerocode is ZeroClaw's terminal interface for managing configuration, chatting with agents, and monitoring your daemon. It connects over a local IPC stream, a Unix domain socket on Unix or a named pipe on Windows, or over WebSocket Secure (WSS) for remote use.

It is the primary way to operate a running ZeroClaw: the Config pane is the preferred path for changing settings, the Code and Chat panes drive agents, and the connection works the same whether the daemon is local or on a remote host.

See the full [zerocode section](../zerocode/overview.md) for running it, the Config pane, themes, remote (WSS) setup, and environment pass-through.
