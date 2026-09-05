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

- **Switch session** opens the session list (default chord: Ctrl+S; rebindable in the keymap).
- Use the list-navigation keys to move the selection (defaults: Up/Down).
- **Enter** switches to the highlighted session.
- **New session** starts fresh (default chord: Ctrl+N; rebindable).

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
| `/display <value>` | Choose how much of the reasoning comes back: `omitted`, `summarized` or `updates`. |
| `/display reset` | Drop the session's display choice. |
| `/effort:<level> <prompt>` | Use a depth for one message only. `/think:<level>` still works. |

The options come from the daemon and follow the session's model, so the pickers list only what the model accepts:

- Claude 4.7 and later (Opus 4.7, 4.8 and 5, Sonnet 5, Fable and Mythos) offer `low`, `medium`, `high`, `xhigh` and `max`, and the displays `omitted` and `summarized`. Fable and Mythos also offer `updates`, the progress notes the model writes between tool calls.
- Claude 4.6 offers no `xhigh` and no display choice.
- Older Claude models offer `medium`, `high` and `max` only when the runtime profile sets `native_thinking = true`, because those levels spend a token budget.
- Claude on Bedrock offers depths but no display.
- Other providers offer nothing: both segments stay hidden, and `/effort` reports that nothing is adjustable.

`medium` sends no depth and lets the model choose. `off` and `minimal` are not offered here: on the current models they send the same request as `low`, and the prompt hints that tell them apart on the CLI and on channels are not applied to daemon sessions, because rewriting the prompt on every change would restart the provider's prompt cache and break signed-thinking replay within a tool round.

A choice lives on the daemon session and beats both the runtime profile's `agent.thinking.display` and the Anthropic slot's `thinking_display`. Switching the model or the provider clears it, because the new model may not accept it, and a new session (Ctrl+N) starts without it. zerocode remembers your last choice per agent in `zerocode-config.toml`:

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

## CLI flags

| Flag | Description |
|------|-------------|
| `--connect <url>` | Connect to a remote daemon via WSS (e.g. `wss://host:9781`) |
| `--tls-skip-verify` | Skip TLS certificate verification. Required for self-signed certs |
| `--config-dir <path>` | Override the config directory |
