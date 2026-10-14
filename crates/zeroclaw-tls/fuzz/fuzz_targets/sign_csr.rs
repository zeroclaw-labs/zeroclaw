//! Fuzz the enrollment CSR parser (A7/A12): `sign_csr` parses an attacker-supplied
//! PKCS#10 CSR. It must never panic on arbitrary input; it either rejects the CSR
//! or issues a leaf, and a CSR is fuzzed against a fixed real CA (we are testing
//! the parser, not the CA).
#![no_main]

use libfuzzer_sys::fuzz_target;
use std::sync::OnceLock;

static CA: OnceLock<(String, String)> = OnceLock::new();

fuzz_target!(|data: &[u8]| {
    let (ca_crt, ca_key) = CA.get_or_init(zeroclaw_tls::testing::gen_ca);
    if let Ok(csr) = std::str::from_utf8(data) {
        // Either Ok(leaf) or Err(rejected) - never a panic.
        let _ = zeroclaw_tls::sign_csr(ca_crt, ca_key, "dev_fuzz", csr);
    }
});
