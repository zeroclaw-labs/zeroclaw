# Signal

ZeroClaw's Signal channel talks to a running `signal-cli` HTTP daemon. Signal does not provide an official bot API, so ZeroClaw connects to `signal-cli` over local HTTP and lets `signal-cli` own the Signal account, device keys, and message transport.

Use this channel when you already operate a Signal account with `signal-cli`, or when you can run the daemon next to ZeroClaw. If you only have the Signal desktop or mobile app installed, that is not enough by itself; ZeroClaw needs the HTTP daemon endpoint.

## Who can talk to the agent

{{#peer-group signal}}

You can also narrow traffic at the channel level: `dm_only = true` ignores
groups; `group_ids = ["<signal-group-id>"]` accepts only listed groups while
still accepting DMs; `ignore_attachments` and `ignore_stories` drop those
message types before they reach the agent.

## Prerequisites

- A Signal account linked or registered in `signal-cli`.
- A running `signal-cli` HTTP daemon, for example `signal-cli daemon --http 127.0.0.1:8686`.
- A ZeroClaw build with the `channel-signal` feature enabled.

Keep the daemon bound to localhost unless you have put it behind your own authenticated network boundary. The daemon can send and receive as the linked Signal account.

## Configure the channel

{{#config-fields channels.signal}}

{{#config-where channels signal}}

Bind the channel to an agent via that agent's `channels` list.

## Media attachments

Inbound attachments are fetched from `signal-cli` and saved under the owning
agent's workspace in `signal_files/`; if no agent owns the channel, they are
saved under the ZeroClaw data directory instead. The agent sees them as
`[IMAGE:...]`, `[AUDIO:...]`, `[VIDEO:...]`, or `[DOCUMENT:...]` markers.

To send a file, the agent writes a marker such as `[IMAGE:report.png]` or
`[DOCUMENT:/path/inside/workspace/notes.pdf]`. Only regular files inside the
workspace are sent; URLs and paths outside the workspace are refused. A file
received over Signal is saved inside the workspace, so the agent can send it
back by reusing the path from its marker.

Each file is limited to Signal's 100 MiB attachment size, which Signal measures
after encryption, so a file just under the limit can still be rejected. All
attachments in one message are also limited to 100 MiB combined: inbound
attachments past that are skipped without downloading, and the message text
still arrives. A marker that cannot be sent, including one past the combined
limit, is removed from the reply, logged, and counted in a short note appended
to the message.

Signal sends a forwarded file and any comment added to it as two separate
messages. To have them answered as one, set `[channels] debounce_ms`; large
files may need a longer window because the file arrives only after it has
downloaded.

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
