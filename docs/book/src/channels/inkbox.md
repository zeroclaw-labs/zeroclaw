# Inkbox

[Inkbox](https://inkbox.ai) gives an agent its own communication identity (a real
email mailbox, a phone number for SMS and voice, and iMessage) behind a single
hosted account. One `[channels.inkbox.<alias>]` instance binds the agent to one
Inkbox identity and answers across every surface that identity exposes.

| Surface | What the agent can do |
|---|---|
| Email | Receive and send email from the identity's mailbox |
| SMS | Two-way texting from the identity's phone number |
| Voice | Answer and place calls, optionally with OpenAI Realtime audio |
| iMessage | Reachable over Apple Messages through the Inkbox router |

Unlike most channels, Inkbox needs no public host of your own: inbound webhooks
and call media are delivered to the gateway through a built-in Inkbox tunnel, so
a laptop or a box behind NAT works without a reverse proxy.

## Who can talk to the agent

Unlike chat-platform channels, Inkbox does not gate senders through a per-channel
[peer group](./peer-groups.md). Reachability is enforced server-side by Inkbox
contact rules on the identity's mailbox and phone number: manage who can reach
the agent from the Inkbox console, and anyone Inkbox admits is delivered to the
gateway.

## Setup

The fastest path is the onboarding wizard. Run quickstart, open the **Channels**
step, and pick **Inkbox**:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw quickstart
```

</div>

The wizard either creates a fresh agent identity for you (self-signup with email
verification) or validates an Inkbox API key you already hold, then offers to
provision a phone number, mint a webhook signing key, and enable OpenAI Realtime
calls. It writes the `[channels.inkbox.<alias>]` block and binds it to the agent.

To configure by hand instead, set the three essentials (an API key, the agent
identity handle, and a recommended webhook signing key):

```toml
[channels.inkbox.default]
enabled = true
identity = "on-call-agent"   # the agent handle this gateway runs as
# api_key and signing_key are secrets — set them masked (see below)
```

## How inbound traffic arrives

The channel opens an outbound tunnel to Inkbox and registers webhook
subscriptions for the identity's mail, SMS, iMessage, and calls. Inbound events
are forwarded over that tunnel to the gateway's loopback listener, so no inbound
port needs to be open. Inkbox signs each webhook with an HMAC over the body;
configure `signing_key` so the channel can verify and reject unsigned or forged
traffic.

## Realtime calls (optional)

By default, voice calls use Inkbox speech-to-text and text-to-speech. Set
`realtime_enabled = true` with a `realtime_api_key` to instead bridge raw call
audio to the OpenAI Realtime API, so the model speaks and listens directly.
`realtime_fallback` controls whether a Realtime connection failure falls back to
Inkbox STT/TTS for that call.

## Configuration surfaces

{{#config-fields channels.inkbox}}

{{#config-where channels inkbox}}

{{#secret-config channels.inkbox.<alias>.api_key}}

The same applies to `signing_key` and `realtime_api_key`.

## Start and check

After configuring an instance, start the channel runner:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw channel start
```

</div>

Use `zeroclaw channel doctor` for a first check; it confirms the tunnel connects
and the identity resolves. This channel is compiled only when the binary is built
with the `channel-inkbox` feature.
