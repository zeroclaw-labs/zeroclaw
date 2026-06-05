# SMS (Twilio, Plivo, Telnyx, Sinch, Vonage)

ZeroClaw can send and receive SMS through five carrier/aggregator APIs:
Twilio, Plivo, Telnyx, Sinch, and Vonage. Each is a first-class channel that
lets your agent reply to text messages from any phone over the public phone
network (PSTN).

## Overview

All five SMS channels share the same architecture:

- **Outbound** is a direct REST call to the vendor's send API. ZeroClaw never
  polls; it posts the reply when the agent produces one. Bodies longer than the
  vendor's per-call ceiling (1600 characters) are split client-side into
  numbered chunks (see [Limitations](#limitations)).
- **Inbound** is a webhook hosted by the ZeroClaw gateway at
  `/<name>/sms` (for example `/twilio/sms`). The vendor POSTs each incoming
  message to that path. The handler **verifies the vendor's signature before
  doing anything else**, drops senders that are not on the channel's
  `allowed_numbers` allowlist, runs the agent loop, and sends the reply back
  over the same channel.

A few cross-cutting properties:

- **Alias-keyed v3 slots.** Each channel is configured under
  `[channels.<name>.<alias>]`, so you can run more than one account of the
  same vendor side by side (for example `[channels.twilio.support]` and
  `[channels.twilio.alerts]`).
- **Feature-gated.** Each channel compiles only when its Cargo feature
  (`channel-twilio`, `channel-plivo`, `channel-telnyx`, `channel-sinch`,
  `channel-vonage`) is enabled in the build. If a channel's route returns
  `404`, the binary was built without that feature or the channel is not
  configured.
- **Credentials come from config only.** Every secret is a `#[secret]` config
  field set in your ZeroClaw config file. These channels do **not** read
  credentials from environment variables.

## Common concepts

### Enabling a channel

Every channel has an `enabled` flag that defaults to `false`. The runtime only
loads channels whose `enabled = true`, so pasting a partial config block does
not accidentally bring a channel live before the rest of its fields are filled
in.

### The `allowed_numbers` allowlist

`allowed_numbers` gates which inbound senders are accepted. It is applied
**after** signature verification, as a second gate:

- **Empty list (the default) denies everyone.** No inbound message is
  processed until you add at least one entry.
- `"*"` allows every sender. This is a public PSTN endpoint, so use the
  wildcard with care.
- Otherwise an entry must match the sender's number. Matching is
  case-insensitive and strips internal whitespace, so the canonical E.164
  value `"+15555550199"` also matches an entry written as `"+1 555 555 0199"`.
  Use E.164 format (a leading `+` and country code).

### Pointing the vendor at your gateway

For each channel, configure the vendor's inbound-SMS webhook to POST to:

```text
https://<your-gateway>/<name>/sms
```

If you run ZeroClaw behind a configured gateway path prefix, include it in the
URL. Twilio and Plivo sign the **destination URL**, so when ZeroClaw sits
behind a reverse proxy or tunnel, make sure `X-Forwarded-Proto` and
`X-Forwarded-Host` reach the gateway correctly — the gateway reconstructs the
signed URL from those headers (falling back to the `Host` header and `https`).

### Signature-scheme summary

| Channel | Webhook path | Signature header | Algorithm |
|---|---|---|---|
| Twilio | `/twilio/sms` | `X-Twilio-Signature` | HMAC-SHA1 over the request URL + sorted form `key+value` pairs, base64 |
| Plivo | `/plivo/sms` | `X-Plivo-Signature-V3` (nonce in `X-Plivo-Signature-V3-Nonce`) | HMAC-SHA256 over URL + nonce + raw body, base64 |
| Telnyx | `/telnyx/sms` | `telnyx-signature-ed25519` (timestamp in `telnyx-timestamp`) | Ed25519 over `{timestamp}\|{raw body}` |
| Sinch | `/sinch/sms` | `x-sinch-webhook-signature` (format `v1,{nonce},{base64-sig}`) | HMAC-SHA256 over nonce bytes + raw body, base64 |
| Vonage | `/vonage/sms` | `sig` form parameter | HMAC-SHA256 over sorted `&k=v` params + secret, lowercase hex |

## Twilio

Twilio sends via its Programmable Messaging `Messages` resource and receives
through the gateway webhook. Create or open an account at the
[Twilio Console](https://console.twilio.com/). From the console dashboard copy
your **Account SID** (`ACxxxxxxxx…`) and **Auth Token**, and provision a phone
number under Phone Numbers to use as the sender.

```toml
[channels.twilio.default]
enabled = true                        # channel is loaded only when true
account_sid = "..."                   # Twilio Account SID (ACxxxx…); public, also the Basic-auth username
auth_token = "..."                    # Auth Token; Basic-auth password AND the inbound HMAC-SHA1 key
from_number = "+15555550100"          # provisioned Twilio number, E.164
allowed_numbers = ["+15555550199"]    # inbound senders; empty = deny all, "*" = allow all
```

- **Outbound:** `POST https://api.twilio.com/2010-04-01/Accounts/{AccountSid}/Messages.json`
  with form fields `From`, `To`, `Body`. Authenticated with HTTP Basic using
  the Account SID as the username and the Auth Token as the password.
- **Inbound:** Set Twilio's "A MESSAGE COMES IN" webhook to
  `https://<your-gateway>/twilio/sms`. The handler verifies the
  `X-Twilio-Signature` header by computing HMAC-SHA1 (keyed by the Auth Token)
  over the full request URL concatenated with the form parameters sorted by
  key (`key + value` for each pair), base64-encoding it, and comparing
  constant-time against the header.

## Plivo

Plivo sends via its REST `Message` resource and receives through the gateway
webhook. Sign in at the [Plivo Console](https://console.plivo.com/). Copy your
**Auth ID** and **Auth Token** from the dashboard, and provision a number to
send from.

```toml
[channels.plivo.default]
enabled = true                        # channel is loaded only when true
account_id = "..."                    # Plivo Auth ID; public account identifier, Basic-auth username
auth_token = "..."                    # Auth Token; Basic-auth password AND the inbound HMAC key
from_number = "+15555550100"          # sender number, E.164
allowed_numbers = ["+15555550199"]    # inbound senders; empty = deny all, "*" = allow all
```

- **Outbound:** `POST https://api.plivo.com/v1/Account/{auth_id}/Message/`
  (the trailing slash is required) with a JSON body
  `{"src": from, "dst": to, "text": body}`. Authenticated with HTTP Basic
  using the Auth ID as the username and the Auth Token as the password.
- **Inbound:** Set the "Message URL" on your Plivo application to
  `https://<your-gateway>/plivo/sms`. The handler reads the V3 nonce from the
  `X-Plivo-Signature-V3-Nonce` header and verifies the `X-Plivo-Signature-V3`
  header by computing HMAC-SHA256 (keyed by the Auth Token) over the byte
  stream `URL || nonce || raw body` — no separators, no parameter sorting — and
  comparing the base64 digest constant-time.

## Telnyx

Telnyx sends via its V2 `Messages` resource and receives through the gateway
webhook with an Ed25519-signed payload. Sign in to the
[Telnyx Portal](https://portal.telnyx.com/). Telnyx uses **two distinct
values**, copied from different portal pages:

- An **API V2 key**, sent as the bearer credential on outbound calls.
- An **Ed25519 public key** (base64), used to verify inbound webhook
  signatures. If Telnyx rotates the signing key, update `public_key` or inbound
  webhooks will fail verification.

```toml
[channels.telnyx.default]
enabled = true                        # channel is loaded only when true
api_key = "..."                       # Telnyx V2 API key; sent as Authorization: Bearer on outbound
from_number = "+15555550100"          # provisioned Telnyx number, E.164
messaging_profile_id = "..."          # optional; route outbound through a named messaging profile
public_key = "..."                    # base64 Ed25519 public key from the portal; verifies inbound webhooks
allowed_numbers = ["+15555550199"]    # inbound senders; empty = deny all, "*" = allow all
```

- **Outbound:** `POST https://api.telnyx.com/v2/messages` with a JSON body of
  `from`, `to`, `text` (and `messaging_profile_id` when configured),
  authenticated with `Authorization: Bearer {api_key}`.
- **Inbound:** Set the Telnyx "Webhook URL" to
  `https://<your-gateway>/telnyx/sms`. The handler reads the `telnyx-timestamp`
  and `telnyx-signature-ed25519` headers, rejects payloads outside a 5-minute
  (300-second) anti-replay window, and verifies the Ed25519 signature over the
  message bytes `{timestamp}|{raw body}` (literal pipe separator) against the
  configured public key. Only `message.received` events are forwarded to the
  agent.

## Sinch

Sinch sends via its `Batches` REST API and receives through the gateway
webhook. Sign in to the [Sinch Customer Dashboard](https://dashboard.sinch.com/).
Sinch separates outbound and inbound credentials:

- A **service plan ID** (public project identifier) and an **API token** for
  outbound sends.
- A separate **callback secret** that signs inbound webhooks. Do not confuse it
  with the API token.

```toml
[channels.sinch.default]
enabled = true                        # channel is loaded only when true
service_plan_id = "..."               # Sinch service plan ID; public, used in the outbound URL path
api_token = "..."                     # API token; sent as Authorization: Bearer on outbound
region = "us"                         # "us" -> us.sms.api.sinch.com, "eu" -> eu.sms.api.sinch.com (default "us")
from_number = "+15555550100"          # provisioned sender, E.164
allowed_numbers = ["+15555550199"]    # inbound senders; empty = deny all, "*" = allow all
callback_secret = "..."               # HMAC-SHA256 secret for inbound webhooks; distinct from api_token
```

- **Outbound:** `POST https://{region}.sms.api.sinch.com/xms/v1/{service_plan_id}/batches`
  with a JSON body `{"from": from, "to": [to], "body": body}`, authenticated
  with `Authorization: Bearer {api_token}`. `region` selects the host
  (`us` or `eu`).
- **Inbound:** Set the Sinch "Callback URL" to
  `https://<your-gateway>/sinch/sms`. The handler verifies the
  `x-sinch-webhook-signature` header, whose format is `v1,{nonce},{base64-sig}`.
  It rejects anything without the `v1` prefix, then computes HMAC-SHA256
  (keyed by `callback_secret`) over `nonce bytes || raw body` and compares the
  base64 digest constant-time. Only `mo_text` payloads are forwarded.

## Vonage

Vonage (formerly Nexmo) sends via its legacy SMS REST API and receives through
the gateway webhook. Sign in to the
[Vonage API Dashboard](https://dashboard.nexmo.com/). Vonage uses **two
distinct secrets**:

- The **API key** (public) and **API secret**, which Vonage's legacy SMS API
  expects in the request **body**, not in headers.
- A separate **signature secret**, set under the dashboard's API settings as
  the "Signature secret" with algorithm "HMAC SHA-256". This signs inbound
  webhooks. Do not confuse it with the API secret.

```toml
[channels.vonage.default]
enabled = true                        # channel is loaded only when true
api_key = "..."                       # Vonage API key; public identifier, sent in the outbound POST body
api_secret = "..."                    # Vonage API secret; sent in the outbound POST body alongside api_key
from_number_or_sender_id = "+15555550100"  # E.164 number, short code, or alphanumeric sender ID
allowed_numbers = ["+15555550199"]    # inbound senders; empty = deny all, "*" = allow all
signature_secret = "..."              # inbound-webhook HMAC-SHA256 secret; distinct from api_secret
```

- **Outbound:** `POST https://rest.nexmo.com/sms/json` with form fields
  `api_key`, `api_secret`, `from`, `to`, `text`. Credentials travel in the
  form body (Vonage's legacy SMS API does not use auth headers). Vonage returns
  HTTP 200 even when a message is rejected; ZeroClaw inspects the per-message
  status in the JSON response and surfaces a non-`"0"` status as an error.
- **Inbound:** Set the "Inbound SMS Webhook" to
  `https://<your-gateway>/vonage/sms` (POST). The handler pops the `sig`
  parameter, sorts the remaining form parameters alphabetically by key,
  concatenates them as `&{key}={value}`, appends the `signature_secret`,
  computes HMAC-SHA256 (keyed by `signature_secret`), lowercase-hex-encodes it,
  and compares constant-time against `sig`.

## Security notes

- **Every inbound webhook rejects unsigned or invalid-signature requests before
  any processing.** A request that fails signature verification is dropped with
  a `401` and never reaches the agent. Parse failures are treated the same way
  so the endpoint does not leak payload-validity information to unauthenticated
  callers.
- **The allowlist is a second gate.** Even a correctly signed message is
  dropped if the sender is not on `allowed_numbers`.
- **This is a public PSTN surface.** Anyone can text your number. Prefer an
  explicit `allowed_numbers` list over `"*"`, and keep the per-channel secrets
  (Auth Token / API token / callback secret / signature secret / public key)
  out of source control.
- **Behind a proxy, get the forwarded headers right.** Twilio and Plivo sign
  the destination URL; if `X-Forwarded-Proto`/`X-Forwarded-Host` are wrong, the
  gateway reconstructs the wrong URL and every signature check fails.

## Limitations

- **Text only.** These channels send and receive SMS text. There is no MMS or
  media-attachment support.
- **Outbound chunking.** A message body up to 1600 characters is sent in a
  single API call. Longer bodies are split client-side into chunks of at most
  1600 characters, broken at sentence enders, then whitespace, then a hard
  character cut. Each chunk of a split message is prefixed with an `(i/N)`
  marker (for example a leading `(1/3)`) so the recipient can reassemble the
  parts.
