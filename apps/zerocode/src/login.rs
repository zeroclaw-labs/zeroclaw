//! URL-scoped OIDC login orchestration. Reuses device enrollment, gateway
//! enrollment and RPC initialize; the local connection config owns credentials.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use clap::Args;
use sha2::{Digest as _, Sha256};

use crate::{Cli, InsecureTlsChoice, client, config, enroll, i18n, oidc_enroll};

#[derive(Args)]
pub(crate) struct LoginArgs {
    /// The remote daemon's OIDC alias (corp or oidc.corp).
    pub provider: String,
    /// Exact WSS URL, or a hostname (defaults to wss://host:9781).
    pub target: String,
    /// Gateway HTTPS origin, including any path prefix. Defaults to
    /// https://<target-host>:42617; use the public HTTPS URL behind a proxy.
    #[arg(long)]
    pub gateway_url: Option<String>,
    /// Private CA for the gateway, which may use different TLS from WSS.
    #[arg(long)]
    pub gateway_ca_cert: Option<String>,
}

pub(crate) fn normalize_target(target: &str) -> Result<String> {
    let target = if target.contains("://") {
        target.to_string()
    } else {
        // Remember an explicit :443 before URL normalization removes it.
        let url = url::Url::parse(&format!("wss://{target}"))
            .context(i18n::t("zc-login-invalid-target"))?;
        let authority = target.split('/').next().unwrap_or(target);
        let explicit_port = authority
            .rsplit_once(':')
            .is_some_and(|(_, tail)| tail.parse::<u16>().is_ok());
        if explicit_port {
            url.to_string()
        } else {
            let mut url = url;
            url.set_port(Some(9781))
                .map_err(|()| anyhow::Error::msg(i18n::t("zc-login-invalid-target")))?;
            url.to_string()
        }
    };
    let url = url::Url::parse(&target).context(i18n::t("zc-login-invalid-target"))?;
    if url.scheme() != "wss"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        bail!(i18n::t("zc-login-invalid-target"));
    }
    Ok(url.to_string())
}

fn provider_alias(provider: &str) -> Result<&str> {
    let alias = provider.strip_prefix("oidc.").unwrap_or(provider);
    if alias.is_empty()
        || alias.len() > 64
        || !alias.as_bytes()[0].is_ascii_alphanumeric()
        || !alias
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        bail!(i18n::t("zc-login-invalid-provider"));
    }
    Ok(alias)
}

pub(crate) fn credential_dir(config_dir: &Path, uri: &str) -> PathBuf {
    config_dir
        .join("connections")
        .join(hex::encode(Sha256::digest(uri.as_bytes())))
}

pub(crate) fn tls_from(section: &config::WssTlsSection) -> client::ClientTls {
    client::ClientTls {
        skip_verify: section.skip_verify,
        ca_cert_path: crate::opt_path(&section.ca_cert_path),
        client_cert_path: crate::opt_path(&section.client_cert_path),
        client_key_path: crate::opt_path(&section.client_key_path),
    }
}

fn absolute(path: &str) -> Result<String> {
    let path = Path::new(path);
    Ok(if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    }
    .to_string_lossy()
    .into_owned())
}

/// Only certificate validation errors offer an explicit insecure fallback.
/// Network failures, timeouts and authentication refusals never weaken TLS.
fn certificate_error(error: &anyhow::Error) -> bool {
    fn is_certificate(error: &(dyn std::error::Error + 'static)) -> bool {
        if matches!(
            error.downcast_ref::<rustls::Error>(),
            Some(rustls::Error::InvalidCertificate(_))
        ) {
            return true;
        }
        // Tokio wraps rustls errors in io::Error. Its source() can skip the
        // wrapped value itself, so inspect get_ref() before following sources.
        if let Some(inner) = error
            .downcast_ref::<std::io::Error>()
            .and_then(std::io::Error::get_ref)
            && is_certificate(inner)
        {
            return true;
        }
        error.source().is_some_and(is_certificate)
    }
    is_certificate(error.as_ref())
}

fn consent(section: &mut config::WssTlsSection, uri: &str) -> Result<()> {
    if section.route_acked(uri) {
        return Ok(());
    }
    match crate::confirm_insecure_tls(uri)? {
        InsecureTlsChoice::Abort => bail!(i18n::t("zc-login-tls-aborted")),
        InsecureTlsChoice::Once => {}
        InsecureTlsChoice::Always => section.skip_verify_routes.push(uri.to_string()),
    }
    Ok(())
}

async fn probe_wss(uri: &str, tls: &client::ClientTls) -> Result<()> {
    let url = url::Url::parse(uri)?;
    let host = url
        .host_str()
        .context("WSS host missing")?
        .trim_matches(['[', ']']);
    let name = rustls::pki_types::ServerName::try_from(host.to_string())?;
    let connector = tokio_rustls::TlsConnector::from(client::RpcClient::wss_tls_config(tls)?);
    tokio::time::timeout(Duration::from_secs(30), async {
        let stream =
            tokio::net::TcpStream::connect((host, url.port_or_known_default().unwrap_or(443)))
                .await?;
        connector.connect(name, stream).await?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .context(i18n::t("zc-login-connect-timeout"))??;
    Ok(())
}

async fn verify_transports(
    uri: &str,
    gateway: &str,
    section: &mut config::WssTlsSection,
    gateway_section: &mut config::WssTlsSection,
) -> Result<()> {
    // Preflight carries no token or device code. Each TLS peer owns its trust:
    // accepting a self-signed WSS certificate must not weaken a public gateway.
    for (address, wss, section) in [(uri, true, section), (gateway, false, gateway_section)] {
        if section.skip_verify && (wss || address.starts_with("https://")) {
            consent(section, address)?;
        }
        let probe = async |tls: &client::ClientTls| {
            if wss {
                probe_wss(address, tls).await
            } else {
                oidc_enroll::GatewayEnrollment::new(address, tls)?
                    .probe()
                    .await
            }
        };
        if let Err(error) = probe(&tls_from(section)).await {
            if section.skip_verify || !certificate_error(&error) {
                return Err(error);
            }
            eprintln!(
                "{}",
                i18n::t_args("zc-login-untrusted", &[("url", address)])
            );
            consent(section, address)?;
            section.skip_verify = true;
            probe(&tls_from(section)).await?;
        }
    }
    Ok(())
}

async fn authenticate(
    uri: &str,
    gateway: &str,
    alias: &str,
    section: &config::WssTlsSection,
    gateway_section: &config::WssTlsSection,
) -> Result<config::OidcLogin> {
    let tls = tls_from(section);
    // verify_transports owns consent for both legs before this flow starts.
    let token = oidc_enroll::run_device_flow(
        gateway,
        &tls_from(gateway_section),
        alias,
        |start| {
            let url = start
                .verification_uri_complete
                .as_deref()
                .unwrap_or(&start.verification_uri);
            eprintln!(
                "{}",
                i18n::t_args(
                    "zc-oidc-enroll-visit",
                    &[("uri", url), ("code", &start.user_code)]
                )
            );
            eprintln!(
                "{}",
                i18n::t_args(
                    "zc-oidc-enroll-waiting",
                    &[("seconds", &start.expires_in.to_string())]
                )
            );
        },
        |_| Ok(()),
    )
    .await?;
    let expires_at = token.expires_in.map(|seconds| {
        chrono::Utc::now()
            .timestamp()
            .saturating_add(i64::try_from(seconds).unwrap_or(i64::MAX))
    });
    let provider = format!("oidc.{alias}");
    let rpc = tokio::time::timeout(
        Duration::from_secs(30),
        client::RpcClient::connect_wss_direct(
            uri,
            None,
            None,
            &tls,
            Some(&token.access_token),
            Some(&provider),
        ),
    )
    .await
    .context(i18n::t("zc-login-connect-timeout"))?
    .context(i18n::t("zc-login-rejected"))?;
    // End the validation session; login is a command, not a TUI launch.
    rpc.shutdown();
    Ok(config::OidcLogin {
        auth_provider: provider,
        auth_token: token.access_token,
        enroll_url: gateway.to_string(),
        expires_at,
        tls: section.clone(),
        gateway_tls: gateway_section.clone(),
    })
}

pub(crate) async fn run(args: &LoginArgs, cli: &Cli, config_dir: &Path) -> Result<()> {
    if cli.connect.is_some() || cli.relay.is_some() {
        bail!(i18n::t("zc-login-conflicting-route"));
    }
    let alias = provider_alias(&args.provider)?;
    let uri = normalize_target(&args.target)?;
    let url = url::Url::parse(&uri)?;
    let persisted = config::load_persisted(config_dir)?;
    let default_gateway = {
        let mut gateway = url.clone();
        gateway
            .set_scheme("https")
            .map_err(|()| anyhow::Error::msg(i18n::t("zc-login-invalid-target")))?;
        gateway
            .set_port(Some(42617))
            .map_err(|()| anyhow::Error::msg(i18n::t("zc-login-invalid-target")))?;
        gateway.set_path("");
        gateway.to_string()
    };
    let gateway = args
        .gateway_url
        .clone()
        .or_else(|| {
            persisted
                .connection
                .logins
                .get(&uri)
                .map(|login| login.enroll_url.clone())
        })
        .unwrap_or(default_gateway);
    let gateway = oidc_enroll::validate_enroll_url(&gateway)?;
    // Do not turn transient env-injected paths or trust into persistent config.
    let mut section = persisted
        .connection
        .logins
        .get(&uri)
        .map(|login| login.tls.clone())
        .unwrap_or_default();
    let mut gateway_section = persisted
        .connection
        .logins
        .get(&uri)
        .filter(|login| login.enroll_url == gateway)
        .map(|login| login.gateway_tls.clone())
        .unwrap_or_default();
    gateway_section.skip_verify |= cli.tls_skip_verify;
    if let Some(path) = &args.gateway_ca_cert {
        gateway_section.ca_cert_path = absolute(path)?;
        gateway_section.skip_verify = cli.tls_skip_verify;
    }
    let credentials = credential_dir(config_dir, &uri);
    enroll::recover_and_validate(&credentials)?;
    section.skip_verify |= cli.tls_skip_verify;
    if let Some(path) = &cli.tls_ca_cert {
        section.ca_cert_path = absolute(path)?;
        section.skip_verify = cli.tls_skip_verify;
    }
    if let Some(path) = &cli.tls_client_cert {
        section.client_cert_path = absolute(path)?;
    }
    if let Some(path) = &cli.tls_client_key {
        section.client_key_path = absolute(path)?;
    }
    let certless =
        section.client_cert_path.is_empty() && !credentials.join("tls/client.crt").exists();
    if cli.enroll || certless {
        let host = cli
            .enroll_host
            .as_deref()
            .unwrap_or(url.host_str().context("WSS host missing")?)
            .trim_matches(['[', ']']);
        enroll::enroll(
            host,
            cli.enroll_port.unwrap_or(enroll::DEFAULT_ENROLL_PORT),
            &credentials,
        )
        .await?;
        section.client_cert_path.clear();
        section.client_key_path.clear();
        if cli.tls_ca_cert.is_none() {
            section.ca_cert_path.clear();
        }
    }
    for (value, name) in [
        (&mut section.ca_cert_path, "ca.crt"),
        (&mut section.client_cert_path, "client.crt"),
        (&mut section.client_key_path, "client.key"),
    ] {
        if value.is_empty() && credentials.join("tls").join(name).exists() {
            *value = absolute(&credentials.join("tls").join(name).to_string_lossy())?;
        }
    }
    verify_transports(&uri, &gateway, &mut section, &mut gateway_section).await?;
    let login = authenticate(&uri, &gateway, alias, &section, &gateway_section).await?;
    config::persist_oidc_login(config_dir, &uri, &login, true)?;
    eprintln!(
        "{}",
        i18n::t_args(
            "zc-login-saved",
            &[
                ("url", &uri),
                (
                    "path",
                    &config::config_path(config_dir).display().to_string()
                )
            ]
        )
    );
    if std::env::var_os("ZEROCLAW_AUTH_TOKEN").is_some() {
        eprintln!("{}", i18n::t("zc-login-env-shadow"));
    }
    Ok(())
}

/// Persisted token expiry comes from the gateway, not unverified JWT claims.
/// Re-enroll before entering raw terminal mode, without asking users to copy a
/// token. A structured authentication refusal can force one fresh sign-in.
pub(crate) async fn renew_expired(
    config_dir: &Path,
    loaded: &mut config::ZerocodeConfig,
    force: bool,
) -> Result<()> {
    let Some(uri) = loaded.connection.wss.uri.clone() else {
        return Ok(());
    };
    let persisted = config::load_persisted(config_dir)?;
    let Some(login) = persisted.connection.logins.get(&uri) else {
        return Ok(());
    };
    if (!force
        && login
            .expires_at
            .is_none_or(|expiry| expiry > chrono::Utc::now().timestamp().saturating_add(30)))
        || std::env::var("ZEROCLAW_AUTH_TOKEN").is_ok_and(|token| !token.is_empty())
    {
        return Ok(());
    }
    let alias = provider_alias(&login.auth_provider)?;
    enroll::recover_and_validate(&credential_dir(config_dir, &uri))?;
    let mut section = login.tls.clone();
    let mut gateway_section = login.gateway_tls.clone();
    eprintln!("{}", i18n::t("zc-login-expired"));
    verify_transports(&uri, &login.enroll_url, &mut section, &mut gateway_section).await?;
    let renewed = authenticate(&uri, &login.enroll_url, alias, &section, &gateway_section).await?;
    config::persist_oidc_login(config_dir, &uri, &renewed, false)?;
    loaded.connection.logins.insert(uri.clone(), renewed);
    loaded.connection.wss = loaded.connection.wss_for(Some(&uri));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser as _;

    #[test]
    fn target_is_canonical_and_preserves_security_boundaries() {
        for (input, expected) in [
            ("HOST.example", "wss://host.example:9781/"),
            ("host.example:443", "wss://host.example/"),
            ("wss://host.example", "wss://host.example/"),
            ("wss://host.example:9781/rpc", "wss://host.example:9781/rpc"),
            ("[::1]", "wss://[::1]:9781/"),
            ("wss://[::1]:8443/path", "wss://[::1]:8443/path"),
        ] {
            assert_eq!(normalize_target(input).unwrap(), expected);
        }
        for bad in [
            "https://host",
            "ws://host",
            "wss://user:pass@host",
            "wss://host/?token=secret",
            "wss://host/#secret",
        ] {
            assert!(normalize_target(bad).is_err(), "accepted {bad}");
        }
        assert_ne!(
            credential_dir(Path::new("config"), "wss://host/a"),
            credential_dir(Path::new("config"), "wss://host/b")
        );
        assert_ne!(
            normalize_target("host:9781").unwrap(),
            normalize_target("host:9782").unwrap()
        );
    }

    #[test]
    fn aliases_cannot_escape_gateway_route() {
        assert_eq!(provider_alias("oidc.corp").unwrap(), "corp");
        assert_eq!(provider_alias("corp-2_test").unwrap(), "corp-2_test");
        for bad in [
            "",
            "oidc.",
            "../corp",
            "corp/other",
            "corp?x",
            "corp#x",
            "corp%2fadmin",
            "_corp",
        ] {
            assert!(provider_alias(bad).is_err(), "accepted {bad}");
        }
    }

    #[test]
    fn cli_login_accepts_global_flags_on_either_side() {
        for args in [
            vec![
                "zerocode",
                "--config-dir",
                "config",
                "login",
                "corp",
                "wss://host:9781",
                "--gateway-url",
                "https://gateway",
                "--tls-ca-cert",
                "ca.pem",
            ],
            vec![
                "zerocode",
                "login",
                "oidc.corp",
                "host",
                "--config-dir",
                "config",
                "--enroll",
                "--enroll-port",
                "8444",
            ],
        ] {
            let cli = Cli::try_parse_from(args).unwrap();
            assert!(matches!(cli.command, Some(crate::Command::Login(_))));
            assert_eq!(cli.config_dir, Some(PathBuf::from("config")));
        }
        assert!(Cli::try_parse_from(["zerocode", "--connect", "wss://host:9781"]).is_ok());
        assert!(Cli::try_parse_from(["zerocode", "login", "corp"]).is_err());
    }

    #[test]
    fn only_certificate_failures_can_offer_skip_verification() {
        let error = anyhow::Error::new(rustls::Error::InvalidCertificate(
            rustls::CertificateError::UnknownIssuer,
        ))
        .context("probe");
        assert!(certificate_error(&error));
        assert!(!certificate_error(&anyhow::Error::msg(
            "UnknownIssuer: connection refused"
        )));
        assert!(!certificate_error(
            &std::io::Error::from(std::io::ErrorKind::ConnectionRefused).into()
        ));
    }
    #[tokio::test]
    async fn real_tls_certificate_failure_is_classified_on_both_transports() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (_, ca, key) = crate::client_crypto::test_pki::gen_ca();
        let (cert, key) =
            crate::client_crypto::test_pki::gen_server_cert(&ca, &key, &["127.0.0.1".into()]);
        for wss in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let acceptor = crate::client_crypto::test_pki::tls_acceptor(&cert, &key);
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.unwrap();
                let _ = acceptor.accept(stream).await;
            });
            let tls = client::ClientTls::default();
            let error = if wss {
                probe_wss(&format!("wss://{addr}/"), &tls)
                    .await
                    .unwrap_err()
            } else {
                oidc_enroll::GatewayEnrollment::new(&format!("https://{addr}"), &tls)
                    .unwrap()
                    .probe()
                    .await
                    .unwrap_err()
            };
            assert!(
                certificate_error(&error),
                "not classified ({wss}): {error:#}"
            );
            server.await.unwrap();
        }
    }
}
