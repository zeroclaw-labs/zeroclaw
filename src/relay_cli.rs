//! `zeroclaw relay claim` — bind this daemon to a ZeroRelay account (self-serve).
//!
//! Derives the daemon's relay-registration identity, proves control of it with an
//! Ed25519 signature, POSTs the proof to the control plane's `/v1/claim` endpoint,
//! and on success writes the `[relay]` config so the daemon registers against the
//! now-allowlisted relay on its next start. The byte-exact claim proof is built by
//! `zeroclaw_runtime::relay_claim`, next to the registration key it must agree with.

use std::time::Duration;

use anyhow::{Context, Result};
use zeroclaw_config::schema::Config;

/// Cap on the control plane's response body. A `/v1/claim` result is a tiny JSON
/// object; anything larger is refused rather than buffered, so a hostile or
/// malfunctioning endpoint cannot make the CLI read an unbounded body into memory.
const MAX_CLAIM_RESPONSE_BYTES: usize = 64 * 1024;

/// Upper bound on the claimed relay address (`host:port`). Generous for any real
/// hostname, tight enough that a runaway value cannot bloat the written config.
const MAX_RELAY_ADDR_LEN: usize = 255;

/// Upper bound on the claimed node-id (an opaque ~32-hex capability in practice).
const MAX_NODE_ID_LEN: usize = 128;

/// A successful `/v1/claim` result: the relay to register against and the node-id
/// bound to this daemon. Field names mirror the control plane's response body.
#[derive(Debug)]
struct Claimed {
    relay_addr: String,
    node_id: String,
}

/// Build the `POST /v1/claim` request body: exactly the four fields the control
/// plane accepts (it rejects unknown fields), each carrying the proof's wire
/// encoding.
fn claim_request_body(
    proof: &zeroclaw_runtime::relay_claim::ClaimProof,
    claim_token: &str,
) -> serde_json::Value {
    serde_json::json!({
        "fingerprint": proof.fingerprint,
        "public_key": proof.public_key_b64,
        "claim_token": claim_token,
        "signature": proof.signature_b64,
    })
}

/// Validate the claimed relay address the same way the relay path constrains a
/// configured `relay.url`: it is a `host:port` dial target whose host portion
/// becomes the outer-TLS SNI and the `wss://` authority. Reject control
/// characters and whitespace (they would corrupt the persisted TOML value and
/// the derived URI), an over-long value, and anything that is not `host:port`
/// with a non-empty host and a `1..=65535` port. The daemon further rejects a
/// host that is not a valid TLS server name at connect time; this surfaces the
/// obvious failures early so no unusable config is written.
fn validate_relay_addr(addr: &str) -> Result<()> {
    if addr.len() > MAX_RELAY_ADDR_LEN {
        anyhow::bail!(
            "the control plane returned an over-long `relay_addr` ({} bytes); no config was written",
            addr.len()
        );
    }
    if addr.chars().any(|c| c.is_control() || c.is_whitespace()) {
        anyhow::bail!(
            "the control plane returned a `relay_addr` with whitespace or control characters; \
             no config was written"
        );
    }
    let (host, port) = addr.rsplit_once(':').context(
        "the control plane returned a `relay_addr` that is not `host:port`; no config was written",
    )?;
    if host.is_empty() {
        anyhow::bail!(
            "the control plane returned a `relay_addr` with an empty host; no config was written"
        );
    }
    // Refuse IPv6 rather than persist a profile that cannot connect.
    //
    // The two consumers of `relay_host` disagree about form: the TLS server-name
    // parser wants a BARE address (`2001:db8::1`) and rejects the bracketed URI
    // form, while the adjacent `wss://{relay_host}/` builder REQUIRES brackets.
    // Storing either one therefore breaks the other, and the failure surfaces
    // later as `invalid relay host` at registration - long after this command
    // has reported a successful claim. Stripping brackets is not a fix for the
    // same reason. IPv6 relay transport is simply not supported yet, so say so
    // here, before anything is written.
    if host.starts_with('[') || host.contains(':') {
        anyhow::bail!(
            "the control plane returned an IPv6 `relay_addr` ({addr}); IPv6 relay addresses are \
             not supported yet, because the TLS name and WebSocket URI consumers require \
             different forms. No config was written - ask the control plane for a hostname or \
             IPv4 address."
        );
    }
    // Check the host with the same parser the daemon uses when it connects, so
    // a host the relay path cannot use (such as `relay/evil`) is refused here
    // rather than saved and then rejected at registration.
    if zeroclaw_runtime::relay::relay_server_name(host).is_err() {
        anyhow::bail!(
            "the control plane returned a `relay_addr` whose host ({host}) is not a valid relay \
             hostname or IPv4 address; no config was written"
        );
    }
    match port.parse::<u16>() {
        Ok(p) if p != 0 => Ok(()),
        _ => anyhow::bail!(
            "the control plane returned a `relay_addr` whose port is not 1..=65535; no config was written"
        ),
    }
}

/// Validate the claimed node-id. It is an opaque capability the daemon registers
/// verbatim, so the constraints are minimal but real: bounded length, and no
/// control characters or whitespace that would corrupt the persisted config.
fn validate_node_id(node_id: &str) -> Result<()> {
    if node_id.len() > MAX_NODE_ID_LEN {
        anyhow::bail!(
            "the control plane returned an over-long `node_id` ({} bytes); no config was written",
            node_id.len()
        );
    }
    if node_id.chars().any(|c| c.is_control() || c.is_whitespace()) {
        anyhow::bail!(
            "the control plane returned a `node_id` with whitespace or control characters; \
             no config was written"
        );
    }
    Ok(())
}

/// Interpret the control plane's response. A non-success status surfaces the
/// server's error and returns `Err`, so the caller writes no config. A success
/// status must carry `relay_addr` and `node_id`, and both must pass the same
/// constraints the relay path enforces; anything else is an error.
fn claim_outcome(status: u16, body: &str) -> Result<Claimed> {
    if !(200..300).contains(&status) {
        anyhow::bail!(
            "the ZeroRelay control plane rejected the claim (HTTP {status}): {body}. \
             No config was written. Confirm the token is correct and unspent, then retry."
        );
    }
    let parsed: serde_json::Value = serde_json::from_str(body).with_context(|| {
        format!("the control plane returned HTTP {status} with a body that is not JSON; no config was written")
    })?;
    let relay_addr = parsed
        .get("relay_addr")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .context("the control plane response is missing `relay_addr`; no config was written")?
        .to_string();
    let node_id = parsed
        .get("node_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
        .context("the control plane response is missing `node_id`; no config was written")?
        .to_string();
    validate_relay_addr(&relay_addr)?;
    validate_node_id(&node_id)?;
    Ok(Claimed {
        relay_addr,
        node_id,
    })
}

/// Persist a successful claim into the daemon's config: enable the relay bridge
/// and pin the relay address and node-id. Routes through the comment-aware
/// `set_prop_persistent` + `save_dirty` writer, so comments and unrelated sections
/// survive. The write is atomic, so a failure leaves the prior config intact.
async fn write_claim_config(config: &mut Config, claimed: &Claimed) -> Result<()> {
    let known: Vec<String> = config.prop_fields().into_iter().map(|f| f.name).collect();
    // Derive the outer-TLS SNI / `wss://` host from the freshly claimed relay
    // address the same way the daemon does — the host portion of `host:port`
    // (see `handle_run`'s `relay_host` derivation). Persisting it OVERWRITES any
    // stale operator-set `[relay].relay_host`: the daemon prefers a non-empty
    // `relay_host` over deriving from `relay.url`, so leaving an old value in
    // place would make it dial the newly claimed relay under the OLD certificate
    // name — a mismatch the "will register" success message would misreport.
    let relay_host = claimed
        .relay_addr
        .rsplit_once(':')
        .map(|(host, _)| host)
        .unwrap_or(claimed.relay_addr.as_str());
    let updates = [
        ("relay.enabled", "true"),
        ("relay.url", claimed.relay_addr.as_str()),
        ("relay.node-id", claimed.node_id.as_str()),
        ("relay.relay-host", relay_host),
    ];
    for (path, value) in updates {
        let resolved = zeroclaw_config::helpers::resolve_field_path(&known, path);
        config.set_prop_persistent(&resolved, value)?;
    }
    // Box the large `Config` save future to stay under the clippy future-size cap.
    Box::pin(config.save_dirty()).await
}

/// True for the exact loopback hosts a cleartext `http://` claim may target.
///
/// `reqwest::Url::host_str` serializes an IPv6 literal bracketed (`[::1]`); accept
/// both that and the bare `::1`, plus the dotted-quad and `localhost`. Anything
/// else — including a loopback *substring* like `127.0.0.1.evil.example` — is not
/// loopback and must not receive the token in the clear.
fn is_loopback_host(host: &str) -> bool {
    matches!(host, "127.0.0.1" | "localhost" | "::1" | "[::1]")
}

/// Reject a `--control` URL that would carry the one-time claim token in the
/// clear or to an origin other than the one it appears to name; return the
/// validated destination on success.
///
/// The authority is parsed with a real URL parser (not string splitting) so that
/// a userinfo-wrapped authority cannot masquerade as loopback: reqwest connects
/// to the *host* of `user[:pass]@host`, not to the loopback-looking userinfo, so
/// `http://127.0.0.1:80@evil.example:9800` would otherwise send the bearer token
/// to `evil.example` in cleartext. Any username or password is therefore
/// rejected outright. HTTPS is required for any real control plane; plain
/// `http://` is allowed only when the parsed host is exactly a loopback host, so
/// local development against a dev control plane (and the wiremock-backed tests
/// below) still works. A query or fragment is rejected because the value must be
/// a bare base URL that `/v1/claim` is appended to. A value with no scheme is
/// rejected here with an actionable message rather than surfacing later as an
/// opaque transport error.
fn ensure_control_is_secure(control: &str) -> Result<String> {
    let url = reqwest::Url::parse(control).map_err(|_| {
        anyhow::Error::msg(format!(
            "--control must be an absolute https:// URL (got `{control}`) — e.g. \
             https://control.zerorelay.net"
        ))
    })?;

    // A `user[:pass]@host` authority makes reqwest connect to `host`, not to the
    // userinfo, so the token would go to `host` — refuse any userinfo outright.
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!(
            "--control must not embed a username or password (`{control}`): a \
             `user@host` URL sends the one-time claim token to `host`, not to the \
             address the userinfo appears to name. Use the plain https URL from \
             your ZeroRelay account."
        );
    }

    // The value is a base URL that `/v1/claim` is appended to; a query or
    // fragment on it is meaningless and would corrupt the request target.
    if url.query().is_some() || url.fragment().is_some() {
        anyhow::bail!("--control must be a bare base URL with no query or fragment (`{control}`)");
    }

    match url.scheme() {
        "https" => Ok(control.to_string()),
        "http" => {
            if is_loopback_host(url.host_str().unwrap_or_default()) {
                Ok(control.to_string())
            } else {
                anyhow::bail!(
                    "--control must use https:// — refusing to send the one-time claim token in \
                     cleartext to a non-loopback http:// address (`{control}`). Use the https URL \
                     from your ZeroRelay account."
                )
            }
        }
        other => anyhow::bail!(
            "--control must be an absolute https:// URL (got scheme `{other}` in `{control}`) — \
             e.g. https://control.zerorelay.net"
        ),
    }
}

/// Handle `zeroclaw relay claim <TOKEN> --control <URL>`.
///
/// Fails closed: a bad token, a non-https control URL, an unreachable control
/// plane, a non-success response, or an unwritable config each abort with an
/// actionable message and never leave a partially written config.
///
/// The claim proof is always signed with the registration key under
/// `config.data_dir` — the exact key the daemon loads and registers with on its
/// next start. There is deliberately no independent data-dir override: proving a
/// key under a different directory would allowlist a fingerprint the daemon then
/// never presents. (`config.data_dir` still honors `ZEROCLAW_DATA_DIR` /
/// `--config-dir`, which move the CLI and the daemon together.)
/// The config paths `relay claim` persists. An env override on any of them
/// would be silently discarded at save time, so the claim must not proceed.
const CLAIM_MANAGED_PATHS: &[&str] = &[
    "relay.enabled",
    "relay.url",
    "relay.node_id",
    "relay.relay_host",
];

/// Fail closed when the environment owns a field this command writes.
fn refuse_claim_field_env_overrides(config: &Config) -> Result<()> {
    let overridden: Vec<&str> = CLAIM_MANAGED_PATHS
        .iter()
        .copied()
        .filter(|path| config.prop_is_env_overridden(path))
        .collect();
    if overridden.is_empty() {
        return Ok(());
    }
    let vars: Vec<String> = overridden
        .iter()
        .map(|p| format!("ZEROCLAW_{}", p.replace('.', "__").to_uppercase()))
        .collect();
    anyhow::bail!(
        "these claim-managed settings are currently set by environment overrides: {}. \
         Environment overrides are never written to the config file, so claiming now would \
         spend your one-time token and then discard the [relay] profile it promised to save. \
         Unset {} and run the claim again. No request was sent and no config was written.",
        overridden.join(", "),
        vars.join(", ")
    );
}

pub async fn handle_claim(config: &mut Config, claim_token: &str, control: &str) -> Result<()> {
    let token = claim_token.trim();
    if token.is_empty() {
        anyhow::bail!(
            "a claim token is required. Get one from your ZeroRelay account, then run: \
             zeroclaw relay claim <TOKEN> --control <URL>"
        );
    }
    let control = control.trim().trim_end_matches('/');
    if control.is_empty() {
        anyhow::bail!("--control <URL> is required (the ZeroRelay control-plane base URL)");
    }
    // Refuse BEFORE the request if the environment already owns a field this
    // command is about to persist.
    //
    // Env overrides are deliberately not saved (see
    // docs/book/src/architecture/config-lifecycle.md), so a claim under an
    // override would spend the one-time token, report success, and then discard
    // the very write it promised - leaving the daemon pointed at the old relay.
    // Failing early keeps the token unspent and the disk config untouched, and
    // it reuses the existing override tracking rather than changing the masking
    // contract.
    refuse_claim_field_env_overrides(config)?;
    // The claim token is a one-time bearer secret; refuse to send it over a
    // channel that would carry it in the clear or to an origin other than the one
    // it names. `control` is the validated destination.
    let control = ensure_control_is_secure(control)?;

    // Sign with the key the daemon will register with: the one under
    // `config.data_dir`. Using any other directory would prove a fingerprint the
    // daemon never presents at registration.
    let data_dir = config.data_dir.clone();
    let signing_key_pkcs8 = zeroclaw_runtime::relay::ensure_signing_key(&data_dir)
        .context("loading the daemon relay registration key")?;
    let proof = zeroclaw_runtime::relay_claim::build_claim_proof(&signing_key_pkcs8, token)?;
    let body = claim_request_body(&proof, token);

    let url = format!("{control}/v1/claim");
    // The one-time claim token rides in this request body. Refuse to follow a
    // redirect (a 307/308 would replay the POST body to whatever origin the
    // `Location` names) and refuse to route through an operator proxy
    // (`HTTP(S)_PROXY`/`ALL_PROXY`) — either would leak the token off the
    // validated destination. Mirrors the sensitive-client pattern in
    // `zeroclaw_runtime::tools::skill_http`.
    let client = reqwest::Client::builder()
        .user_agent(format!("zeroclaw/{}", env!("CARGO_PKG_VERSION")))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .context("building the claim HTTP client")?;
    let response = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .with_context(|| {
            format!("could not reach the ZeroRelay control plane at {url}; check --control and your network")
        })?;
    let status = response.status().as_u16();
    // Read the body under a hard cap so a hostile or malfunctioning control plane
    // cannot make the CLI buffer an unbounded response into memory. A real claim
    // result is a tiny JSON object; an oversized body is refused, not truncated
    // into a misparse.
    // Propagate a read failure rather than defaulting to an empty body: an empty
    // body would be reported as a JSON parse error, hiding the real cause (a
    // dropped connection mid-response, a TLS error) behind a misleading message.
    let (text, overflowed) =
        zeroclaw_tools::helpers::read_response_text(response, Some(MAX_CLAIM_RESPONSE_BYTES))
            .await
            .context(
                "could not read the ZeroRelay control plane's response body; no config was written",
            )?;
    if overflowed {
        anyhow::bail!(
            "the ZeroRelay control plane returned an oversized response (> {MAX_CLAIM_RESPONSE_BYTES} bytes); \
             no config was written"
        );
    }
    let claimed = claim_outcome(status, &text)?;

    Box::pin(write_claim_config(config, &claimed))
        .await
        .with_context(|| {
            format!(
                "the claim succeeded (node-id {}, relay {}) but writing [relay] to {} failed; \
                 set it manually: enabled = true, url = \"{}\", node_id = \"{}\", \
                 relay_host = \"{}\"",
                claimed.node_id,
                claimed.relay_addr,
                config.config_path.display(),
                claimed.relay_addr,
                claimed.node_id,
                // The host the profile would have pinned for TLS - omitting it
                // is how an operator ends up with a mismatched profile.
                claimed
                    .relay_addr
                    .rsplit_once(':')
                    .map_or(claimed.relay_addr.as_str(), |(h, _)| h)
            )
        })?;

    println!(
        "{}",
        crate::ta(
            "cli-relay-claim-ok",
            &[
                ("node_id", claimed.node_id.as_str()),
                ("relay", claimed.relay_addr.as_str()),
            ],
            "Daemon claimed. Start (or restart) the daemon to register against the relay.",
        )
    );
    // The binding is saved either way, but registration additionally requires the
    // WSS listener, which is OFF by default - the relay bridges to it. Promising
    // registration unconditionally would be wrong for most fresh installs, so say
    // what is still needed instead of silently enabling a listener.
    if !config.wss.enabled {
        println!(
            "{}",
            crate::ta(
                "cli-relay-claim-wss-disabled",
                &[],
                "Note: [wss] is disabled, and the relay refuses registration until it is \
                 enabled. The claim above is saved and stays valid - enable the WSS listener \
                 (see the secure-transport guide) and the binding takes effect on the next start.",
            )
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_proof() -> zeroclaw_runtime::relay_claim::ClaimProof {
        // Mint a real registration key the same way the daemon does, so the proof
        // is derived from an authentic PKCS#8 key without depending on `ring` here.
        let tmp = tempfile::TempDir::new().unwrap();
        let pkcs8 = zeroclaw_runtime::relay::ensure_signing_key(tmp.path()).unwrap();
        zeroclaw_runtime::relay_claim::build_claim_proof(&pkcs8, "tok-xyz").unwrap()
    }

    #[test]
    fn request_body_has_exactly_the_four_fields_with_wire_encodings() {
        let proof = sample_proof();
        let body = claim_request_body(&proof, "tok-xyz");
        let obj = body.as_object().expect("body is a JSON object");

        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["claim_token", "fingerprint", "public_key", "signature"]
        );
        assert_eq!(obj["fingerprint"], serde_json::json!(proof.fingerprint));
        assert_eq!(obj["public_key"], serde_json::json!(proof.public_key_b64));
        assert_eq!(obj["claim_token"], serde_json::json!("tok-xyz"));
        assert_eq!(obj["signature"], serde_json::json!(proof.signature_b64));
    }

    #[test]
    fn claim_outcome_parses_success_body() {
        let claimed = claim_outcome(
            200,
            r#"{"node_id":"n-1","relay_addr":"relay.example:8443"}"#,
        )
        .unwrap();
        assert_eq!(claimed.node_id, "n-1");
        assert_eq!(claimed.relay_addr, "relay.example:8443");
    }

    #[test]
    fn claim_outcome_rejects_non_success_status() {
        let err = claim_outcome(403, r#"{"error":"claim_rejected"}"#).unwrap_err();
        assert!(err.to_string().contains("403"));
        assert!(err.to_string().contains("No config was written"));
    }

    #[test]
    fn claim_outcome_rejects_success_without_required_fields() {
        assert!(claim_outcome(200, r#"{"node_id":"n-1"}"#).is_err());
        assert!(claim_outcome(200, r#"{"relay_addr":"r:1"}"#).is_err());
        assert!(claim_outcome(200, "not json").is_err());
    }

    #[test]
    fn claim_outcome_rejects_malformed_relay_addr() {
        // No port.
        assert!(claim_outcome(200, r#"{"node_id":"n","relay_addr":"hostonly"}"#).is_err());
        // Empty host.
        assert!(claim_outcome(200, r#"{"node_id":"n","relay_addr":":8443"}"#).is_err());
        // Port out of range / zero.
        assert!(claim_outcome(200, r#"{"node_id":"n","relay_addr":"h:99999"}"#).is_err());
        assert!(claim_outcome(200, r#"{"node_id":"n","relay_addr":"h:0"}"#).is_err());
        // Whitespace / control characters would corrupt the persisted value.
        assert!(claim_outcome(200, r#"{"node_id":"n","relay_addr":"ho st:8443"}"#).is_err());
        assert!(claim_outcome(200, "{\"node_id\":\"n\",\"relay_addr\":\"h:8443\\n\"}").is_err());
        // A valid hostname / IPv4 host:port is accepted.
        assert!(claim_outcome(200, r#"{"node_id":"n","relay_addr":"relay.example:8443"}"#).is_ok());
        assert!(claim_outcome(200, r#"{"node_id":"n","relay_addr":"192.0.2.10:8443"}"#).is_ok());
    }

    /// An IPv6 `relay_addr` must be refused HERE, before anything is persisted.
    ///
    /// This test previously asserted the opposite - that a bracketed literal was
    /// accepted - which is what let the defect through. Accepting it writes
    /// `relay_host = "[2001:db8::1]"`, and the two consumers of that value want
    /// different forms: the TLS server-name parser rejects the bracketed form,
    /// while the neighbouring `wss://{relay_host}/` builder requires it. The
    /// claim would report success and registration would then fail with
    /// `invalid relay host`, with the bad profile already on disk.
    #[test]
    fn claim_outcome_rejects_ipv6_relay_addr_before_writing_config() {
        for addr in [
            "[2001:db8::1]:8443", // bracketed URI form
            "[::1]:8443",
            "2001:db8::1:8443", // bare form: rsplit_once(':') still leaves colons in the host
        ] {
            let body = format!(r#"{{"node_id":"n","relay_addr":"{addr}"}}"#);
            let err = claim_outcome(200, &body)
                .expect_err("an IPv6 relay_addr must not produce a claim outcome");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("IPv6") && msg.contains("No config was written"),
                "the refusal must name IPv6 and say nothing was written: {msg}"
            );
        }
    }

    /// A host that splits as `host:port` but that the relay connection path
    /// cannot use must be refused before anything is written. `relay/evil`
    /// used to pass, be saved as `relay_host = "relay/evil"`, and only fail at
    /// registration.
    #[test]
    fn claim_outcome_rejects_hosts_the_relay_path_cannot_use() {
        for addr in [
            "relay/evil:8443",
            "user@relay:8443",
            "relay?x:8443",
            "relay..example:8443",
        ] {
            let body = format!(r#"{{"node_id":"n","relay_addr":"{addr}"}}"#);
            let err = claim_outcome(200, &body)
                .expect_err("an unusable relay host must not produce a claim outcome");
            let msg = format!("{err:#}");
            assert!(
                msg.contains("not a valid relay hostname") && msg.contains("no config was written"),
                "{addr}: the refusal must name the host problem and say nothing was written: {msg}"
            );
        }
    }

    #[test]
    fn claim_outcome_rejects_malformed_node_id() {
        // Control character in the node-id.
        assert!(claim_outcome(200, "{\"node_id\":\"n\\u0000\",\"relay_addr\":\"h:1\"}").is_err());
        // Whitespace in the node-id.
        assert!(claim_outcome(200, r#"{"node_id":"n id","relay_addr":"h:1"}"#).is_err());
        // Over-long node-id.
        let huge = "n".repeat(MAX_NODE_ID_LEN + 1);
        let body = format!(r#"{{"node_id":"{huge}","relay_addr":"h:1"}}"#);
        assert!(claim_outcome(200, &body).is_err());
    }

    #[test]
    fn control_url_must_not_leak_the_token_in_cleartext() {
        // https is always fine.
        assert!(ensure_control_is_secure("https://control.zerorelay.net").is_ok());
        assert!(ensure_control_is_secure("https://127.0.0.1:9800").is_ok());

        // http is allowed only to a loopback host (local dev + the wiremock tests).
        assert!(ensure_control_is_secure("http://127.0.0.1:9800").is_ok());
        assert!(ensure_control_is_secure("http://127.0.0.1").is_ok());
        assert!(ensure_control_is_secure("http://localhost:9800").is_ok());
        assert!(ensure_control_is_secure("http://[::1]:9800").is_ok());

        // http to any real host is refused — that would send the one-time token
        // over the wire in the clear.
        let err = ensure_control_is_secure("http://control.zerorelay.net").unwrap_err();
        assert!(err.to_string().contains("https"));
        assert!(ensure_control_is_secure("http://198.51.100.7:9800").is_err());
        // A loopback substring in the host must not fool the check.
        assert!(ensure_control_is_secure("http://127.0.0.1.evil.example").is_err());

        // A scheme-less value is rejected here, not later as an opaque error.
        assert!(ensure_control_is_secure("control.zerorelay.net").is_err());
        assert!(ensure_control_is_secure("ftp://control.zerorelay.net").is_err());
    }

    #[test]
    fn control_url_rejects_userinfo_wrapped_external_hosts() {
        // The bug: reqwest connects to the *host* of `user[:pass]@host`, so a
        // loopback-looking userinfo would ship the token to the real external
        // host in the clear. Every userinfo form must be rejected.
        //
        // `127.0.0.1:80` is parsed as user=127.0.0.1 / pass=80; the host is
        // evil.example — the token would leak there.
        assert!(ensure_control_is_secure("http://127.0.0.1:80@evil.example:9800").is_err());
        // Bracketed-IPv6-looking userinfo, same trick.
        assert!(ensure_control_is_secure("http://[::1]@evil.example:9800").is_err());
        // Bare username, no password.
        assert!(ensure_control_is_secure("http://localhost@evil.example:9800").is_err());
        // Userinfo is rejected even when the host itself IS loopback and even on
        // https — a control URL never legitimately carries credentials.
        assert!(ensure_control_is_secure("http://user@127.0.0.1:9800").is_err());
        assert!(ensure_control_is_secure("https://user:pass@control.zerorelay.net").is_err());

        // A query or fragment is not a valid base URL and is refused.
        assert!(ensure_control_is_secure("https://control.zerorelay.net?x=1").is_err());
        assert!(ensure_control_is_secure("https://control.zerorelay.net#frag").is_err());

        // The happy paths return the validated destination unchanged.
        assert_eq!(
            ensure_control_is_secure("https://control.zerorelay.net").unwrap(),
            "https://control.zerorelay.net"
        );
        assert_eq!(
            ensure_control_is_secure("http://127.0.0.1:9800").unwrap(),
            "http://127.0.0.1:9800"
        );
    }

    #[tokio::test]
    async fn claim_client_refuses_cross_origin_redirect_and_never_leaks_the_body() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The second origin stands in for a redirect target the attacker controls.
        // If the client followed the redirect it would replay the POST (with the
        // claim token) here; it must receive nothing.
        let attacker = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/claim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": "leaked",
                "relay_addr": "attacker:1",
            })))
            .mount(&attacker)
            .await;

        // The control plane the operator pointed at answers with a redirect to
        // the attacker origin.
        let control = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/claim"))
            .respond_with(
                ResponseTemplate::new(307)
                    .insert_header("location", format!("{}/v1/claim", attacker.uri())),
            )
            .mount(&control)
            .await;

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = seed_config(tmp.path());
        let before = std::fs::read_to_string(&config.config_path).unwrap();

        let err = handle_claim(&mut config, "tok-secret", &control.uri())
            .await
            .unwrap_err();
        // The redirect surfaced as a non-success status instead of being followed.
        assert!(err.to_string().contains("307"), "err: {err}");

        // The attacker origin never saw the claim request — the token did not leak.
        let leaked = attacker.received_requests().await.unwrap();
        assert!(
            leaked.is_empty(),
            "the claim client must not follow a redirect to another origin (leaked {} request(s))",
            leaked.len()
        );

        // And a non-success claim wrote no config.
        let after = std::fs::read_to_string(&config.config_path).unwrap();
        assert_eq!(
            before, after,
            "a refused redirect must not touch the config"
        );
    }

    fn seed_config(dir: &std::path::Path) -> Config {
        let config_path = dir.join("config.toml");
        let schema_version = Config::default().schema_version;
        let seed = format!(
            "schema_version = {schema_version}\n\n\
             # Gateway listener — unrelated section, must survive untouched.\n\
             [gateway]\n\
             host = \"127.0.0.1\"\n\
             port = 8080\n\n\
             # Relay bridge settings.\n\
             [relay]\n\
             # keep-this-comment: operator note about the relay\n\
             tofu = false\n"
        );
        std::fs::write(&config_path, seed).unwrap();
        Config {
            config_path,
            data_dir: dir.to_path_buf(),
            ..Default::default()
        }
    }

    /// Seed a config whose `[relay]` already pins a `relay_host` for a *different*
    /// relay than the one about to be claimed, plus a comment and an unrelated
    /// section that must survive the claim write.
    fn seed_config_with_stale_relay_host(dir: &std::path::Path) -> Config {
        let config_path = dir.join("config.toml");
        let schema_version = Config::default().schema_version;
        let seed = format!(
            "schema_version = {schema_version}\n\n\
             # Gateway listener — unrelated section, must survive untouched.\n\
             [gateway]\n\
             host = \"127.0.0.1\"\n\
             port = 8080\n\n\
             [relay]\n\
             # keep-this-comment: operator note about the relay\n\
             relay_host = \"old.example\"\n\
             tofu = false\n"
        );
        std::fs::write(&config_path, seed).unwrap();
        Config {
            config_path,
            data_dir: dir.to_path_buf(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn write_claim_config_overwrites_stale_relay_host_for_consistency() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = seed_config_with_stale_relay_host(tmp.path());

        write_claim_config(
            &mut config,
            &Claimed {
                relay_addr: "new.example:8443".to_string(),
                node_id: "node-xyz".to_string(),
            },
        )
        .await
        .unwrap();

        let written = std::fs::read_to_string(&config.config_path).unwrap();

        // The stale host is gone: it would otherwise be preferred over the new
        // url for TLS SNI + the wss URI, dialing the new relay under the old name.
        assert!(
            !written.contains("old.example"),
            "stale relay_host must not survive the claim:\n{written}"
        );

        // The persisted profile is internally consistent: relay_host is either
        // cleared (daemon derives from url) or equals the new url's host.
        let reloaded: Config = toml::from_str(&written).unwrap();
        assert_eq!(reloaded.relay.url, "new.example:8443");
        let derived_host = reloaded
            .relay
            .url
            .rsplit_once(':')
            .map(|(host, _)| host.to_string())
            .unwrap_or_else(|| reloaded.relay.url.clone());
        assert!(
            reloaded.relay.relay_host.is_empty() || reloaded.relay.relay_host == derived_host,
            "relay_host `{}` is inconsistent with url host `{derived_host}`",
            reloaded.relay.relay_host
        );

        // Comments and the unrelated section still survive byte-for-byte.
        assert!(
            written.contains("# Gateway listener — unrelated section, must survive untouched."),
            "unrelated-section comment lost:\n{written}"
        );
        assert!(
            written.contains("[gateway]\nhost = \"127.0.0.1\"\nport = 8080"),
            "unrelated section body changed:\n{written}"
        );
        assert!(
            written.contains("# keep-this-comment: operator note about the relay"),
            "in-section comment lost:\n{written}"
        );
        assert!(
            written.contains("tofu = false"),
            "sibling field lost:\n{written}"
        );
    }

    #[tokio::test]
    async fn write_claim_config_sets_relay_and_preserves_comments_and_sections() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = seed_config(tmp.path());

        write_claim_config(
            &mut config,
            &Claimed {
                relay_addr: "relay.example:8443".to_string(),
                node_id: "node-abc".to_string(),
            },
        )
        .await
        .unwrap();

        let written = std::fs::read_to_string(&config.config_path).unwrap();

        // [relay] now carries the claimed values.
        assert!(written.contains("enabled = true"), "got:\n{written}");
        assert!(
            written.contains("url = \"relay.example:8443\""),
            "got:\n{written}"
        );
        // Struct fields serialize snake_case on disk (the schema field is
        // `node_id`), even though the CLI prop path is `relay.node-id`.
        assert!(
            written.contains("node_id = \"node-abc\""),
            "got:\n{written}"
        );

        // Comments and the unrelated section survive byte-for-byte.
        assert!(
            written.contains("# Gateway listener — unrelated section, must survive untouched."),
            "unrelated-section comment lost:\n{written}"
        );
        assert!(
            written.contains("[gateway]\nhost = \"127.0.0.1\"\nport = 8080"),
            "unrelated section body changed:\n{written}"
        );
        assert!(
            written.contains("# keep-this-comment: operator note about the relay"),
            "in-section comment lost:\n{written}"
        );
        assert!(
            written.contains("tofu = false"),
            "sibling field lost:\n{written}"
        );

        // The rewrite reparses and reflects the claim.
        let reloaded: Config = toml::from_str(&written).unwrap();
        assert!(reloaded.relay.enabled);
        assert_eq!(reloaded.relay.url, "relay.example:8443");
        assert_eq!(reloaded.relay.node_id, "node-abc");
    }

    #[tokio::test]
    async fn handle_claim_writes_config_and_sends_the_four_field_body_on_success() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = seed_config(tmp.path());

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/claim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": "node-from-server",
                "relay_addr": "relay.server:9443",
            })))
            .expect(1)
            .mount(&server)
            .await;

        handle_claim(&mut config, "tok-live", &server.uri())
            .await
            .unwrap();

        // The request body carried exactly the four fields, with a matching proof.
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let obj = sent.as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            ["claim_token", "fingerprint", "public_key", "signature"]
        );
        assert_eq!(obj["claim_token"], serde_json::json!("tok-live"));
        // Fingerprint on the wire equals sha256(pubkey) — the identity the daemon
        // registers under, proving the request is self-consistent.
        let pubkey = base64_decode(obj["public_key"].as_str().unwrap());
        let expected_fpr = zeroclaw_runtime::relay_claim::fingerprint_of_pubkey(&pubkey);
        assert_eq!(obj["fingerprint"], serde_json::json!(expected_fpr));

        // Config was written from the server's response.
        let written = std::fs::read_to_string(&config.config_path).unwrap();
        assert!(
            written.contains("node_id = \"node-from-server\""),
            "got:\n{written}"
        );
        assert!(
            written.contains("url = \"relay.server:9443\""),
            "got:\n{written}"
        );
        assert!(written.contains("enabled = true"), "got:\n{written}");
    }

    #[tokio::test]
    /// A claim-managed field owned by the environment must stop the command
    /// BEFORE the request: the one-time token stays unspent and the config file
    /// is untouched.
    ///
    /// Without this, the claim would succeed remotely, spend the token, and then
    /// have its promised `[relay]` write discarded by env masking at save time -
    /// leaving the daemon pointed at the old relay while reporting the new one.
    async fn handle_claim_refuses_when_a_claim_field_is_env_overridden() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = seed_config(tmp.path());
        let before = std::fs::read_to_string(&config.config_path).unwrap();

        // A server that FAILS the test if it is ever called: the refusal must
        // happen before any network request.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/claim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": "n", "relay_addr": "relay.example:8443",
            })))
            .expect(0)
            .mount(&server)
            .await;

        for overridden in CLAIM_MANAGED_PATHS {
            config.env_overridden_paths.clear();
            config
                .env_overridden_paths
                .insert((*overridden).to_string());

            let err = handle_claim(&mut config, "tok-live", &server.uri())
                .await
                .expect_err("an env-overridden claim field must refuse the claim");
            let msg = format!("{err:#}");
            assert!(
                msg.contains(overridden),
                "the refusal must name the offending setting ({overridden}): {msg}"
            );
            assert!(
                msg.contains("No request was sent"),
                "the refusal must state that nothing was sent: {msg}"
            );
            assert_eq!(
                std::fs::read_to_string(&config.config_path).unwrap(),
                before,
                "{overridden}: the config file must be untouched"
            );
        }
        // Mock `.expect(0)` is asserted on drop: no POST ever happened.
    }

    #[tokio::test]
    async fn handle_claim_non_200_does_not_write_config() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = seed_config(tmp.path());
        let before = std::fs::read_to_string(&config.config_path).unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/claim"))
            .respond_with(ResponseTemplate::new(403).set_body_json(serde_json::json!({
                "error": "claim_rejected",
                "message": "claim rejected",
            })))
            .mount(&server)
            .await;

        let err = handle_claim(&mut config, "tok-bad", &server.uri())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("403"), "err: {err}");

        // The config file is byte-identical: no half-write on rejection.
        let after = std::fs::read_to_string(&config.config_path).unwrap();
        assert_eq!(before, after, "a rejected claim must not touch the config");
    }

    #[tokio::test]
    async fn handle_claim_with_an_unusable_relay_host_does_not_write_config() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = seed_config(tmp.path());
        let before = std::fs::read_to_string(&config.config_path).unwrap();

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/claim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": "node-ok",
                "relay_addr": "relay/evil:8443",
            })))
            .mount(&server)
            .await;

        let err = handle_claim(&mut config, "tok-host", &server.uri())
            .await
            .expect_err("a claim naming an unusable relay host must fail");
        assert!(
            format!("{err:#}").contains("not a valid relay hostname"),
            "err: {err:#}"
        );

        let after = std::fs::read_to_string(&config.config_path).unwrap();
        assert_eq!(
            before, after,
            "an unusable relay host must not touch the config"
        );
    }

    #[tokio::test]
    async fn handle_claim_rejects_oversized_response_body_without_writing_config() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let tmp = tempfile::TempDir::new().unwrap();
        let mut config = seed_config(tmp.path());
        let before = std::fs::read_to_string(&config.config_path).unwrap();

        // A well-formed success body padded past the cap: the fields are valid, so
        // only the size guard can reject it.
        let pad = "a".repeat(MAX_CLAIM_RESPONSE_BYTES + 4096);
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/claim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": "node-ok",
                "relay_addr": "relay.ok:8443",
                "pad": pad,
            })))
            .mount(&server)
            .await;

        let err = handle_claim(&mut config, "tok-big", &server.uri())
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("oversized"),
            "an oversized body must be refused; err: {err}"
        );

        // No config was written despite the 200 status.
        let after = std::fs::read_to_string(&config.config_path).unwrap();
        assert_eq!(
            before, after,
            "an oversized response must not touch the config"
        );
    }

    #[tokio::test]
    async fn claim_proves_the_key_the_daemon_registers_with_from_config_data_dir() {
        use wiremock::matchers::{method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        // The daemon loads its registration key from `config.data_dir`. Give the
        // config its OWN data dir, distinct from any other directory, so the
        // equality below is a real constraint on which key is proven.
        let cfg_data = tempfile::TempDir::new().unwrap();
        let config_home = tempfile::TempDir::new().unwrap();
        let config_path = config_home.path().join("config.toml");
        std::fs::write(&config_path, "schema_version = 3\n").unwrap();
        let mut config = Config {
            config_path,
            data_dir: cfg_data.path().to_path_buf(),
            ..Default::default()
        };

        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/claim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "node_id": "node-id-eq",
                "relay_addr": "relay.eq:8443",
            })))
            .expect(1)
            .mount(&server)
            .await;

        handle_claim(&mut config, "tok-eq", &server.uri())
            .await
            .unwrap();

        // The fingerprint the claim PROVED (what the control plane allowlisted).
        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 1);
        let sent: serde_json::Value = serde_json::from_slice(&requests[0].body).unwrap();
        let proven_fpr = sent["fingerprint"].as_str().unwrap().to_string();

        // The fingerprint the daemon will REGISTER with: it loads the signing key
        // from `config.data_dir` on start. `ensure_signing_key` is idempotent, so
        // this reads back the very key the claim signed with. The fingerprint is
        // sha256(pubkey) and independent of the token, so any token serves here.
        let daemon_key = zeroclaw_runtime::relay::ensure_signing_key(&config.data_dir).unwrap();
        let daemon_fpr = zeroclaw_runtime::relay_claim::build_claim_proof(&daemon_key, "any")
            .unwrap()
            .fingerprint;
        assert_eq!(
            proven_fpr, daemon_fpr,
            "the claim must prove the exact key the daemon registers with (config.data_dir)"
        );

        // And the equality is dir-specific: a key under a DIFFERENT data dir — the
        // fingerprint the removed `--data-dir` override could have proved — yields
        // a different fingerprint, so this assertion is not vacuous.
        let other = tempfile::TempDir::new().unwrap();
        let other_key = zeroclaw_runtime::relay::ensure_signing_key(other.path()).unwrap();
        let other_fpr = zeroclaw_runtime::relay_claim::build_claim_proof(&other_key, "any")
            .unwrap()
            .fingerprint;
        assert_ne!(
            proven_fpr, other_fpr,
            "a different data dir must yield a different fingerprint"
        );
    }

    // Minimal base64 STANDARD decode for the test assertion above; base64 is a
    // dev-dependency of this crate.
    fn base64_decode(s: &str) -> Vec<u8> {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.decode(s).unwrap()
    }
}
