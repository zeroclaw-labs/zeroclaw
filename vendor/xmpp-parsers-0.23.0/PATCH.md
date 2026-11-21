# XMPP date dependency security maintenance

Source: xmpp-parsers 0.23.0, MPL-2.0. Original copyright and LICENSE are retained.
Upstream source commit: 1710a6f99edd18cd926a2dc5d14ec532a2a6ad72.
Original registry archive SHA-256:
92b5d63bbd122ce3b5287d83efdd6916fb88bb274be23f6a9140238e0e1465b0.
The original checksum is also recorded in the preceding Cargo.lock revision.

This local patch removes the mandatory chrono edge from xmpp-parsers, using
time 0.3 for XEP-0082 dates and XEP-0202 entity time. It addresses
RUSTSEC-2026-0543 (unmaintained dependency) and downstream security review
requests. Utc::now and timestamp are supported APIs; the advisory concerns
library maintenance.

The patch is included directly in ZeroClaw PR #11847. It requires no follow-up
PR to be merged first. xmpp-rs currently declines AI-assisted contributions;
no submission to that project is claimed.

## API and wire changes

DateTime wraps time::OffsetDateTime. timezone/with_timezone use UtcOffset.
format takes time format descriptions and returns Result. TimeResult uses
UtcOffset and OffsetDateTime; its conversion normalizes UTC. Wire encoding
retains fractional seconds and the sign of negative sub-hour offsets.
Malformed RFC3339 dates remain rejected. Parse error types/messages change.
ZeroClaw and tokio-xmpp do not call the replaced public date APIs.

## Scope and removal

Other workspace chrono/chrono-tz users remain tracked separately. This patch
removes the XMPP dependency edge without waiving the security gate.
Remove the override when a compatible maintained parser release drops chrono.


## Validation

Run independently from the repository root:

```sh
cargo test --manifest-path vendor/xmpp-parsers-0.23.0/Cargo.toml --lib
cargo test --manifest-path vendor/xmpp-parsers-0.23.0/Cargo.toml --lib --features pedantic
cargo check --locked -p zeroclaw-channels --no-default-features --features channel-xmpp --tests
```

The default parser suite passed 640 tests with 8 existing ignored tests.
The strict XML suite passed 669 tests with 8 existing ignored tests.
The tokio-xmpp dependency subtree was inspected and contains no chrono edge.
The channel regression test parses an actual XMPP delayed-delivery XML element,
checking fractional precision, negative sub-hour offsets and invalid inputs.

The channel test targets passed cargo check on Windows with Rust 1.98.1.
Existing platform-specific dead-code warnings remain in unchanged modules.
