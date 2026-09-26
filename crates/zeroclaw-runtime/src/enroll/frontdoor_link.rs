//! Operator-console hint for browser enrollment through the relay frontdoor.
//!
//! When the daemon mints an enrollment pairing code and a relay is configured,
//! the operator can hand a phone a ready-made link to the relay's browser
//! enrollment page, `https://<relay>/?node=<node-id>&code=<pairing-code>`, and a
//! terminal QR code of the same link. The page fills its two fields from the
//! link and still waits for the user to fetch the agent CA and confirm the
//! short-auth-string, so the link carries no more authority than the code it
//! already shows.
//!
//! The link contains the one-time pairing code, so it is treated exactly like
//! the code: it is written only to the operator's console writer handed in by
//! the caller, never to the structured log, and never retained.

use std::io::{self, Write};

use super::RelayProfile;

/// Build the frontdoor link for `relay_addr` (`[relay].url`, `host:port`).
///
/// The frontdoor is served on the relay's outer TLS listener, so the page is at
/// `https://host:port/`; `:443` is omitted. A `wss://`/`https://` scheme or a
/// trailing path on `relay_addr` is tolerated and dropped. Returns `None` when
/// any part is missing or the address is unusable, so the caller simply skips
/// the hint rather than printing a broken link.
pub fn frontdoor_link(relay_addr: &str, node_id: &str, pairing_code: &str) -> Option<String> {
    let node_id = node_id.trim();
    let pairing_code = pairing_code.trim();
    if node_id.is_empty() || pairing_code.is_empty() {
        return None;
    }
    let authority = relay_authority(relay_addr)?;
    Some(format!(
        "https://{authority}/?node={}&code={}",
        urlencoding::encode(node_id),
        urlencoding::encode(pairing_code),
    ))
}

/// Reduce `[relay].url` to the `host[:port]` authority of the frontdoor page.
fn relay_authority(relay_addr: &str) -> Option<String> {
    let mut rest = relay_addr.trim();
    // Only a TLS scheme is tolerated; the frontdoor is served over TLS. Any
    // other scheme (plaintext or unknown) yields no link rather than a guess.
    for scheme in ["wss://", "https://"] {
        if rest.len() >= scheme.len() && rest[..scheme.len()].eq_ignore_ascii_case(scheme) {
            rest = &rest[scheme.len()..];
            break;
        }
    }
    if rest.contains("://") {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    if authority.is_empty()
        || authority.contains('@')
        || authority
            .chars()
            .any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    // `[v6]:443` / `host:443` -> drop the default https port.
    let authority = match authority.rsplit_once(':') {
        Some((host, "443")) if !host.is_empty() && (!host.contains(':') || host.ends_with(']')) => {
            host
        }
        _ => authority,
    };
    Some(authority.to_string())
}

/// Render `text` as a terminal QR code (two modules per character cell).
///
/// Returns `None` if the payload cannot be encoded; the error is deliberately
/// not formatted with the payload, which contains the pairing code.
pub fn render_terminal_qr(text: &str) -> Option<String> {
    let qr = qrcode::QrCode::new(text.as_bytes()).ok()?;
    Some(
        qr.render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .build(),
    )
}

/// Localized console lines. Kept as a trait-free pair of closures' worth of
/// strings so the writer is testable without the i18n bundle.
pub struct FrontdoorHintText {
    /// Header line introducing the link and QR.
    pub header: String,
    /// Line carrying the link; `{link}` has already been substituted.
    pub link_line: String,
}

/// Write the frontdoor hint for a freshly minted `pairing_code` to `out`.
///
/// Writes nothing (and returns `Ok(false)`) when no relay is configured. The QR
/// block is only rendered when `render_qr` is set, which callers tie to stdout
/// being an interactive terminal: under a service manager stdout lands in the
/// journal, where a QR block is noise.
pub fn write_frontdoor_hint(
    out: &mut dyn Write,
    profile: &RelayProfile,
    pairing_code: &str,
    render_qr: bool,
    text: impl FnOnce(&str) -> FrontdoorHintText,
) -> io::Result<bool> {
    let Some(link) = frontdoor_link(&profile.relay_url, &profile.node_id, pairing_code) else {
        return Ok(false);
    };
    let lines = text(&link);
    writeln!(out, "{}", lines.header)?;
    writeln!(out, "{}", lines.link_line)?;
    if render_qr && let Some(qr) = render_terminal_qr(&link) {
        writeln!(out)?;
        write!(out, "{qr}")?;
        if !qr.ends_with('\n') {
            writeln!(out)?;
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NODE: &str = "0d3c4f3e8b9a1d2c3b4a5968778695a4";
    const CODE: &str = "Xy7Kq2Lm9Pz4";

    fn profile(url: &str, node: &str) -> RelayProfile {
        RelayProfile {
            relay_url: url.into(),
            node_id: node.into(),
            relay_cert_pin: String::new(),
        }
    }

    fn plain_text(link: &str) -> FrontdoorHintText {
        FrontdoorHintText {
            header: "Or enroll a phone in its browser:".into(),
            link_line: format!("    link : {link}"),
        }
    }

    #[test]
    fn builds_the_frontdoor_link_from_host_and_port() {
        assert_eq!(
            frontdoor_link("relay.example.com:9443", NODE, CODE).as_deref(),
            Some(
                "https://relay.example.com:9443/?node=0d3c4f3e8b9a1d2c3b4a5968778695a4&code=Xy7Kq2Lm9Pz4"
            )
        );
    }

    #[test]
    fn drops_the_default_port_scheme_and_path() {
        let want = format!("https://relay.example.com/?node={NODE}&code={CODE}");
        for addr in [
            "relay.example.com:443",
            "wss://relay.example.com:443/relay",
            "WSS://relay.example.com",
            " https://relay.example.com/ ",
        ] {
            assert_eq!(
                frontdoor_link(addr, NODE, CODE).as_deref(),
                Some(want.as_str()),
                "{addr}"
            );
        }
        assert_eq!(
            frontdoor_link("[2001:db8::1]:443", NODE, CODE).as_deref(),
            Some(format!("https://[2001:db8::1]/?node={NODE}&code={CODE}").as_str())
        );
        assert_eq!(
            frontdoor_link("[2001:db8::1]:9443", NODE, CODE).as_deref(),
            Some(format!("https://[2001:db8::1]:9443/?node={NODE}&code={CODE}").as_str())
        );
    }

    #[test]
    fn percent_encodes_the_node_id_and_code() {
        let link = frontdoor_link("relay:9443", "node&id=1#x", "ab+c/d").expect("link");
        assert_eq!(
            link,
            "https://relay:9443/?node=node%26id%3D1%23x&code=ab%2Bc%2Fd"
        );
    }

    #[test]
    fn refuses_to_build_a_link_from_missing_or_unusable_parts() {
        assert_eq!(frontdoor_link("", NODE, CODE), None);
        assert_eq!(frontdoor_link("relay:9443", "", CODE), None);
        assert_eq!(frontdoor_link("relay:9443", NODE, "  "), None);
        assert_eq!(frontdoor_link("user@relay:9443", NODE, CODE), None);
        assert_eq!(frontdoor_link("re lay:9443", NODE, CODE), None);
        assert_eq!(frontdoor_link("wss:///relay", NODE, CODE), None);
        // A non-TLS or unknown scheme is not guessed at.
        for addr in ["ftp://relay:9443", "file://relay", "FTP://relay"] {
            assert_eq!(frontdoor_link(addr, NODE, CODE), None, "{addr}");
        }
    }

    #[test]
    fn writes_the_link_and_qr_to_the_console_writer() {
        let mut out = Vec::new();
        let wrote = write_frontdoor_hint(
            &mut out,
            &profile("relay:9443", NODE),
            CODE,
            true,
            plain_text,
        )
        .expect("write");
        assert!(wrote);
        let text = String::from_utf8(out).expect("utf8");
        let link = format!("https://relay:9443/?node={NODE}&code={CODE}");
        assert!(text.contains(&format!("    link : {link}")), "{text}");
        let qr = render_terminal_qr(&link).expect("qr");
        assert!(
            text.contains(&qr),
            "the QR of the exact link must follow: {text}"
        );
        assert!(
            qr.contains('█') || qr.contains('▀') || qr.contains('▄'),
            "not a unicode QR"
        );
    }

    #[test]
    fn omits_the_qr_when_not_on_a_terminal() {
        let mut out = Vec::new();
        write_frontdoor_hint(
            &mut out,
            &profile("relay:9443", NODE),
            CODE,
            false,
            plain_text,
        )
        .expect("write");
        let text = String::from_utf8(out).expect("utf8");
        assert_eq!(text.lines().count(), 2, "header and link only: {text:?}");
        assert!(
            !text.contains('▀') && !text.contains('▄'),
            "no QR block: {text:?}"
        );
    }

    #[test]
    fn writes_nothing_without_a_relay() {
        let mut out = Vec::new();
        let wrote =
            write_frontdoor_hint(&mut out, &RelayProfile::default(), CODE, true, plain_text)
                .expect("write");
        assert!(!wrote);
        assert!(out.is_empty());
    }

    /// The link and QR go to the operator's console writer only: producing them
    /// must emit nothing on the structured log stream, and certainly not the code.
    #[test]
    fn the_pairing_code_never_reaches_the_log_stream() {
        ::zeroclaw_log::try_install_capture_subscriber();
        let mut rx = ::zeroclaw_log::subscribe_or_install();
        while rx.try_recv().is_ok() {}

        let mut out = Vec::new();
        for render_qr in [true, false] {
            write_frontdoor_hint(
                &mut out,
                &profile("relay:9443", NODE),
                CODE,
                render_qr,
                plain_text,
            )
            .expect("write");
        }
        // An unusable address takes the early-return path; it must not log either.
        write_frontdoor_hint(
            &mut out,
            &profile("bad host:1", NODE),
            CODE,
            true,
            plain_text,
        )
        .expect("write");

        let mut captured = String::new();
        while let Ok(event) = rx.try_recv() {
            captured.push_str(&event.to_string());
        }
        assert!(
            !captured.contains(CODE),
            "the pairing code reached the log stream: {captured}"
        );
        assert!(
            String::from_utf8_lossy(&out).contains(CODE),
            "sanity: the console writer did receive the link"
        );
    }
}
