use anyhow::{Context, Result};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use tokio::io::{self, AsyncBufReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        Message,
        client::IntoClientRequest,
        http::{HeaderValue, header},
    },
};
use zeroclaw_config::schema::resolve_runtime_dirs_for_onboarding;

const CONFIG_NOT_FOUND_ERROR: &str = "ERROR: config.toml not found.  Are you sure the bridge and ZeroClaw are running on the same host?  Tool use will not work remotely!";
const PAIRING_TOKEN_NOT_FOUND_ERROR: &str = "ERROR: Gateway pairing is active but no ACP bridge token is cached. Run `zeroclaw gateway get-paircode --new`, then run `zeroclaw-acp-bridge --pair-code <code>`, or set ZEROCLAW_ACP_BRIDGE_TOKEN.";
const ACP_BRIDGE_TOKEN_ENV: &str = "ZEROCLAW_ACP_BRIDGE_TOKEN";
const ACP_BRIDGE_PAIRING_CODE_ENV: &str = "ZEROCLAW_ACP_PAIRING_CODE";

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("{error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let bridge_target = load_acp_bridge_target().await?;
    let mut request = bridge_target
        .url
        .as_str()
        .into_client_request()
        .with_context(|| format!("failed to build request for {}", bridge_target.url))?;
    if let Some(token) = bridge_target.token {
        let auth = HeaderValue::from_str(&format!("Bearer {token}"))
            .context("gateway paired token contains invalid header characters")?;
        request.headers_mut().insert(header::AUTHORIZATION, auth);
    }

    let (ws_stream, _) = connect_async(request)
        .await
        .with_context(|| format!("failed to connect to {}", bridge_target.url))?;
    let (mut ws_write, mut ws_read) = ws_stream.split();

    let stdin_to_ws = tokio::spawn(async move {
        let stdin = io::stdin();
        let mut lines = BufReader::new(stdin).lines();

        while let Some(line) = lines.next_line().await.context("failed to read stdin")? {
            ws_write
                .send(Message::Text(line.into()))
                .await
                .context("failed to write websocket message")?;
        }

        ws_write
            .send(Message::Close(None))
            .await
            .context("failed to close websocket")
    });

    let ws_to_stdout = tokio::spawn(async move {
        let mut stdout = io::stdout();

        while let Some(message) = ws_read.next().await {
            match message.context("failed to read websocket message")? {
                Message::Text(text) => write_frame(&mut stdout, text.as_bytes()).await?,
                Message::Binary(bytes) => write_frame(&mut stdout, &bytes).await?,
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) | Message::Frame(_) => {}
            }
        }

        stdout.flush().await.context("failed to flush stdout")
    });

    tokio::select! {
        result = stdin_to_ws => result.context("stdin bridge task panicked")??,
        result = ws_to_stdout => result.context("websocket bridge task panicked")??,
    }

    Ok(())
}

async fn load_acp_bridge_target() -> Result<BridgeTarget> {
    let (config_dir, _) = resolve_runtime_dirs_for_onboarding().await?;
    let config_path = config_dir.join("config.toml");
    if !config_path.exists() {
        anyhow::bail!(CONFIG_NOT_FOUND_ERROR);
    }

    let contents = tokio::fs::read_to_string(&config_path)
        .await
        .with_context(|| format!("failed to read {}", config_path.display()))?;
    let config: BridgeConfig = toml::from_str(&contents)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;

    let pair_code = pair_code_from_args(std::env::args().skip(1))?
        .or_else(|| env_value(ACP_BRIDGE_PAIRING_CODE_ENV));
    resolve_acp_bridge_target(&config.gateway, &config_dir, pair_code.as_deref()).await
}

#[cfg(test)]
fn acp_bridge_target(config: &BridgeGatewayConfig) -> Result<BridgeTarget> {
    if config.require_pairing {
        anyhow::bail!(PAIRING_TOKEN_NOT_FOUND_ERROR);
    }

    Ok(bridge_target(config, None))
}

async fn resolve_acp_bridge_target(
    config: &BridgeGatewayConfig,
    config_dir: &Path,
    pair_code: Option<&str>,
) -> Result<BridgeTarget> {
    if !config.require_pairing {
        return Ok(bridge_target(config, None));
    }

    if let Some(token) = token_from_env() {
        return Ok(bridge_target(config, Some(token)));
    }

    let cache_path = cached_token_path(config_dir);
    if let Some(token) = read_cached_token(&cache_path).await? {
        return Ok(bridge_target(config, Some(token)));
    }

    let pair_code = if let Some(code) = pair_code.map(str::trim).filter(|code| !code.is_empty()) {
        Some(code.to_string())
    } else {
        fetch_pairing_code(&pairing_code_url(config)).await?
    };

    let Some(pair_code) = pair_code else {
        anyhow::bail!(PAIRING_TOKEN_NOT_FOUND_ERROR);
    };

    let token = exchange_pairing_code(&pair_url(config), &pair_code).await?;
    write_cached_token(&cache_path, &token).await?;

    Ok(bridge_target(config, Some(token)))
}

fn bridge_target(config: &BridgeGatewayConfig, token: Option<String>) -> BridgeTarget {
    BridgeTarget {
        url: acp_websocket_url(
            config.gateway_scheme(),
            config.host.trim(),
            config.port,
            config.path_prefix.as_deref(),
        ),
        token,
    }
}

fn token_from_env() -> Option<String> {
    env_value(ACP_BRIDGE_TOKEN_ENV)
}

fn env_value(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn pair_code_from_args(args: impl IntoIterator<Item = String>) -> Result<Option<String>> {
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--pair-code" {
            let code = args
                .next()
                .ok_or_else(|| anyhow::anyhow!("--pair-code requires a code value"))?;
            return Ok(Some(code));
        }
        if let Some(code) = arg.strip_prefix("--pair-code=") {
            return Ok(Some(code.to_string()));
        }
    }
    Ok(None)
}

fn cached_token_path(config_dir: &Path) -> PathBuf {
    config_dir.join("acp-bridge-token")
}

async fn read_cached_token(path: &Path) -> Result<Option<String>> {
    match tokio::fs::read_to_string(path).await {
        Ok(contents) => Ok(Some(contents.trim().to_string()).filter(|token| !token.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e).with_context(|| format!("failed to read {}", path.display())),
    }
}

async fn write_cached_token(path: &Path, token: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }

    write_private_file(path, format!("{token}\n").as_bytes()).await
}

#[cfg(unix)]
async fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    use tokio::io::AsyncWriteExt;

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(contents)
        .await
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

#[cfg(not(unix))]
async fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    tokio::fs::write(path, contents)
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

fn pairing_code_url(config: &BridgeGatewayConfig) -> String {
    gateway_http_url(config, "/pair/code")
}

fn pair_url(config: &BridgeGatewayConfig) -> String {
    gateway_http_url(config, "/pair")
}

fn gateway_http_url(config: &BridgeGatewayConfig, path: &str) -> String {
    let scheme = if config.tls.as_ref().is_some_and(|tls| tls.enabled) {
        "https"
    } else {
        "http"
    };
    http_gateway_url(
        scheme,
        config.host.trim(),
        config.port,
        config.path_prefix.as_deref(),
        path,
    )
}

fn http_gateway_url(
    scheme: &str,
    host: &str,
    port: u16,
    path_prefix: Option<&str>,
    path: &str,
) -> String {
    let host = bracket_host(host);
    let path_prefix = path_prefix.unwrap_or_default().trim_end_matches('/');
    format!("{scheme}://{host}:{port}{path_prefix}{path}")
}

async fn fetch_pairing_code(url: &str) -> Result<Option<String>> {
    #[derive(Deserialize)]
    struct PairCodeResponse {
        pairing_code: Option<String>,
    }

    let response = reqwest::get(url)
        .await
        .with_context(|| format!("failed to fetch pairing code from {url}"))?;
    if !response.status().is_success() {
        return Ok(None);
    }
    let body = response
        .json::<PairCodeResponse>()
        .await
        .with_context(|| format!("failed to parse pairing code response from {url}"))?;
    Ok(body
        .pairing_code
        .map(|code| code.trim().to_string())
        .filter(|code| !code.is_empty()))
}

async fn exchange_pairing_code(url: &str, code: &str) -> Result<String> {
    #[derive(Deserialize)]
    struct PairResponse {
        token: String,
    }

    let client = reqwest::Client::new();
    let response = client
        .post(url)
        .header("X-Pairing-Code", code)
        .send()
        .await
        .with_context(|| format!("failed to pair ACP bridge at {url}"))?;
    if !response.status().is_success() {
        anyhow::bail!(
            "failed to pair ACP bridge at {url}: gateway returned {}",
            response.status()
        );
    }
    let body = response
        .json::<PairResponse>()
        .await
        .with_context(|| format!("failed to parse pairing response from {url}"))?;
    let token = body.token.trim().to_string();
    if token.is_empty() {
        anyhow::bail!("gateway pairing response did not include a bearer token");
    }
    Ok(token)
}

fn acp_websocket_url(scheme: &str, host: &str, port: u16, path_prefix: Option<&str>) -> String {
    let host = bracket_host(host);
    let path_prefix = path_prefix.unwrap_or_default().trim_end_matches('/');
    format!("{scheme}://{host}:{port}{path_prefix}/acp")
}

fn bracket_host(host: &str) -> String {
    if host.contains(':') && !host.starts_with('[') && !host.ends_with(']') {
        format!("[{host}]")
    } else {
        host.to_string()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct BridgeTarget {
    url: String,
    token: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
struct BridgeConfig {
    #[serde(default)]
    gateway: BridgeGatewayConfig,
}

#[derive(Debug, Deserialize)]
struct BridgeGatewayConfig {
    #[serde(default = "default_gateway_host")]
    host: String,
    #[serde(default = "default_gateway_port")]
    port: u16,
    #[serde(default = "default_require_pairing")]
    require_pairing: bool,
    #[serde(default)]
    #[serde(rename = "paired_tokens")]
    _paired_tokens: Vec<String>,
    #[serde(default)]
    path_prefix: Option<String>,
    #[serde(default)]
    tls: Option<BridgeGatewayTlsConfig>,
}

impl BridgeGatewayConfig {
    fn gateway_scheme(&self) -> &'static str {
        if self.tls.as_ref().is_some_and(|tls| tls.enabled) {
            "wss"
        } else {
            "ws"
        }
    }
}

#[derive(Debug, Deserialize)]
struct BridgeGatewayTlsConfig {
    #[serde(default)]
    enabled: bool,
}

impl Default for BridgeGatewayConfig {
    fn default() -> Self {
        Self {
            host: default_gateway_host(),
            port: default_gateway_port(),
            require_pairing: default_require_pairing(),
            _paired_tokens: Vec::new(),
            path_prefix: None,
            tls: None,
        }
    }
}

fn default_gateway_host() -> String {
    "127.0.0.1".to_string()
}

fn default_gateway_port() -> u16 {
    42617
}

fn default_require_pairing() -> bool {
    true
}

async fn write_frame<W>(writer: &mut W, bytes: &[u8]) -> Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer
        .write_all(bytes)
        .await
        .context("failed to write stdout")?;
    writer
        .write_all(b"\n")
        .await
        .context("failed to write stdout newline")?;
    writer.flush().await.context("failed to flush stdout")
}

#[cfg(test)]
mod tests {
    use super::{
        BridgeGatewayConfig, BridgeGatewayTlsConfig, CONFIG_NOT_FOUND_ERROR,
        PAIRING_TOKEN_NOT_FOUND_ERROR, acp_bridge_target, acp_websocket_url, cached_token_path,
        exchange_pairing_code, fetch_pairing_code, http_gateway_url, pair_code_from_args,
        read_cached_token, token_from_env, write_cached_token, write_frame,
    };

    struct EnvGuard {
        key: &'static str,
        original: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let original = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, original }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.original {
                unsafe { std::env::set_var(self.key, value) };
            } else {
                unsafe { std::env::remove_var(self.key) };
            }
        }
    }

    #[tokio::test]
    async fn write_frame_appends_newline() {
        let mut output = Vec::new();

        write_frame(&mut output, br#"{"jsonrpc":"2.0"}"#)
            .await
            .unwrap();

        assert_eq!(output, b"{\"jsonrpc\":\"2.0\"}\n");
    }

    #[test]
    fn acp_websocket_url_uses_config_host_and_port() {
        assert_eq!(
            acp_websocket_url("ws", "192.0.2.10", 49152, None),
            "ws://192.0.2.10:49152/acp"
        );
    }

    #[test]
    fn acp_websocket_url_brackets_ipv6_hosts() {
        assert_eq!(
            acp_websocket_url("ws", "::1", 42617, None),
            "ws://[::1]:42617/acp"
        );
    }

    #[test]
    fn acp_websocket_url_includes_path_prefix() {
        assert_eq!(
            acp_websocket_url("ws", "127.0.0.1", 42617, Some("/zeroclaw")),
            "ws://127.0.0.1:42617/zeroclaw/acp"
        );
    }

    #[test]
    fn acp_websocket_url_uses_wss_for_tls_gateway() {
        assert_eq!(
            acp_websocket_url("wss", "127.0.0.1", 42617, None),
            "wss://127.0.0.1:42617/acp"
        );
    }

    #[test]
    fn acp_bridge_target_does_not_use_persisted_paired_token_hashes_as_bearer_tokens() {
        let config = BridgeGatewayConfig {
            require_pairing: true,
            _paired_tokens: vec![
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string(),
            ],
            ..Default::default()
        };

        let error = acp_bridge_target(&config).unwrap_err().to_string();

        assert_eq!(error, PAIRING_TOKEN_NOT_FOUND_ERROR);
    }

    #[test]
    fn acp_bridge_target_fails_when_pairing_required_without_token() {
        let config = BridgeGatewayConfig::default();

        let error = acp_bridge_target(&config).unwrap_err().to_string();

        assert_eq!(error, PAIRING_TOKEN_NOT_FOUND_ERROR);
    }

    #[test]
    fn acp_bridge_target_allows_unpaired_local_gateway() {
        let config = BridgeGatewayConfig {
            require_pairing: false,
            ..Default::default()
        };

        let target = acp_bridge_target(&config).unwrap();

        assert_eq!(target.token, None);
    }

    #[test]
    fn token_from_env_uses_plaintext_bridge_token() {
        let _guard = EnvGuard::set("ZEROCLAW_ACP_BRIDGE_TOKEN", "zc_plaintext");

        assert_eq!(token_from_env().as_deref(), Some("zc_plaintext"));
    }

    #[test]
    fn cached_token_path_lives_next_to_config_without_using_config_toml() {
        let dir = std::path::Path::new("/tmp/zeroclaw-config");

        assert_eq!(cached_token_path(dir), dir.join("acp-bridge-token"));
    }

    #[tokio::test]
    async fn read_cached_token_trims_private_token_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acp-bridge-token");
        tokio::fs::write(&path, "  zc_cached\n").await.unwrap();

        assert_eq!(
            read_cached_token(&path).await.unwrap().as_deref(),
            Some("zc_cached")
        );
    }

    #[tokio::test]
    async fn write_cached_token_persists_token_for_future_bridge_starts() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acp-bridge-token");

        write_cached_token(&path, "zc_new").await.unwrap();

        assert_eq!(
            read_cached_token(&path).await.unwrap().as_deref(),
            Some("zc_new")
        );
    }

    #[test]
    fn pair_code_from_args_supports_flag_forms() {
        assert_eq!(
            pair_code_from_args(["--pair-code".to_string(), "ABC123".to_string()]).unwrap(),
            Some("ABC123".to_string())
        );
        assert_eq!(
            pair_code_from_args(["--pair-code=XYZ789".to_string()]).unwrap(),
            Some("XYZ789".to_string())
        );
    }

    #[test]
    fn http_gateway_url_honors_path_prefix_and_ipv6() {
        assert_eq!(
            http_gateway_url("https", "::1", 42617, Some("/zeroclaw"), "/pair"),
            "https://[::1]:42617/zeroclaw/pair"
        );
    }

    #[tokio::test]
    async fn fetch_pairing_code_reads_gateway_pair_code_response() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("GET"))
            .and(wiremock::matchers::path("/pair/code"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "success": true,
                    "pairing_required": true,
                    "pairing_code": "PAIR123"
                })),
            )
            .mount(&server)
            .await;

        let code = fetch_pairing_code(&format!("{}/pair/code", server.uri()))
            .await
            .unwrap();

        assert_eq!(code.as_deref(), Some("PAIR123"));
    }

    #[tokio::test]
    async fn exchange_pairing_code_posts_code_and_returns_token() {
        let server = wiremock::MockServer::start().await;
        wiremock::Mock::given(wiremock::matchers::method("POST"))
            .and(wiremock::matchers::path("/pair"))
            .and(wiremock::matchers::header("X-Pairing-Code", "PAIR123"))
            .respond_with(
                wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "paired": true,
                    "persisted": true,
                    "token": "zc_plaintext"
                })),
            )
            .mount(&server)
            .await;

        let token = exchange_pairing_code(&format!("{}/pair", server.uri()), "PAIR123")
            .await
            .unwrap();

        assert_eq!(token, "zc_plaintext");
    }

    #[test]
    fn acp_bridge_target_honors_tls_and_path_prefix() {
        let config = BridgeGatewayConfig {
            require_pairing: false,
            path_prefix: Some("/zeroclaw".to_string()),
            tls: Some(BridgeGatewayTlsConfig { enabled: true }),
            ..Default::default()
        };

        let target = acp_bridge_target(&config).unwrap();

        assert_eq!(target.url, "wss://127.0.0.1:42617/zeroclaw/acp");
    }

    #[test]
    fn missing_config_error_matches_acp_client_guidance() {
        assert_eq!(
            CONFIG_NOT_FOUND_ERROR,
            "ERROR: config.toml not found.  Are you sure the bridge and ZeroClaw are running on the same host?  Tool use will not work remotely!"
        );
    }
}
