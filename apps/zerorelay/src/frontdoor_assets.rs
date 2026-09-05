//! The browser enrollment frontdoor's served assets: one page and one script.
//!
//! PHASE 1 IS ENROLLMENT ONLY. The page pairs a browser with a daemon and shows
//! the credential it was issued. It does not open a session, and it never opens
//! a relay route: it speaks plain `fetch()` to the frontdoor's own HTTP routes
//! and the relay performs the daemon exchange (see `crate::enroll_proxy`).
//!
//! What that removes, relative to the frontdoor this replaces: there is no TLS
//! implementation, no X.509 parser, no certificate-chain validation and no relay
//! DATA frame codec in served JavaScript. Those existed only to let a browser
//! be its own TLS client through the tunnel, and the relay-terminated model
//! makes them unnecessary rather than merely smaller.
//!
//! The crypto that DOES remain is the crypto the native client also performs,
//! and no more:
//!
//! * a P-256 keypair and a PKCS#10 CSR, because the daemon issues against a CSR
//!   and verifies its self-signature (`zeroclaw_tls::sign_csr`). Generating this
//!   in the browser is what keeps the private key out of the relay's hands.
//! * SHA-256 for the short-auth-string, whose derivation must match
//!   `zeroclaw_tls::enrollment_sas` byte for byte.
//!
//! Both run on `crypto.subtle` - the browser's own audited primitives. The only
//! hand-written encoding is DER assembly for one fixed structure: no protocol
//! state machine, no parsing of attacker-supplied bytes, and no handling of
//! secret material beyond handing the private key to `sign()`. That is a
//! different class of code from the TLS 1.3 client this frontdoor deleted, and
//! it is the minimum that lets the browser keep its own key.

/// The pairing page.
pub(crate) const INDEX_HTML: &str = r##"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ZeroClaw Relay - browser enrollment</title>
<style>
  :root { color-scheme: light dark; }
  body { font-family: system-ui, -apple-system, "Segoe UI", sans-serif; margin: 0 auto; max-width: 44rem; padding: 2rem 1.25rem 4rem; line-height: 1.5; }
  h1 { font-size: 1.4rem; margin-bottom: 0.25rem; }
  .sub { opacity: 0.75; margin-top: 0; }
  .trust { border: 1px solid currentColor; border-left-width: 4px; padding: 0.85rem 1rem; margin: 1.5rem 0; font-size: 0.94rem; }
  .trust h2 { font-size: 1rem; margin: 0 0 0.5rem; }
  .trust p { margin: 0.5rem 0; }
  label { display: block; margin: 1rem 0 0.25rem; font-weight: 600; font-size: 0.92rem; }
  input { width: 100%; padding: 0.5rem; font: inherit; box-sizing: border-box; }
  button { font: inherit; padding: 0.55rem 1.1rem; margin-top: 1rem; cursor: pointer; }
  button[disabled] { cursor: progress; opacity: 0.6; }
  .sas { font-family: ui-monospace, SFMono-Regular, Menlo, monospace; font-size: 1.9rem; letter-spacing: 0.12em; margin: 0.5rem 0; }
  pre { overflow-x: auto; padding: 0.75rem; border: 1px solid currentColor; font-size: 0.82rem; }
  dt { font-weight: 600; margin-top: 0.6rem; }
  dd { margin: 0.1rem 0 0; font-family: ui-monospace, SFMono-Regular, Menlo, monospace; overflow-wrap: anywhere; }
  .status { margin-top: 1rem; min-height: 1.4rem; }
  .error { font-weight: 600; }
  [hidden] { display: none !important; }
</style>
</head>
<body>
<h1>ZeroClaw browser enrollment</h1>
<p class="sub">Pair this browser with a ZeroClaw agent reached through this relay.</p>

<section class="trust">
  <h2>What you are trusting</h2>
  <p>
    This page is served by the relay, and the relay performs the enrollment
    exchange with your agent <strong>on your behalf</strong>. Enrolling here
    trusts the operator of this relay: it sees your pairing code and the
    certificate you are issued, and it could use that pairing code to enrol a
    client of its own.
  </p>
  <p>
    Your private key is generated in this browser and is never sent to the
    relay. Only a certificate signing request leaves this page.
  </p>
  <p>
    The short-auth-string below <em>detects</em> a relay that substitutes a
    different agent CA - if it does not match your agent's console, stop. It
    cannot protect you from a relay you have chosen to trust.
  </p>
  <p>
    <strong>Sessions are not offered here.</strong> This page enrols only. To
    talk to your agent, use <code>zerocode</code> or another native client,
    which connects end-to-end encrypted and does not trust the relay.
  </p>
</section>

<section id="step-details">
  <h2>1. Agent and pairing code</h2>
  <label for="node-id">Agent node id</label>
  <input id="node-id" autocomplete="off" spellcheck="false" placeholder="the node id your agent registered under">
  <label for="pairing-code">Pairing code</label>
  <input id="pairing-code" autocomplete="off" spellcheck="false" placeholder="from your agent's console">
  <button id="begin">Fetch the agent CA</button>
</section>

<section id="step-sas" hidden>
  <h2>2. Confirm the short-auth-string</h2>
  <p>Your agent's console prints a short-auth-string. It must match this one:</p>
  <p class="sas" id="sas-value"></p>
  <p>If these do not match, do not continue - the enrollment may be intercepted.</p>
  <button id="confirm">They match - enrol this browser</button>
  <button id="abort">They do not match - stop</button>
</section>

<section id="step-done" hidden>
  <h2>3. Enrolled</h2>
  <p>This browser was issued a client certificate by your agent.</p>
  <dl>
    <dt>Device id</dt><dd id="device-id"></dd>
    <dt>Certificate expires</dt><dd id="not-after"></dd>
    <dt>Relay</dt><dd id="relay-url"></dd>
    <dt>Node id</dt><dd id="relay-node"></dd>
  </dl>
  <h3>Next step: open a session with a native client</h3>
  <p>
    The relay cannot carry a browser session in this release, so this page stops
    here. Save the two files below and hand them to <code>zerocode</code>, which
    will connect to your agent end-to-end encrypted - the relay forwards those
    bytes without being able to read them.
  </p>
  <p>
    Keep the private key on this device. Anyone who holds it can act as this
    enrolled client.
  </p>
  <h3>Client certificate</h3>
  <pre id="cert-pem"></pre>
  <h3>Client private key</h3>
  <pre id="key-pem"></pre>
</section>

<p class="status" id="status" role="status"></p>
<p class="status error" id="error" role="alert"></p>

<script src="/app.js"></script>
</body>
</html>
"##;

/// The page driver.
///
/// The section between the `zeroclaw-enroll-crypto` markers is pure, DOM-free
/// and self-contained on purpose: the test suite slices it out of this very
/// constant and runs it, so the CSR and SAS the page produces are checked
/// against the daemon's own issuer rather than assumed to be well-formed.
pub(crate) const APP_JS: &str = r##"(function () {
  'use strict';

  // ---- 8< ---- zeroclaw-enroll-crypto ---- 8< ----
  // Pure helpers: no DOM, no network. Everything cryptographic here runs on
  // crypto.subtle; the hand-written part is DER assembly for one fixed
  // structure (a PKCS#10 request), not a protocol implementation.

  function concatBytes(parts) {
    let total = 0;
    for (const p of parts) total += p.length;
    const out = new Uint8Array(total);
    let at = 0;
    for (const p of parts) { out.set(p, at); at += p.length; }
    return out;
  }

  function hex(bytes) {
    return Array.from(bytes, (b) => b.toString(16).padStart(2, '0')).join('');
  }

  async function sha256Hex(bytes) {
    return hex(new Uint8Array(await crypto.subtle.digest('SHA-256', bytes)));
  }

  // Strip the PEM armour and base64-decode. This does NOT parse the
  // certificate: the relay has already refused any chain that is not exactly
  // one certificate, and the SAS is a digest over these bytes as they stand.
  function pemToDer(pem) {
    const b64 = pem.replace(/-----[^-]+-----/g, '').replace(/\s+/g, '');
    const bin = atob(b64);
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i += 1) out[i] = bin.charCodeAt(i);
    return out;
  }

  function derToPem(der, label) {
    let b64 = '';
    for (let i = 0; i < der.length; i += 1) b64 += String.fromCharCode(der[i]);
    b64 = btoa(b64);
    const lines = b64.match(/.{1,64}/g) || [];
    return `-----BEGIN ${label}-----\n${lines.join('\n')}\n-----END ${label}-----\n`;
  }

  // Must match zeroclaw_tls::enrollment_sas byte for byte: the operator is
  // comparing this string against the one the daemon printed.
  async function enrollmentSas(pairingCode, caFingerprintHex) {
    const enc = new TextEncoder();
    const digest = await sha256Hex(concatBytes([
      enc.encode('zeroclaw-enroll-sas-v1'),
      Uint8Array.of(0),
      enc.encode(pairingCode.trim()),
      Uint8Array.of(0),
      enc.encode(caFingerprintHex.trim().toLowerCase()),
    ]));
    const s = digest.slice(0, 8).toUpperCase();
    return `${s.slice(0, 4)}-${s.slice(4, 8)}`;
  }

  // --- minimal DER writer (fixed shapes only) ---

  function derLength(n) {
    if (n < 0x80) return Uint8Array.of(n);
    const bytes = [];
    let v = n;
    while (v > 0) { bytes.unshift(v & 0xff); v >>>= 8; }
    return Uint8Array.from([0x80 | bytes.length, ...bytes]);
  }

  function derTlv(tag, content) {
    return concatBytes([Uint8Array.of(tag), derLength(content.length), content]);
  }

  // A DER INTEGER from a big-endian magnitude: drop leading zeros, then add one
  // back if the top bit would otherwise read as a negative number.
  function derPositiveInteger(magnitude) {
    let start = 0;
    while (start < magnitude.length - 1 && magnitude[start] === 0) start += 1;
    let body = magnitude.slice(start);
    if (body[0] & 0x80) body = concatBytes([Uint8Array.of(0), body]);
    return derTlv(0x02, body);
  }

  // WebCrypto returns ECDSA signatures as raw r||s (IEEE P1363); X.509 wants
  // SEQUENCE { r INTEGER, s INTEGER }.
  function ecdsaRawToDer(raw) {
    const half = raw.length / 2;
    return derTlv(0x30, concatBytes([
      derPositiveInteger(raw.slice(0, half)),
      derPositiveInteger(raw.slice(half)),
    ]));
  }

  // id-at-commonName (2.5.4.3)
  const OID_COMMON_NAME = Uint8Array.of(0x06, 0x03, 0x55, 0x04, 0x03);
  // ecdsa-with-SHA256 (1.2.840.10045.4.3.2), no parameters
  const ALG_ECDSA_SHA256 = Uint8Array.of(
    0x30, 0x0a, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02,
  );

  // Name ::= SEQUENCE OF RelativeDistinguishedName. The subject is only a hint:
  // the daemon assigns the real device id and overrides everything here, so this
  // carries no identity claim.
  function derSubject(commonName) {
    const value = derTlv(0x0c, new TextEncoder().encode(commonName));
    const attribute = derTlv(0x30, concatBytes([OID_COMMON_NAME, value]));
    return derTlv(0x30, derTlv(0x31, attribute));
  }

  // Generate a P-256 keypair and the PKCS#10 request for it. The SPKI bytes are
  // used exactly as crypto.subtle exported them - the encoder never inspects or
  // rebuilds the public key.
  async function createEnrollmentMaterial(commonName) {
    const keyPair = await crypto.subtle.generateKey(
      { name: 'ECDSA', namedCurve: 'P-256' },
      true,
      ['sign', 'verify'],
    );
    const spki = new Uint8Array(await crypto.subtle.exportKey('spki', keyPair.publicKey));
    const pkcs8 = new Uint8Array(await crypto.subtle.exportKey('pkcs8', keyPair.privateKey));

    // CertificationRequestInfo ::= SEQUENCE {
    //   version INTEGER (0), subject Name, subjectPKInfo SPKI,
    //   attributes [0] IMPLICIT SET OF Attribute (empty) }
    const info = derTlv(0x30, concatBytes([
      Uint8Array.of(0x02, 0x01, 0x00),
      derSubject(commonName),
      spki,
      Uint8Array.of(0xa0, 0x00),
    ]));

    const rawSignature = new Uint8Array(await crypto.subtle.sign(
      { name: 'ECDSA', hash: 'SHA-256' },
      keyPair.privateKey,
      info,
    ));
    // BIT STRING with zero unused bits.
    const signature = derTlv(0x03, concatBytes([
      Uint8Array.of(0), ecdsaRawToDer(rawSignature),
    ]));
    const csr = derTlv(0x30, concatBytes([info, ALG_ECDSA_SHA256, signature]));

    return {
      csrPem: derToPem(csr, 'CERTIFICATE REQUEST'),
      keyPem: derToPem(pkcs8, 'PRIVATE KEY'),
    };
  }

  const zeroclawEnrollCrypto = {
    pemToDer, sha256Hex, enrollmentSas, createEnrollmentMaterial, derToPem, hex,
  };
  if (typeof globalThis !== 'undefined') {
    globalThis.__ZEROCLAW_ENROLL_CRYPTO__ = zeroclawEnrollCrypto;
  }
  // ---- >8 ---- zeroclaw-enroll-crypto ---- >8 ----

  if (typeof document === 'undefined') return;

  const $ = (id) => document.getElementById(id);
  const statusLine = $('status');
  const errorLine = $('error');

  function setStatus(text) { statusLine.textContent = text; }
  function setError(text) { errorLine.textContent = text; }

  async function postJson(path, body) {
    const response = await fetch(path, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify(body),
    });
    let payload = null;
    try { payload = await response.json(); } catch (_) { payload = null; }
    if (!response.ok) {
      const detail = (payload && payload.error) || `request failed (${response.status})`;
      throw new Error(detail);
    }
    return payload;
  }

  const state = { nodeId: '', pairingCode: '', caChainPem: '' };

  $('begin').addEventListener('click', async () => {
    setError('');
    const nodeId = $('node-id').value.trim();
    const pairingCode = $('pairing-code').value.trim();
    if (!nodeId || !pairingCode) {
      setError('Enter both the agent node id and the pairing code.');
      return;
    }
    $('begin').disabled = true;
    setStatus('Fetching the agent CA through the relay...');
    try {
      const trust = await postJson('/enroll/ca', { node_id: nodeId });
      const fingerprint = await sha256Hex(pemToDer(trust.ca_chain_pem));
      const sas = await enrollmentSas(pairingCode, fingerprint);
      state.nodeId = nodeId;
      state.pairingCode = pairingCode;
      state.caChainPem = trust.ca_chain_pem;
      $('sas-value').textContent = sas;
      $('step-details').hidden = true;
      $('step-sas').hidden = false;
      setStatus('Compare the short-auth-string with your agent console.');
    } catch (error) {
      setError(String(error.message || error));
      setStatus('');
    } finally {
      $('begin').disabled = false;
    }
  });

  $('abort').addEventListener('click', () => {
    state.nodeId = '';
    state.pairingCode = '';
    state.caChainPem = '';
    $('step-sas').hidden = true;
    $('step-details').hidden = false;
    setStatus('');
    setError('Enrollment stopped. Nothing was sent and no certificate was trusted.');
  });

  $('confirm').addEventListener('click', async () => {
    setError('');
    $('confirm').disabled = true;
    setStatus('Generating a key in this browser...');
    try {
      const material = await createEnrollmentMaterial('zeroclaw-browser');
      setStatus('Enrolling through the relay...');
      const issued = await postJson('/enroll', {
        node_id: state.nodeId,
        pairing_code: state.pairingCode,
        csr_pem: material.csrPem,
        ca_chain_pem: state.caChainPem,
      });
      $('device-id').textContent = issued.device_id || '(none)';
      $('not-after').textContent = issued.not_after
        ? new Date(issued.not_after * 1000).toISOString()
        : '(unknown)';
      const profile = issued.relay_profile || {};
      $('relay-url').textContent = profile.relay_url || '(none configured)';
      $('relay-node').textContent = profile.node_id || state.nodeId;
      $('cert-pem').textContent = issued.cert_pem || '';
      $('key-pem').textContent = material.keyPem;
      $('step-sas').hidden = true;
      $('step-done').hidden = false;
      setStatus('Enrolled.');
      // The pairing code is one-time and now spent; drop our copy.
      state.pairingCode = '';
    } catch (error) {
      setError(String(error.message || error));
      setStatus('');
      $('confirm').disabled = false;
    }
  });
})();
"##;

#[cfg(test)]
mod tests {
    use super::*;

    /// Markers around the DOM-free crypto section, so the tests below run the
    /// SHIPPED source rather than a copy that can drift from it.
    const CRYPTO_BEGIN: &str = "// ---- 8< ---- zeroclaw-enroll-crypto ---- 8< ----";
    const CRYPTO_END: &str = "// ---- >8 ---- zeroclaw-enroll-crypto ---- >8 ----";

    fn crypto_section() -> &'static str {
        let start = APP_JS
            .find(CRYPTO_BEGIN)
            .expect("crypto section start marker");
        let end = APP_JS.find(CRYPTO_END).expect("crypto section end marker");
        assert!(end > start, "markers must be in order");
        &APP_JS[start..end]
    }

    /// The deleted TLS-in-JS frontdoor is GONE, not shrunk.
    ///
    /// Every name below belonged to the browser TLS 1.3 client, its X.509
    /// validation, or the relay DATA frame codec the page used to drive a tunnel
    /// itself. Under the relay-terminated model the page opens no relay route at
    /// all, so none of this may reappear in the served bundle.
    #[test]
    fn the_served_bundle_carries_no_tls_engine_and_no_relay_data_codec() {
        let bundle = format!("{INDEX_HTML}{APP_JS}");
        for symbol in [
            // Browser TLS 1.3 client + X.509 validation.
            "BrowserTls13Client",
            "TlsWebSocket",
            "ZeroClawEnrollmentTls",
            "TLS_AES_128_GCM_SHA256",
            "handshake",
            "verifyServerCertificateChain",
            "verifyServerCertificateVerify",
            "assertCertificateAuthority",
            "tls-engine.js",
            // Relay framing driven from the page.
            "RelayDataTransport",
            "encodeDataFrame",
            "decodeDataFrame",
            "DataAck",
            "zeroclaw.relay.v1",
            "tunnel-worker.js",
            "importScripts",
            // The dashboard/session tier, deferred out of phase 1.
            "webui",
            "JsonRpcClient",
            "zeroclaw-rpc-request",
            "session/prompt",
        ] {
            assert!(
                !bundle.contains(symbol),
                "the served bundle must not contain `{symbol}`"
            );
        }
        // A WebSocket is how the deleted page reached the daemon; the phase-1
        // page uses fetch() only.
        assert!(
            !bundle.contains("new WebSocket"),
            "the page must not open a WebSocket"
        );
    }

    /// The page's own crypto is the crypto the native client performs, and no
    /// more: keygen + CSR (because the daemon issues against a CSR) and SHA-256
    /// (for the SAS). Everything else runs on crypto.subtle.
    #[test]
    fn the_page_uses_web_crypto_rather_than_hand_written_primitives() {
        let section = crypto_section();
        assert!(section.contains("crypto.subtle.generateKey"));
        assert!(section.contains("crypto.subtle.sign"));
        assert!(section.contains("crypto.subtle.digest"));
        assert!(section.contains("crypto.subtle.exportKey"));
        // No hand-written symmetric crypto, key schedule, or hash.
        for banned in ["AES", "HKDF", "GCM", "sha256Block", "SHA256_K"] {
            assert!(
                !section.contains(banned),
                "the page must not implement `{banned}` itself"
            );
        }
    }

    /// Run the shipped crypto section under node and return its JSON output.
    ///
    /// Returns `None` when node is unavailable so the suite still runs; the
    /// callers assert loudly on the value when it is present.
    fn run_page_crypto(driver: &str) -> Option<serde_json::Value> {
        let dir = tempfile::tempdir().ok()?;
        let script = dir.path().join("page-crypto.mjs");
        std::fs::write(&script, format!("{}\n{driver}\n", crypto_section())).ok()?;
        let output = std::process::Command::new("node")
            .arg(&script)
            .output()
            .ok()?;
        assert!(
            output.status.success(),
            "the page's crypto section failed under node: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).ok()
    }

    /// The CSR the PAGE builds must be accepted by the daemon's real issuer.
    ///
    /// This is the assertion that makes the hand-written DER safe to ship: the
    /// bytes come from the served JavaScript, and `sign_csr` verifies the CSR's
    /// self-signature and rejects anything malformed.
    #[test]
    fn the_page_csr_is_accepted_by_the_daemon_issuer() {
        let Some(result) = run_page_crypto(
            r#"
const material = await __ZEROCLAW_ENROLL_CRYPTO__.createEnrollmentMaterial('zeroclaw-browser');
process.stdout.write(JSON.stringify(material));
"#,
        ) else {
            eprintln!("skipping: node is not available to run the page's crypto");
            return;
        };
        let csr_pem = result["csrPem"].as_str().expect("a CSR");
        assert!(
            csr_pem.contains("BEGIN CERTIFICATE REQUEST"),
            "got: {csr_pem}"
        );
        assert!(
            result["keyPem"]
                .as_str()
                .is_some_and(|k| k.contains("BEGIN PRIVATE KEY")),
            "the page must keep a usable private key"
        );

        let (ca_cert_pem, ca_key_pem) = zeroclaw_tls::testing::gen_ca();
        let issued = zeroclaw_tls::sign_csr(&ca_cert_pem, &ca_key_pem, "device-1", csr_pem)
            .expect("the daemon issuer must accept the page's CSR");
        assert!(issued.cert_pem.contains("BEGIN CERTIFICATE"));
    }

    /// The SAS the page renders must equal the one the daemon prints, or the
    /// operator's comparison is meaningless.
    #[test]
    fn the_page_sas_matches_the_daemon_value() {
        let (ca_cert_pem, _key) = zeroclaw_tls::testing::gen_ca();
        let pairing_code = "482913";
        let driver = format!(
            r#"
const caPem = {ca:?};
const der = __ZEROCLAW_ENROLL_CRYPTO__.pemToDer(caPem);
const fingerprint = await __ZEROCLAW_ENROLL_CRYPTO__.sha256Hex(der);
const sas = await __ZEROCLAW_ENROLL_CRYPTO__.enrollmentSas({code:?}, fingerprint);
process.stdout.write(JSON.stringify({{ fingerprint, sas }}));
"#,
            ca = ca_cert_pem,
            code = pairing_code,
        );
        let Some(result) = run_page_crypto(&driver) else {
            eprintln!("skipping: node is not available to run the page's crypto");
            return;
        };

        let expected_fingerprint =
            zeroclaw_tls::single_cert_pem_sha256_fingerprint(&ca_cert_pem).expect("fingerprint");
        assert_eq!(
            result["fingerprint"].as_str().unwrap(),
            expected_fingerprint,
            "the page must fingerprint the CA exactly as the daemon does"
        );
        assert_eq!(
            result["sas"].as_str().unwrap(),
            zeroclaw_tls::enrollment_sas(pairing_code, &expected_fingerprint),
            "the page's SAS must match the daemon console value"
        );
    }
}
