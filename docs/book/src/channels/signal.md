# Signal

ZeroClaw's Signal channel talks to a running `signal-cli` HTTP daemon. Signal does not provide an official bot API, so ZeroClaw connects to `signal-cli` over local HTTP and lets `signal-cli` own the Signal account, device keys, and message transport.

Use this channel when you already operate a Signal account with `signal-cli`, or when you can run the daemon next to ZeroClaw. If you only have the Signal desktop or mobile app installed, that is not enough by itself; ZeroClaw needs the HTTP daemon endpoint.

## Who can talk to the agent

{{#peer-group signal}}

You can also narrow traffic at the channel level: `dm_only = true` ignores
groups; `group_ids = ["<signal-group-id>"]` accepts only listed groups while
still accepting DMs; `ignore_attachments` and `ignore_stories` drop those
message types before they reach the agent.

Messages you send yourself to your own number ("Note to Self" in the Signal
app) arrive from `signal-cli` as a sync event rather than a normal message,
and ZeroClaw only accepts the ones addressed back to the configured
`account`; sync traffic sent to other contacts, and group sync traffic, is
ignored. Because the sender of a Note-to-Self message is the account
itself, that number must also be listed in the `external_peers` of a
`[peer_groups.<name>]` block with `channel = "signal"` for these messages
to reach the agent. When signal-cli reports a ZeroClaw self-send on the
sync stream, ZeroClaw correlates it with the exact timestamp returned by
the send RPC and does not re-ingest it. An event that arrives before the
RPC response waits for that timestamp; an equal body with a different
timestamp remains a genuine note.

If the daemon accepts a self-send but the HTTP response is lost or cannot
be parsed, ZeroClaw cannot recover the canonical timestamp safely. It then
fails Note-to-Self closed until the ZeroClaw daemon restarts rather than risk
an agent replying to its own output. Restarting only the Signal channel does
not clear this process-wide safety state. Ordinary Signal messages are
unaffected.

Echo correlation is tracked per Signal endpoint and account, so every
outbound surface bound to the same account -- the supervised listener, the
tool-facing channel handle used by the gateway, and the SOP adapter --
shares one record. A self-send performed by any of them is recognized and
suppressed by the listener, instead of returning as a new turn.

A confirmed self-send is cleared when its own echo arrives on the sync
stream. ZeroClaw tracks at most 128 unresolved self-sends and never evicts
them, because dropping one would let a late echo be replayed as a genuine
note. If 128 self-sends accumulate whose echoes never arrive -- a sustained
sync-stream outage while sending continues -- further Note-to-Self sends are
refused before the RPC with an error naming the 128-message safety limit,
until the ZeroClaw daemon restarts. Restarting only the Signal channel or
reloading its config intentionally keeps the process-wide correlation record,
because a late echo may still arrive on a rebuilt listener. Restart the full
ZeroClaw daemon to clear the record and restore Note-to-Self sending. Inbound
Signal traffic and all other outbound targets keep working throughout; only
self-sends are refused.

## Prerequisites

- A Signal account linked or registered in `signal-cli`.
- A running `signal-cli` HTTP daemon, for example `signal-cli daemon --http 127.0.0.1:8686`.
- A ZeroClaw build with the `channel-signal` feature enabled.

Keep the daemon bound to localhost unless you have put it behind your own authenticated network boundary. The daemon can send and receive as the linked Signal account.

## Configure the channel

{{#config-fields channels.signal}}

{{#config-where channels signal}}

Bind the channel to an agent via that agent's `channels` list.

## Start and check

Start the daemon first, then start ZeroClaw channels:

<div class="os-tabs-src">

#### sh

```sh
signal-cli daemon --http 127.0.0.1:8686
zeroclaw channel start
```

</div>

Use `zeroclaw channel doctor` to confirm ZeroClaw can load the configured channel. If the channel fails at runtime, check that `http_url` points at the daemon, the account is registered in `signal-cli`, and the build includes `channel-signal`.

## Common confusion

The `signal-cli` project is primarily known as a CLI, but ZeroClaw needs its HTTP daemon mode. If you installed only the command-line binary and never started the daemon, ZeroClaw has nothing to connect to.
