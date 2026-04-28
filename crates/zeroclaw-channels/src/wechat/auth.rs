//! WeChat iLink Bot QR-code login flow.
//!
//! Used by the `zeroclaw channel wechat login` CLI to obtain credentials and
//! save them via [`super::accounts::save_account`].

use std::time::{Duration, Instant};

use qrcode::{render::unicode, QrCode};
use reqwest::header::HeaderMap;
use serde::Deserialize;
use tracing::{info, warn};

use super::accounts::{normalize_account_id, save_account, AccountData, DEFAULT_BASE_URL};

const DEFAULT_BOT_TYPE: &str = "3";
const POLL_TIMEOUT_MS: u64 = 35_000;
const MAX_QR_REFRESH_COUNT: u8 = 3;
const DEFAULT_LOGIN_TIMEOUT_SECS: u64 = 480;

#[derive(Debug, Clone)]
pub struct LoginOptions {
    pub base_url: String,
    pub timeout: Duration,
}

impl Default for LoginOptions {
    fn default() -> Self {
        Self {
            base_url: DEFAULT_BASE_URL.to_string(),
            timeout: Duration::from_secs(DEFAULT_LOGIN_TIMEOUT_SECS),
        }
    }
}

#[derive(Debug, Deserialize)]
struct QrCodeResponse {
    qrcode: String,
    qrcode_img_content: String,
}

#[derive(Debug, Deserialize)]
struct StatusResponse {
    status: String,
    bot_token: Option<String>,
    ilink_bot_id: Option<String>,
    baseurl: Option<String>,
    ilink_user_id: Option<String>,
}

#[derive(Debug, Clone)]
struct LoginResult {
    account_id: String,
    token: String,
    base_url: Option<String>,
    user_id: Option<String>,
}

/// Run the QR-code login flow end-to-end. Prints the QR code to stdout, polls
/// for confirmation, and saves credentials. Returns the saved `account_id`.
pub async fn login(opts: LoginOptions) -> anyhow::Result<String> {
    let client = reqwest::Client::builder().build()?;

    let qr = fetch_qr_code(&client, &opts.base_url).await?;
    println!("请使用微信 App 的「扫一扫」扫描下方二维码:\n");
    print_qr_terminal(&qr.qrcode_img_content);
    println!("\n二维码链接（备用）:\n{}\n", qr.qrcode_img_content);

    let result = wait_for_login(&client, &opts.base_url, qr, opts.timeout).await?;

    let account_id = normalize_account_id(&result.account_id);
    save_account(
        &account_id,
        AccountData {
            token: Some(result.token),
            base_url: result.base_url.or_else(|| Some(opts.base_url)),
            user_id: result.user_id,
            saved_at: None,
        },
    )?;
    Ok(account_id)
}

async fn fetch_qr_code(client: &reqwest::Client, base_url: &str) -> anyhow::Result<QrCodeResponse> {
    let base = format!("{}/", base_url.trim_end_matches('/'));
    let url = format!(
        "{base}ilink/bot/get_bot_qrcode?bot_type={}",
        urlencoding::encode(DEFAULT_BOT_TYPE)
    );
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        anyhow::bail!("fetch qr failed: HTTP {}", resp.status());
    }
    Ok(resp.json::<QrCodeResponse>().await?)
}

async fn poll_qr_status(
    client: &reqwest::Client,
    base_url: &str,
    qrcode: &str,
) -> anyhow::Result<StatusResponse> {
    let mut headers = HeaderMap::new();
    headers.insert("iLink-App-ClientVersion", "1".parse().unwrap());
    let base = format!("{}/", base_url.trim_end_matches('/'));
    let url = format!(
        "{base}ilink/bot/get_qrcode_status?qrcode={}",
        urlencoding::encode(qrcode)
    );
    let req = client
        .get(url)
        .headers(headers)
        .timeout(Duration::from_millis(POLL_TIMEOUT_MS));
    match req.send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                anyhow::bail!("poll qr failed: HTTP {}", resp.status());
            }
            Ok(resp.json::<StatusResponse>().await?)
        }
        Err(e) if e.is_timeout() => Ok(StatusResponse {
            status: "wait".to_string(),
            bot_token: None,
            ilink_bot_id: None,
            baseurl: None,
            ilink_user_id: None,
        }),
        Err(e) => Err(e.into()),
    }
}

async fn wait_for_login(
    client: &reqwest::Client,
    base_url: &str,
    initial: QrCodeResponse,
    timeout: Duration,
) -> anyhow::Result<LoginResult> {
    let started = Instant::now();
    let mut qrcode = initial.qrcode;
    let mut refreshes = 1u8;

    loop {
        if started.elapsed() > timeout {
            anyhow::bail!("login timed out before confirmation");
        }
        match poll_qr_status(client, base_url, &qrcode).await {
            Ok(status) => match status.status.as_str() {
                "wait" => {}
                "scaned" => info!("二维码已扫码，等待确认"),
                "confirmed" => {
                    let token = status
                        .bot_token
                        .ok_or_else(|| anyhow::anyhow!("server returned no bot_token"))?;
                    let account_id = status
                        .ilink_bot_id
                        .ok_or_else(|| anyhow::anyhow!("server returned no ilink_bot_id"))?;
                    return Ok(LoginResult {
                        account_id,
                        token,
                        base_url: status.baseurl,
                        user_id: status.ilink_user_id,
                    });
                }
                "expired" => {
                    if refreshes >= MAX_QR_REFRESH_COUNT {
                        anyhow::bail!("QR code expired too many times");
                    }
                    refreshes += 1;
                    warn!("二维码过期，刷新中 ({refreshes}/{MAX_QR_REFRESH_COUNT})");
                    let refreshed = fetch_qr_code(client, base_url).await?;
                    qrcode = refreshed.qrcode;
                    println!("\n新二维码:\n");
                    print_qr_terminal(&refreshed.qrcode_img_content);
                }
                other => warn!("未知扫码状态: {other}"),
            },
            Err(e) => warn!("轮询扫码状态失败 (将重试): {e}"),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn print_qr_terminal(content: &str) {
    match QrCode::new(content.as_bytes()) {
        Ok(code) => {
            let rendered = code.render::<unicode::Dense1x2>().build();
            println!("{rendered}");
        }
        Err(_) => println!("(二维码渲染失败，请使用上方备用链接)"),
    }
}
