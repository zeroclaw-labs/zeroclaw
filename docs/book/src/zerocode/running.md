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

Local **Code** sessions start in the directory you launched zerocode from, so
file and shell tools operate on that project. If that directory cannot be
determined, the session reports an error instead of starting somewhere else.
Local **Chat** sessions still use the selected agent's configured workspace.
Remote (WSS) Code sessions keep the directory picker. Existing sessions keep
their own working directory when you resume them.

## Switching sessions

In the **Chat** and **Code** panes you can load or switch existing sessions without restarting zerocode:

- **Switch session** opens the session list (default chord: Ctrl+S; rebindable in the keymap).
- Use the list-navigation keys to move the selection (defaults: Up/Down).
- **Enter** switches to the highlighted session.
- **New session** starts fresh (default chord: Ctrl+N; rebindable).

The in-app help overlay shows your live key bindings for these actions.

Chat/Code sessions and ACP-backed sessions use different stores. If you use the ACP protocol directly, use `session/load` when you need transcript replay and `session/resume` when you only need the server-side session state restored. See the [ACP documentation](../channels/acp.md) for protocol-level details.

## Terminal text input

zerocode runs as a terminal UI in raw mode. It receives terminal key and paste
events, not native platform text-field events. On macOS, system text
replacements therefore work only when your terminal expands them before
zerocode receives the input.

### Composer editing

The Chat and Code composers support these defaults. “Primary” means Command on macOS and Control elsewhere; literal Control aliases also work when your terminal delivers them.

| Action | Default shortcut |
| --- | --- |
| Undo / redo | Primary+Z / Primary+Shift+Z (also Control+Z / Control+Shift+Z) |
| Select all | Primary+A (also Control+A) |
| Copy / cut selection | Primary+C / Primary+X (also Control+C / Control+X) |
| Extend selection | Shift+arrows, Shift+Home/End |
| Extend selection by word | Alt+Shift+Left/Right |
| Delete next word | Alt+Delete |
| Clear text | Primary+U (also Control+U) |

Undo history belongs to the current draft and retains at most 100 edit groups. Consecutive typing, including spaces, undoes in one step. A pause of two seconds, cursor or selection movement, a mouse click or drag, or another editing command ends the typing group. Pasted text, completion, cut, clear, and newline each form a separate edit. Typing over a selection starts a new group; undo restores the selected text and selection. Cursor movement does not create an edit. A new text edit after undo discards redo. Undo and redo restore text, cursor, and selection, but never add or remove attachments. Sending, switching sessions, or loading a queued message for editing starts fresh history.

Selection shortcuts act on the focused composer, not the queue sidebar. Copying selected input does not cancel a running turn or quit; with no input selection, Control+C retains its cancel/quit behavior. Dialogs and transcript browsing keep their own shortcuts. Use `/attach` to browse files; the configurable **browse files** action has no default shortcut because Primary+A now selects text. The Help overlay shows the current configured bindings.

Terminal or operating-system shortcuts may intercept Command, Control, or clipboard events before zerocode sees them. Clipboard copy uses the terminal's OSC 52 support; bracketed paste remains available.

## CLI flags

| Flag | Description |
|------|-------------|
| `--connect <url>` | Connect to a remote daemon via WSS (e.g. `wss://host:9781`) |
| `--tls-skip-verify` | Skip TLS certificate verification. Required for self-signed certs |
| `--config-dir <path>` | Override the config directory |

## Terminal status

zerocode automatically publishes the most urgent Chat or Code turn state to
the terminal using two escape-sequence conventions:

- OSC 2 sets a short, human-readable tab title such as `⏳ my-agent — working`
  or `⚠ my-agent — awaiting approval`.
- OSC 9;4 reports cleared, indeterminate, or warning progress without requiring
  another program to parse the title text.

Both sequences derive idle, working, blocked, and finished semantics from the
content-free lifecycle contract in `zeroclaw-api`; Zerocode keeps localized
detail such as thinking, responding, or the current tool only for display.
Every live session in the sidebar is a candidate, focused or not: blocked
outranks working, working outranks idle, and a named session wins ties.

This is terminal metadata, not a connection to a particular workspace manager.
Compatible terminals and multiplexers may display, retain, or consume it;
software that does not support these sequences ignores them. The payload
includes only the selected agent alias and a bounded status or tool name. It
never includes the prompt, tool arguments, tool output, or response text.

If the daemon connection is lost, zerocode immediately clears progress and
publishes the neutral `✓ zerocode` title instead of retaining a cached working
or blocked state. Session state remains available for reconnection, and live
session status is projected again only after the daemon reconnects.

On normal exit and supported termination signals, zerocode clears progress and
restores the terminal title when the terminal supports a title stack. It writes
a neutral `zerocode` fallback for terminals without one. Like all terminal
programs, it cannot clean up after an uncatchable `SIGKILL` or an abrupt machine
shutdown. A later terminal or shell title update, or closing the tab, clears
that stale display.
