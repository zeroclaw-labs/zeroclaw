# Plugin Release Acceptance

A first-party plugin is accepted for a release when an operator can install it
by name on a published `zeroclaw` binary, with no Rust toolchain on the
machine, and can show that the plugin, not a built-in feature, does the work.
This page is the runbook for that proof and the template for its evidence. It
uses the Discord channel plugin as the worked example. For another package,
substitute its name, its alias, its required keys, and its destinations.

One run proves one package version on one published build of the host. It
says nothing about other targets, other host versions, or behavior under load.

## When to run it

Run it after the release is published and the package version is in the
registry, once for each package version the release claims to support. Run it
again when either side changes: a new package version, or a host release that
changes the plugin interface.

## Before you start

- **The artifact.** A published `zeroclaw` CLI archive for a 64-bit target
  whose plugin host compiles `.wasm` components. Check it first with
  [Release artifact verification](./release-verification.md). An x86_64 Linux
  archive in a fresh container is the simplest place to show that no
  toolchain was involved.
- **A clean machine.** No Rust toolchain and no configuration left over from an
  earlier run.
- **Platform test accounts.** For Discord, a dedicated bot application in a
  small test server, never a production bot, with the **Message Content
  Intent** on and the bot invited (see the [Discord quickstart](../channels/discord.md#quickstart)),
  plus a second Discord account for the tester. Plan to reset the bot token
  when the run is over.
- **A model provider.** Any configured provider works. A scripted local
  provider makes the agent's replies repeatable.
- **An evidence file.** Copy the [evidence template](#evidence-template) and
  fill it in as you go.

Enter the bot token only at the masked `zeroclaw config set` prompt. Never put
it in a file, on a command line, in shell history, in a screenshot, or in the
evidence.

## 1. Record the environment

```sh
command -v cargo rustc || echo "no Rust toolchain"
sha256sum zeroclaw-<target>.tar.gz
zeroclaw --version
zeroclaw plugin search discord
```

Record the archive name and digest, the version string, and the registry entry
the install resolves: its name, version, archive URL, and `sha256`.

## 2. Install by name and bind an instance

```sh
zeroclaw plugin install discord --channel-alias acceptance --egress declared
zeroclaw config set plugins.entries.zpi1_WyJkaXNjb3JkIiwiY2hhbm5lbCIsImFjY2VwdGFuY2UiXQ.config.bot_token
zeroclaw config set plugins.enabled true
zeroclaw plugin info discord
```

The install prints the binding and the hosts it granted. `plugin info` must
report that the package loads on this host and that the instance key above
exists. Then, by hand:

- add `plugin.acceptance` to one enabled agent's `channels`;
- add a peer group with `channel = "plugin.acceptance"` whose
  `external_peers` holds the tester's numeric Discord user ID.

The [Discord page](../channels/discord.md#running-discord-as-a-plugin-experimental)
shows both. Configure no `[channels.discord.<alias>]` table anywhere, then
start the daemon. Keep a copy of `config.toml` for the evidence; its secrets
stay encrypted.

## 3. Exercise the plugin

Work through the template in order and record what you observe, including
anything unexpected. The log records named below are in the daemon's
structured log, which the dashboard's Logs page and `GET /api/logs` read (see
[Observability](../ops/observability.md)). Use synthetic message text
throughout.

## Evidence template

| Claim | Action | Evidence | Result |
|---|---|---|---|
| Installed by name with no toolchain | Steps 1 and 2 | `command -v` output, install output, registry entry, archive digest | |
| Loads on this host | `zeroclaw plugin info discord` | Its load verdict | |
| Inbound goes through the plugin | The tester sends a message | A `channel inbound message` record with `zeroclaw.channel = plugin.acceptance` | |
| Outbound goes through the plugin | The agent replies | A `reply delivered` record for `plugin.acceptance`; a redacted screenshot of the reply | |
| The built-in channel handled nothing | Read the startup output and `/health` | The channel list shows `plugin.acceptance` and no `discord.*`; `/health` lists `channel:plugin.acceptance` and no `channel:discord.*`; the config copy has no `[channels.discord]` table | |
| Negative control | Set `channels.plugin.acceptance.enabled` to `false`, restart, send a message | No inbound record and no reply; set it back to `true` | |
| Reconnect | Cut the machine's network for at least two minutes, then restore it | The next message is answered; note whether the plugin resumed its session or started a new one | |
| Shutdown | Stop the daemon | The process exits cleanly; note how long the bot takes to show offline | |
| Denied destination | Remove `*.discord.gg` from the instance's `egress_hosts`, restart | A `plugin_egress_denied` record with `transport = websocket`, the refused host, and a `remedy` command; no inbound | |
| Grant restored | Put `*.discord.gg` back, without a restart | The bot reconnects; note how long it took | |
| Denied sender | Send from an account outside the peer group | A `plugin_channel_sender_denied` record and no reply | |
| Missing platform permission | Remove the bot's **Send Messages** permission in one channel and message it there | A send failure in the log, the daemon still running, no reconnect loop | |
| Invalid token | Set a syntactically valid invalid or revoked token through the masked `config set` prompt, then restart | The plugin reports Discord's authentication refusal and backs off; no reconnect loop in the log over five minutes. A malformed token rejected locally by `configure()` does not exercise platform authentication or backoff | |
| Trust policy holds | Set `plugins.security.signature_mode` to `strict` without trusting the package's publisher key, restart | The daemon skips the package and logs the signature refusal; `plugin.acceptance` does not start. Restore the previous mode | |

## Redact before posting

The `channel inbound message` and `reply delivered` records carry the message
text and the sender's user ID. Before posting anything:

- replace user, server, and channel IDs with placeholders;
- keep only synthetic message text;
- replace every `enc2:` value in the config copy with `<encrypted>`;
- crop screenshots to the messages under test.

The token never appears in the evidence, redacted or not.

## Post the evidence

Post on the tracking issue for the release: the artifact name and digest, the
`zeroclaw --version` output, the registry entry and package version, the exact
commands from steps 1 and 2, the filled-in table, and links to the pull
requests that changed the package or the host since the last accepted run.

## Clean up

1. Stop the daemon and run `zeroclaw plugin remove discord`. Removal keeps the
   instance's `[[plugins.entries]]` row and its `[channels.plugin.acceptance]`
   binding and prints both; delete them.
2. Reset the bot token in the Discord Developer Portal, so the token used in
   the run stops working.
