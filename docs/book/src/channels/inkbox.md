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

Run quickstart, open the **Channels** step, and pick **Inkbox**:

<div class="os-tabs-src">

#### sh

```sh
zeroclaw quickstart
```

</div>

Quickstart shows Inkbox's schema-derived fields and writes what you enter into a
`[channels.inkbox.<alias>]` block bound to the agent, the same way every other
channel is onboarded. It does not create an identity, provision a number, verify
email, or mint a key for you: get an **agent-scoped API key** and its **identity
handle** from the [Inkbox console](https://inkbox.ai) first, then paste them here.

The fields it collects (everything after `identity` is optional, with sensible
defaults pre-filled):

| Field | Required | Notes |
|---|---|---|
| `api_key` | yes (secret) | agent-scoped Inkbox API key for this identity |
| `identity` | yes | the agent handle this gateway runs as |
| `signing_key` | no (secret) | webhook signing key (`whsec_...`) for inbound verification |
| `base_url` | no | defaults to `https://inkbox.ai` |
| `realtime_enabled`, `realtime_api_key` | no | turn on OpenAI Realtime calls and the OpenAI key (see below) |

Creating the identity, provisioning a phone number, minting a signing key, and
enabling iMessage happen in the Inkbox console, not the CLI.

To configure by hand instead, set the essentials directly:

```toml
[channels.inkbox.default]
enabled = true
identity = "on-call-agent"   # the agent handle this gateway runs as
# api_key and signing_key are secrets; set them masked (see below)
```

## How inbound traffic arrives

The channel opens an outbound tunnel to Inkbox and registers webhook
subscriptions for the identity's mail, SMS, iMessage, and calls. Inbound events
are forwarded over that tunnel to the gateway's loopback listener, so no inbound
port needs to be open. Inkbox signs each webhook with an HMAC over the body;
configure `signing_key` so the channel can verify and reject unsigned or forged
traffic. Call-media WebSocket upgrades are signed the same way (over the signed
call context) and are verified before the socket is accepted: a forged, stale,
or replayed upgrade is rejected before any audio bridge or external model
connection exists.

## Delivery failures

An outbound message can be rejected at send time (an outbound content-policy
block, an opted-out recipient, a bad address) or fail after acceptance (the
carrier flags it, or the receiving mail server bounces it, reported via the
delivery-lifecycle webhooks). Both cases wake the agent in the same
conversation with the exact error and its own undelivered body, so it can fix
and resend. Guardrails: a hard cap of three total sends per logical reply, a
budget that resets on a fresh inbound message, a delivered receipt, or after
30 minutes, and webhook replays deduped per failed message. When there is
nothing sensible to resend, the agent replies `[SILENT]` and nothing is
delivered to the recipient.

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
