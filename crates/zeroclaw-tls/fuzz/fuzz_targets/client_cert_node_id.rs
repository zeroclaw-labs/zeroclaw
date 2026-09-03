//! Fuzz the outer client-cert CN reader (A12): `client_cert_node_id` runs an
//! x509-parser over an attacker-supplied certificate DER (the outer-mTLS variant).
//! It must never panic and must return `None` for any unparseable input. The
//! fingerprint helper is exercised alongside it (pure hashing, must never panic).
#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = zeroclaw_tls::client_cert_node_id(data);
    let _ = zeroclaw_tls::cert_sha256_fingerprint(data);
});
