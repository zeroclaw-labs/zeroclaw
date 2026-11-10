//! First-contact URL-scoped OIDC login. Gateway sign-in authorizes automatic
//! device certificate bootstrap; no pairing flow is involved. The local
//! connection config owns credentials, and WSS still requires mTLS + a bearer.

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
    config::canonical_wss_target(target).context(i18n::t("zc-login-invalid-target"))
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

/// The saved certificate path selects its enrollment generation. Legacy URL
/// caches remain readable; an external certificate never redirects cache writes.
pub(crate) fn active_credential_dir(
    config_dir: &Path,
    uri: &str,
    section: &config::WssTlsSection,
) -> Result<PathBuf> {
    let root = std::path::absolute(credential_dir(config_dir, uri))?;
    let cert = Path::new(&section.client_cert_path);
    if let Some(generation) = cert.parent().and_then(Path::parent)
        && generation.parent() == Some(root.join("generations").as_path())
        && cert == generation.join("tls/client.crt")
    {
        return Ok(generation.to_path_buf());
    }
    Ok(root)
}

/// X.509 is the authority for certificate lifetime, not profile.json or token
/// expiry. A missing, malformed, expired or not-yet-valid identity is replaced
/// before a WSS attempt; the new handshake still verifies key/CA/signature.
fn certificate_usable(section: &config::WssTlsSection) -> bool {
    let Some(cert) = std::fs::read(&section.client_cert_path)
        .ok()
        .and_then(|pem| rustls_pemfile::certs(&mut pem.as_slice()).next())
        .and_then(Result::ok)
    else {
        return false;
    };
    let Ok((rest, parsed)) = x509_parser::parse_x509_certificate(cert.as_ref()) else {
        return false;
    };
    let now = chrono::Utc::now().timestamp();
    rest.is_empty()
        && parsed.validity().not_before.timestamp() <= now
        && parsed.validity().not_after.timestamp() > now.saturating_add(30)
        && std::fs::read(&section.client_key_path)
            .ok()
            .and_then(|key| {
                rustls_pemfile::private_key(&mut key.as_slice())
                    .ok()
                    .flatten()
            })
            .is_some()
}

/// Decide whether login must issue a separate generation.
///
/// `Ok(true)` means the saved certificate will not be reused. The previous
/// directory is left unchanged, including a published manifest that no longer
/// matches a deleted or damaged `client.crt`. `Ok(false)` means that generation
/// was validated and may be reused. A mixed set is still refused on that path.
fn prepare_generation_for_login(
    credentials: &Path,
    section: &config::WssTlsSection,
    force_enroll: bool,
) -> Result<bool> {
    if force_enroll || !certificate_usable(section) {
        return Ok(true);
    }
    enroll::recover_and_validate(credentials)?;
    Ok(false)
}

/// Own only the candidate generation until authentication succeeds. The config's
/// atomic path swap selects a generation; no login attempt overwrites one that
/// another process or the previous saved login may still be using.
struct AcceptedLogin {
    login: config::OidcLogin,
    generation: Option<tempfile::TempDir>,
}

impl AcceptedLogin {
    fn persist(self, config_dir: &Path, uri: &str, select: bool) -> Result<config::OidcLogin> {
        if let Some(generation) = self.generation {
            // Keep before writing config: a durability/permissions error can be
            // returned after its atomic rename has already published new paths.
            // An orphan candidate is safer than deleting a referenced key.
            let path = generation.keep();
            if let Some(parent) = path.parent() {
                crate::secure_file::sync_dir_where_supported(parent)?;
            }
        }
        config::persist_oidc_login(config_dir, uri, &self.login, select)?;
        Ok(self.login)
    }
}

fn new_generation(config_dir: &Path, uri: &str) -> Result<tempfile::TempDir> {
    let root = credential_dir(config_dir, uri);
    let generations = root.join("generations");
    std::fs::create_dir_all(&generations).context(i18n::t("zc-login-stage-credentials"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        for dir in [&root, &generations] {
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
                .context(i18n::t("zc-login-stage-credentials"))?;
        }
    }
    for dir in [
        Some(config_dir),
        root.parent(),
        Some(root.as_path()),
        Some(generations.as_path()),
    ]
    .into_iter()
    .flatten()
    {
        crate::secure_file::sync_dir_where_supported(dir)?;
    }
    tempfile::Builder::new()
        .prefix("login-")
        .tempdir_in(generations)
        .context(i18n::t("zc-login-stage-credentials"))
}

/// Apply this invocation's TLS flags without making them config authority.
fn apply_tls_overrides(section: &mut config::WssTlsSection, cli: &Cli) -> Result<()> {
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
    Ok(())
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

async fn verify_peer(address: &str, wss: bool, section: &mut config::WssTlsSection) -> Result<()> {
    // Preflight carries no token or device code. Each TLS peer owns its trust:
    // accepting a self-signed WSS certificate must not weaken a public gateway.
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
    Ok(())
}

async fn authenticate(
    uri: &str,
    gateway: &str,
    alias: &str,
    section: &mut config::WssTlsSection,
    gateway_section: &mut config::WssTlsSection,
    config_dir: &Path,
    force_enroll: bool,
) -> Result<AcceptedLogin> {
    // First contact starts with gateway TLS, not the client-authenticated WSS
    // plane and not the pairing-only enrollment listener.
    verify_peer(gateway, false, gateway_section).await?;
    // Gateway consent was obtained before device code or bearer exchange.
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
    let credentials = active_credential_dir(config_dir, uri, section)?;
    // Replacement stages a new generation and must not validate or rewrite the
    // previous one. A missing or damaged certificate fails the published
    // manifest, and that refusal would otherwise block the replacement.
    let replacing = prepare_generation_for_login(&credentials, section, force_enroll)?;
    let mut generation = None;
    if replacing {
        let replace_managed_ca = section.ca_cert_path.is_empty()
            || Path::new(&section.ca_cert_path) == credentials.join("tls/ca.crt");
        let staged = new_generation(config_dir, uri)?;
        let gateway_client =
            oidc_enroll::GatewayEnrollment::new(gateway, &tls_from(gateway_section))?;
        enroll::enroll_with_oidc(&gateway_client, alias, &token.access_token, staged.path())
            .await?;
        for (path, name) in [
            (&mut section.client_cert_path, "client.crt"),
            (&mut section.client_key_path, "client.key"),
        ] {
            *path = absolute(&staged.path().join("tls").join(name).to_string_lossy())?;
        }
        if replace_managed_ca {
            section.ca_cert_path = absolute(&staged.path().join("tls/ca.crt").to_string_lossy())?;
        }
        generation = Some(staged);
    }
    verify_peer(uri, true, section).await?;
    let tls = tls_from(section);
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
    Ok(AcceptedLogin {
        login: config::OidcLogin {
            auth_provider: provider,
            auth_token: token.access_token,
            enroll_url: gateway.to_string(),
            expires_at,
            tls: section.clone(),
            gateway_tls: gateway_section.clone(),
        },
        generation,
    })
}

pub(crate) async fn run(args: &LoginArgs, cli: &Cli, config_dir: &Path) -> Result<()> {
    if cli.connect.is_some() || cli.relay.is_some() {
        bail!(i18n::t("zc-login-conflicting-route"));
    }
    if cli.enroll_host.is_some() || cli.enroll_port.is_some() {
        bail!(i18n::t("zc-login-no-pairing-endpoint"));
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
    let credentials = active_credential_dir(config_dir, &uri, &section)?;
    apply_tls_overrides(&mut section, cli)?;
    for (value, name) in [
        (&mut section.ca_cert_path, "ca.crt"),
        (&mut section.client_cert_path, "client.crt"),
        (&mut section.client_key_path, "client.key"),
    ] {
        if value.is_empty() && credentials.join("tls").join(name).exists() {
            *value = absolute(&credentials.join("tls").join(name).to_string_lossy())?;
        }
    }
    let login = authenticate(
        &uri,
        &gateway,
        alias,
        &mut section,
        &mut gateway_section,
        config_dir,
        cli.enroll,
    )
    .await?;
    login.persist(config_dir, &uri, true)?;
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
    cli: &Cli,
) -> Result<()> {
    let Some(uri) = loaded.connection.wss.uri.clone() else {
        return Ok(());
    };
    let persisted = config::load_persisted(config_dir)?;
    let Some(login) = persisted.connection.logins.get(&uri) else {
        return Ok(());
    };
    if std::env::var("ZEROCLAW_AUTH_TOKEN").is_ok_and(|token| !token.is_empty()) {
        return Ok(());
    }
    let mut section = login.tls.clone();
    apply_tls_overrides(&mut section, cli)?;
    let credentials = active_credential_dir(config_dir, &uri, &login.tls)?;
    if !force
        && login
            .expires_at
            .is_none_or(|expiry| expiry > chrono::Utc::now().timestamp().saturating_add(30))
        && !prepare_generation_for_login(&credentials, &section, false)?
    {
        loaded.connection.wss.tls = section;
        return Ok(());
    }
    let alias = provider_alias(&login.auth_provider)?;
    let mut gateway_section = login.gateway_tls.clone();
    eprintln!("{}", i18n::t("zc-login-expired"));
    let mut renewed = authenticate(
        &uri,
        &login.enroll_url,
        alias,
        &mut section,
        &mut gateway_section,
        config_dir,
        false,
    )
    .await?;
    let effective_tls = renewed.login.tls.clone();
    // Validate and use this run's overrides, but renew only persistent facts:
    // the token and any newly bootstrapped generation. Ordinary startup must
    // never turn a transient CA, key path or skip flag into saved trust.
    if cli.tls_skip_verify || cli.tls_ca_cert.is_some() {
        renewed.login.tls.skip_verify = login.tls.skip_verify;
    }
    if cli.tls_ca_cert.is_some() {
        let old_credentials = active_credential_dir(config_dir, &uri, &login.tls)?;
        renewed.login.tls.ca_cert_path = if let Some(generation) = renewed.generation.as_ref()
            && (login.tls.ca_cert_path.is_empty()
                || Path::new(&login.tls.ca_cert_path) == old_credentials.join("tls/ca.crt"))
        {
            absolute(&generation.path().join("tls/ca.crt").to_string_lossy())?
        } else {
            login.tls.ca_cert_path.clone()
        };
    }
    if renewed.generation.is_none() {
        if cli.tls_client_cert.is_some() {
            renewed.login.tls.client_cert_path = login.tls.client_cert_path.clone();
        }
        if cli.tls_client_key.is_some() {
            renewed.login.tls.client_key_path = login.tls.client_key_path.clone();
        }
    }
    let renewed = renewed.persist(config_dir, &uri, false)?;
    loaded.connection.logins.insert(uri.clone(), renewed);
    loaded.connection.wss = loaded.connection.wss_for(Some(&uri));
    loaded.connection.wss.tls = effective_tls;
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
            // Deliberately rejected input, never an outbound connection.
            "ws://host", // nosemgrep: javascript.lang.security.detect-insecure-websocket.detect-insecure-websocket
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
    #[test]
    fn active_generation_is_selected_from_the_saved_certificate_path() {
        let dir = tempfile::tempdir().unwrap();
        let uri = "wss://daemon.example/rpc";
        let root = credential_dir(dir.path(), uri);
        let generation = root.join("generations/login-candidate");
        let mut tls = config::WssTlsSection {
            client_cert_path: generation.join("tls/client.crt").display().to_string(),
            ..Default::default()
        };
        assert_eq!(
            active_credential_dir(dir.path(), uri, &tls).unwrap(),
            generation
        );
        tls.client_cert_path = dir.path().join("external/client.crt").display().to_string();
        assert_eq!(active_credential_dir(dir.path(), uri, &tls).unwrap(), root);
        tls.client_cert_path = root.join("tls/client.crt").display().to_string();
        assert_eq!(active_credential_dir(dir.path(), uri, &tls).unwrap(), root);
    }

    #[test]
    fn certificate_reuse_requires_valid_x509_and_a_readable_key() {
        let dir = tempfile::tempdir().unwrap();
        let cert = dir.path().join("client.crt");
        let key = dir.path().join("client.key");
        let section = config::WssTlsSection {
            client_cert_path: cert.display().to_string(),
            client_key_path: key.display().to_string(),
            ..Default::default()
        };
        assert!(!certificate_usable(&section));
        let pair = rcgen::KeyPair::generate().unwrap();
        std::fs::write(&key, pair.serialize_pem()).unwrap();
        for (before, after, expected) in [
            ((2000, 1, 1), (2001, 1, 1), false),
            ((2998, 1, 1), (2999, 1, 1), false),
            ((2000, 1, 1), (2999, 1, 1), true),
        ] {
            let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
            params.not_before = rcgen::date_time_ymd(before.0, before.1, before.2);
            params.not_after = rcgen::date_time_ymd(after.0, after.1, after.2);
            std::fs::write(&cert, params.self_signed(&pair).unwrap().pem()).unwrap();
            assert_eq!(certificate_usable(&section), expected);
        }
        std::fs::write(&key, "invalid private key").unwrap();
        assert!(!certificate_usable(&section));
        std::fs::write(&cert, "invalid certificate").unwrap();
        assert!(!certificate_usable(&section));
    }

    #[test]
    fn a_damaged_certificate_can_be_replaced_without_changing_the_old_generation() {
        let dir = tempfile::tempdir().unwrap();
        let uri = "wss://daemon.example/rpc";
        let generation = credential_dir(dir.path(), uri).join("generations/login-old");
        let tls = generation.join("tls");
        std::fs::create_dir_all(&tls).unwrap();
        let digest = "ab".repeat(32);
        std::fs::write(
            tls.join("published.manifest"),
            format!(
                "zerocode-published-v1\n{digest} client.crt\n{digest} ca.crt\n{digest} client.key\n{digest} profile.json\n"
            ),
        )
        .unwrap();
        for (name, bytes) in [
            ("ca.crt", &b"ca"[..]),
            ("client.key", &b"key"[..]),
            ("profile.json", &b"{}"[..]),
        ] {
            std::fs::write(tls.join(name), bytes).unwrap();
        }
        let cert = tls.join("client.crt");
        std::fs::write(&cert, b"damaged-cert").unwrap();
        let before = std::fs::read(&cert).unwrap();
        let manifest = std::fs::read(tls.join("published.manifest")).unwrap();
        let section = config::WssTlsSection {
            client_cert_path: cert.display().to_string(),
            client_key_path: tls.join("client.key").display().to_string(),
            ..Default::default()
        };

        let refused = enroll::recover_and_validate(&generation).unwrap_err();
        assert!(
            format!("{refused:#}").contains("published credential set is inconsistent"),
            "reuse must still refuse the damaged generation: {refused:#}"
        );
        assert!(
            prepare_generation_for_login(&generation, &section, false).unwrap(),
            "login must be allowed to stage a replacement"
        );
        assert!(
            prepare_generation_for_login(&generation, &section, true).unwrap(),
            "explicit enrollment must not be blocked by the old manifest"
        );
        assert_eq!(std::fs::read(&cert).unwrap(), before);
        assert_eq!(
            std::fs::read(tls.join("published.manifest")).unwrap(),
            manifest
        );
        assert_eq!(std::fs::read(tls.join("client.key")).unwrap(), b"key");
    }

    #[test]
    fn a_mixed_usable_certificate_is_still_refused_for_reuse() {
        let dir = tempfile::tempdir().unwrap();
        let tls = dir.path().join("tls");
        std::fs::create_dir_all(&tls).unwrap();
        let cert = tls.join("client.crt");
        let key = tls.join("client.key");
        let pair = rcgen::KeyPair::generate().unwrap();
        std::fs::write(&key, pair.serialize_pem()).unwrap();
        let mut params = rcgen::CertificateParams::new(Vec::<String>::new()).unwrap();
        params.not_before = rcgen::date_time_ymd(2000, 1, 1);
        params.not_after = rcgen::date_time_ymd(2999, 1, 1);
        std::fs::write(&cert, params.self_signed(&pair).unwrap().pem()).unwrap();
        let digest = "cd".repeat(32);
        std::fs::write(
            tls.join("published.manifest"),
            format!(
                "zerocode-published-v1\n{digest} client.crt\n{digest} ca.crt\n{digest} client.key\n{digest} profile.json\n"
            ),
        )
        .unwrap();
        std::fs::write(tls.join("ca.crt"), b"ca").unwrap();
        std::fs::write(tls.join("profile.json"), b"{}").unwrap();
        let before = std::fs::read(&cert).unwrap();
        let section = config::WssTlsSection {
            client_cert_path: cert.display().to_string(),
            client_key_path: key.display().to_string(),
            ..Default::default()
        };

        assert!(certificate_usable(&section));
        assert!(prepare_generation_for_login(dir.path(), &section, false).is_err());
        assert!(prepare_generation_for_login(dir.path(), &section, true).unwrap());
        assert_eq!(std::fs::read(&cert).unwrap(), before);
    }

    #[test]
    fn failed_config_save_preserves_prior_files_and_the_accepted_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let uri = "wss://daemon.example/rpc";
        let generation = new_generation(dir.path(), uri).unwrap();
        let candidate = generation.path().join("client.key");
        crate::secure_file::write_durable(&candidate, b"synthetic-new-key", true).unwrap();
        let old_key = credential_dir(dir.path(), uri).join("old.key");
        crate::secure_file::write_durable(&old_key, b"synthetic-old-key", true).unwrap();
        let path = config::config_path(dir.path());
        std::fs::write(&path, "[connection]\nlogins = 'malformed'\n").unwrap();
        let before = std::fs::read(&path).unwrap();
        let accepted = AcceptedLogin {
            login: config::OidcLogin {
                auth_provider: "oidc.corp".into(),
                auth_token: "synthetic-token".into(),
                enroll_url: "https://gateway.example".into(),
                expires_at: None,
                tls: config::WssTlsSection::default(),
                gateway_tls: config::WssTlsSection::default(),
            },
            generation: Some(generation),
        };
        assert!(accepted.persist(dir.path(), uri, true).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(old_key).unwrap(), b"synthetic-old-key");
        assert_eq!(std::fs::read(candidate).unwrap(), b"synthetic-new-key");
    }
}
