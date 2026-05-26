# Signal

ZeroClaw's Signal channel talks to a running `signal-cli` HTTP daemon. Signal does not provide an official bot API, so ZeroClaw connects to `signal-cli` over local HTTP and lets `signal-cli` own the Signal account, device keys, and message transport.

Use this channel when you already operate a Signal account with `signal-cli`, or when you can run the daemon next to ZeroClaw. If you only have the Signal desktop or mobile app installed, that is not enough by itself; ZeroClaw needs the HTTP daemon endpoint.

## Prerequisites

- A Signal account linked or registered in `signal-cli`.
- A running `signal-cli` HTTP daemon, for example `signal-cli daemon --http 127.0.0.1:8686`.
- A ZeroClaw build with the `channel-signal` feature enabled.

Keep the daemon bound to localhost unless you have put it behind your own authenticated network boundary. The daemon can send and receive as the linked Signal account.

## Configure the channel

The easiest path is the channels onboarding flow:

```bash
zeroclaw onboard channels
```

For manual config, create or update a Signal channel block:

```toml
[channels.signal.default]
enabled = true
http_url = "http://127.0.0.1:8686"
account = "<your-signal-e164-number>"

[agents.assistant]
enabled = true
channels = ["signal.default"]
```

`http_url` is the base URL of the `signal-cli` daemon. `account` is the account identifier `signal-cli` uses for the linked Signal account, usually the E.164 phone number you registered with Signal.

The `channels` entry binds the channel alias to the agent that should answer it. Use your real agent alias instead of `assistant`.

## Restrict who can talk to the agent

Inbound peer authorization lives in `peer_groups`. A group can target every Signal alias with `channel = "signal"` or one alias with `channel = "signal.default"`.

```toml
[peer_groups.signal_ops]
channel = "signal.default"
agents = []
external_peers = ["<allowed-signal-sender>"]
ignore = []
```

Signal sender identifiers may be E.164 phone numbers or UUID/source identifiers depending on what `signal-cli` reports for the event. Use the identifier shape from your daemon logs or event payloads.

You can also narrow traffic at the channel level:

```toml
[channels.signal.default]
dm_only = true
ignore_attachments = true
ignore_stories = true
```

`dm_only = true` ignores groups. `group_ids = ["<signal-group-id>"]` accepts only listed groups while still accepting DMs. `ignore_attachments` and `ignore_stories` reduce message types that are forwarded to the agent.

## Start and check

Start the daemon first, then start ZeroClaw channels:

```bash
signal-cli daemon --http 127.0.0.1:8686
zeroclaw channel start
```

Use `zeroclaw channel doctor` to confirm ZeroClaw can load the configured channel. If the channel fails at runtime, check that `http_url` points at the daemon, the account is registered in `signal-cli`, and the build includes `channel-signal`.

## Common confusion

The `signal-cli` project is primarily known as a CLI, but ZeroClaw needs its HTTP daemon mode. If you installed only the command-line binary and never started the daemon, ZeroClaw has nothing to connect to.
