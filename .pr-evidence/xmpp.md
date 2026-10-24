# XMPP channel verification

The screenshots use the real WebUI renderer with a local API fixture. Accounts,
hosts, passwords and fingerprints are fictitious. They are not evidence of a
production connection or of a model-provider call.

## WebUI

English locale, Chrome, desktop 1440x1000 and mobile 390x844.

- [Connection and masked credential](xmpp-connection-desktop.png)
- [Optional OMEMO configuration](xmpp-omemo-desktop.png)
- [Canonical allowed-user policy](xmpp-allowed-users-desktop.png)
- [Mobile allowed-user policy](xmpp-allowed-users-mobile.png)

The temporary local fixture intercepted API requests and checked protocol
persistence, independent port selection, blank/masked replacement-secret
editing, OMEMO and enabled switches, alias-scoped canonical authorization,
peer-group updates, JavaScript errors and horizontal page overflow. Its
ad hoc driver and raw JSON are intentionally absent from the PR after review;
there is no supported in-PR script or configurable navigation origin.

The Advanced screenshot was refreshed after removing the executable setting;
see [the code revision, focused results and visual scope](xmpp-executable-setting.md).
The other images retain their original fixture scope. Follow the supported
configuration steps in the channel guide for manual reproduction.

## Transport

The opt-in `crates/zeroclaw-channels/tests/xmpp_bridge_integration.py` runner was
exercised against an isolated Prosody 13 server on loopback with disposable
accounts, PEP/pubsub, Direct TLS and mandatory STARTTLS listeners, and a
temporary CA trusted only by the test processes. It exercises the actual
embedded Python transport, not a mock cryptography implementation.

Observed results:

```text
PASS direct TLS / mandatory STARTTLS and normalized direct messages
PASS untrusted TLS certificate rejected
PASS OMEMO authenticated encryption both ways and persistent identity across restart
PASS plaintext ignored when OMEMO required
PASS revoked trust blocks outbound without plaintext fallback
```

Rust tests cover adapter authorization before dispatch, required encryption,
alias attribution, offline/unauthorized sends, configuration round trips,
disabled partial configurations and password secrecy metadata. Python unit
tests cover fingerprint parsing and persistent private SQLite state.

The automated transport matrix is Linux-only. Native Windows execution,
OMEMO v2, MUC, attachments and a real model-provider conversation are outside
this verification. The channel uses the existing shared agent runtime.
