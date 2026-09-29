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

## Session working directories

Fresh **Chat** sessions, and fresh **Code** sessions on a local connection, use
the selected agent's configured workspace, so file and shell tools operate there
unless you choose a directory yourself. The daemon resolves that root and
reports it back; zerocode does not substitute the directory you launched it
from.

Remote (WSS) **Code** always asks first. A fresh or restarted remote Code
session opens the daemon-side directory picker before the session is created, so
its root is always a directory you selected on the daemon's filesystem. That
picker browses the daemon's machine, not your local one, and it has no default
to fall back to.

In the **Code** pane, `/change-directory` opens a directory picker and starts a
new session in the selected directory. It works on both connections: locally the
picker browses this machine, and over WSS it browses the daemon's filesystem.
The existing session is not moved: it remains available at its own saved root,
and you can switch back to it at any time. Cancelling the picker, or a selection
the daemon rejects, returns you to that session unchanged and reports why.

Resumed Code sessions keep the working directory they were created with, even
if your launch directory or the agent's configured workspace changes afterwards.
**Chat** differs here: a reattached Chat session can resolve against the selected
agent's current workspace.

## Switching sessions

In the **Chat** and **Code** panes you can load or switch existing sessions without restarting zerocode:

- **Switch session** opens the session list (default chord: Ctrl+S; rebindable in the keymap).
- Use the list-navigation keys to move the selection (defaults: Up/Down).
- **Enter** switches to the highlighted session.
- **New session** opens the same add-agent picker as the sidebar `[+]` and adds a session for the agent you choose, leaving the focused session tracked (default chord: Ctrl+N; rebindable).

Clicking a session row's body focuses it; clicking its right-edge `✕` closes that specific session without focusing it first.
Use the Sessions header `[+]` to add a sibling session and `[-]` to close the focused session in the active pane.
Closing a live session safely stops its current work while preserving durable history.

Switching to an existing **Code** session resumes it at its own saved root,
while **New session** starts fresh: at the selected agent's workspace over a
local connection, or in the directory you pick in the daemon-side picker over
WSS. Neither action changes the root of a session that is already running; use
`/change-directory` when you want a Code session somewhere else.

The in-app help overlay shows your live key bindings for these actions.

Chat/Code sessions and ACP-backed sessions use different stores. If you use the ACP protocol directly, use `session/load` when you need transcript replay and `session/resume` when you only need the server-side session state restored. See the [ACP documentation](../channels/acp.md) for protocol-level details.

## Session controls

Next to the model, the chat title shows the session's reasoning **effort** and **display** as `effort:<level>` and `display:<value>` segments, whenever the session's model lets you adjust them. Click a segment, or run the matching command, to change it for this session:

| Command | Effect |
|---|---|
| `/effort` | Open the effort picker. |
| `/effort <level>` | Set the reasoning depth for this session. `/thinking` and `/think` are aliases. |
| `/effort reset` | Drop the session's depth and return to the runtime profile default. |
| `/display` | Open the display picker. |
| `/display <value>` | Choose how much of the reasoning comes back: `omitted` or `summarized`. |
| `/display reset` | Drop the session's display choice. |
| `/effort:<level> <prompt>` | Use a depth for one message only. `/think:<level>` still works. |

The options come from the daemon and follow the session's model, so the pickers list only what the model accepts:

- Claude 4.7 and later (Opus 4.7, 4.8 and 5, Sonnet 5, Fable and Mythos) offer `low`, `medium`, `high`, `xhigh` and `max`. Generations 4.7 through 5.0 also offer `updates`, the progress notes some models write between tool calls, alongside `omitted` and `summarized`; generation 5.1 narrowed the field to those two, so a configured `updates` is sent to them as `summarized` with a warning.
- Claude 4.6 offers no `xhigh` and no display choice.
- Older Claude models offer `medium`, `high` and `max` only when the runtime profile sets `native_thinking = true`, because those levels spend a token budget.
- Claude on Bedrock offers depths but no display.
- Other providers offer nothing: both segments stay hidden, and `/effort` reports that nothing is adjustable.

`medium` sends no depth and lets the model choose. The profile's `native_thinking` switch gates the token budget of the older generations only, so leaving it off does not stop a depth from reaching a model that reads one. `off` and `minimal` are not offered here: on the current models they send the same request as `low`, and the prompt hints that tell them apart on the CLI and on channels are not applied to daemon sessions, because rewriting the prompt on every change would restart the provider's prompt cache and break signed-thinking replay within a tool round.

A choice lives on the daemon session and sits at the top of the display chain: it beats the Anthropic slot's `thinking_display`, which in turn beats the runtime profile's `agent.thinking.display`. Switching the model or the provider clears it, because the new model may not accept it, and a new session (Ctrl+N) starts without it. zerocode remembers your last choice per agent in `zerocode-config.toml`:

```toml
[thinking.agent_override.coder]
level = "high"
display = "summarized"
```

When a new session starts, a remembered value is applied only if the session's model offers it; otherwise the info bar says it was skipped and nothing is applied. Changing the effort mid-session changes the request the model sees, so the provider's prompt cache may not carry over to the next turn.

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
