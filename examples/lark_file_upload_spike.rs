//! Lark / Feishu file upload spike — validates the API path PR 3 will rely on.
//!
//! ## Why this exists
//!
//! PR 3 plans to override `Channel::send_with_artifacts` for `LarkChannel` so
//! tool-produced files (`.docx`, `.pdf`, …) become native attachments in the
//! chat instead of click-through download links. Before committing to that
//! design we want to validate the happy-path end-to-end, against a real
//! tenant, with our existing `reqwest` + `tokio` stack.
//!
//! Specifically this spike answers:
//!
//! 1. **Upload mechanics** — what does the multipart payload to
//!    `POST /open-apis/im/v1/files` look like, and what is the shape of the
//!    `data.file_key` we get back?
//! 2. **Send mechanics** — does `POST /open-apis/im/v1/messages` with
//!    `msg_type=file` and `content={"file_key":"..."}` produce a usable
//!    attachment card in the chat client?
//! 3. **UX comparison** — does a standalone file message look better than
//!    the existing interactive card with a download button, or worse?
//!
//! ## How to run
//!
//! ```bash
//! cargo run --example lark_file_upload_spike -- \
//!     --app-id cli_xxx --app-secret yyy \
//!     --chat-id oc_zzz \
//!     --file ./testdata/sample.docx \
//!     --platform feishu        # or `lark`
//! ```
//!
//! Required env vars (alternative to flags):
//! - `LARK_APP_ID`, `LARK_APP_SECRET`, `LARK_CHAT_ID`, `LARK_TEST_FILE`
//! - `LARK_PLATFORM=feishu` or `lark`
//!
//! ## Status
//!
//! This is a **research-only** binary. It is not wired into the production
//! agent loop and ships zero behaviour change. PR 3 will use the validated
//! API shape to implement the trait method properly.
//!
//! ## Critical things to look for when running
//!
//! - File size limit: Lark documents 30MB; a file at 31MB should fail with
//!   a deterministic error code, not a confusing 500. Try an oversized file.
//! - File type: the `file_type` parameter must be one of
//!   `opus|mp4|pdf|doc|xls|ppt|stream`. `stream` is the catch-all for
//!   `.docx`/`.pptx`/`.xlsx`/`.csv` etc. — confirm it actually works for
//!   those office formats and not just generic blobs.
//! - Naming: confirm Chinese filenames survive the multipart round-trip
//!   without mojibake.
//! - Permissions: the bot must be a member of the target chat AND have
//!   `im:resource:upload` + `im:message` scopes. Spike output will tell
//!   you which scope is missing if the call fails.

use anyhow::{anyhow, Context, Result};
use clap::Parser;
use reqwest::multipart::{Form, Part};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Instant;

/// Raw clap-parsed arguments. ENV vars are resolved separately in
/// `Args::resolve` so we do not pull in clap's `env` feature on the main
/// crate just for a spike.
#[derive(Debug, Clone, Parser)]
#[command(about = "PR 3 spike: upload a file to Lark/Feishu and send as native attachment")]
struct RawArgs {
    /// Lark app id (`cli_xxx`). Falls back to env `LARK_APP_ID`.
    #[arg(long)]
    app_id: Option<String>,

    /// Lark app secret. Falls back to env `LARK_APP_SECRET`.
    #[arg(long)]
    app_secret: Option<String>,

    /// Target chat ID (`oc_xxx` for chats, or open_id for direct messages).
    /// Falls back to env `LARK_CHAT_ID`.
    #[arg(long)]
    chat_id: Option<String>,

    /// Path to the file to upload. Falls back to env `LARK_TEST_FILE`.
    #[arg(long)]
    file: Option<PathBuf>,

    /// `feishu` (CN) or `lark` (international). Falls back to env
    /// `LARK_PLATFORM`, defaults to `feishu`.
    #[arg(long)]
    platform: Option<String>,

    /// `chat_id`, `open_id`, `union_id`, `email`, or `user_id`. Default
    /// `chat_id` matches `LarkChannel::send_message_url()`.
    #[arg(long, default_value = "chat_id")]
    receive_id_type: String,

    /// Skip sending the message after upload (just validates the upload path).
    #[arg(long)]
    upload_only: bool,
}

#[derive(Debug, Clone)]
struct Args {
    app_id: String,
    app_secret: String,
    chat_id: String,
    file: PathBuf,
    platform: String,
    receive_id_type: String,
    upload_only: bool,
}

impl Args {
    /// Merge clap arguments with env-var fallbacks. Fails with a clear
    /// message naming both the flag and the env var when something is
    /// missing — easier to diagnose than clap's auto-generated error
    /// when the user forgot to export the credentials.
    fn resolve(raw: RawArgs) -> Result<Self> {
        fn pick(flag_val: Option<String>, env_key: &str, flag_name: &str) -> Result<String> {
            flag_val
                .or_else(|| std::env::var(env_key).ok())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| anyhow!("missing --{flag_name} (or set env var {env_key})"))
        }
        let app_id = pick(raw.app_id, "LARK_APP_ID", "app-id")?;
        let app_secret = pick(raw.app_secret, "LARK_APP_SECRET", "app-secret")?;
        let chat_id = pick(raw.chat_id, "LARK_CHAT_ID", "chat-id")?;
        let file = raw
            .file
            .or_else(|| std::env::var("LARK_TEST_FILE").ok().map(PathBuf::from))
            .ok_or_else(|| anyhow!("missing --file (or set env var LARK_TEST_FILE)"))?;
        let platform = raw
            .platform
            .or_else(|| std::env::var("LARK_PLATFORM").ok())
            .unwrap_or_else(|| "feishu".into());
        Ok(Self {
            app_id,
            app_secret,
            chat_id,
            file,
            platform,
            receive_id_type: raw.receive_id_type,
            upload_only: raw.upload_only,
        })
    }

    fn api_base(&self) -> &'static str {
        match self.platform.to_ascii_lowercase().as_str() {
            "lark" => "https://open.larksuite.com/open-apis",
            // Default to feishu so an unknown value fails fast against the CN
            // endpoint rather than silently routing internationally.
            _ => "https://open.feishu.cn/open-apis",
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::resolve(RawArgs::parse())?;
    init_tracing();

    println!("\n── Lark file upload spike ──");
    println!("platform     : {}", args.platform);
    println!("api_base     : {}", args.api_base());
    println!("chat_id      : {}", args.chat_id);
    println!("file         : {}", args.file.display());
    println!("upload_only  : {}", args.upload_only);
    println!();

    // 1. Acquire tenant access token. Uses the same endpoint as
    //    `LarkChannel::tenant_access_token_url`, deliberately re-implemented
    //    here so the spike has zero coupling to production code.
    let client = reqwest::Client::builder()
        .build()
        .context("build reqwest client")?;
    let t0 = Instant::now();
    let token = fetch_tenant_token(&client, args.api_base(), &args.app_id, &args.app_secret)
        .await
        .context("fetch tenant_access_token (check app_id / app_secret)")?;
    println!(
        "✅ token acquired in {} ms (length: {})",
        t0.elapsed().as_millis(),
        token.len()
    );

    // 2. Upload the file. Multipart parts: file_type, file_name, file.
    //    `file_type=stream` is the catch-all for .docx/.pptx/.xlsx/.csv —
    //    PR 3 will use it for everything except known opus/mp4/pdf/doc/xls/ppt.
    let bytes = tokio::fs::read(&args.file)
        .await
        .with_context(|| format!("read {}", args.file.display()))?;
    let display_name = args
        .file
        .file_name()
        .ok_or_else(|| anyhow!("file has no basename: {}", args.file.display()))?
        .to_string_lossy()
        .into_owned();
    let file_type = guess_file_type(&display_name);
    println!(
        "→ uploading {} bytes as file_type={} name={}",
        bytes.len(),
        file_type,
        display_name
    );

    let t1 = Instant::now();
    let file_key = upload_file(
        &client,
        args.api_base(),
        &token,
        file_type,
        &display_name,
        bytes,
    )
    .await
    .context("upload to /im/v1/files")?;
    println!(
        "✅ uploaded in {} ms — file_key = {}",
        t1.elapsed().as_millis(),
        file_key
    );

    if args.upload_only {
        println!("\n--upload-only set; skipping send. file_key reusable until expiry.");
        return Ok(());
    }

    // 3. Send a `msg_type=file` message referring to the new file_key.
    let t2 = Instant::now();
    let send_resp = send_file_message(
        &client,
        args.api_base(),
        &token,
        &args.receive_id_type,
        &args.chat_id,
        &file_key,
    )
    .await
    .context("send file message")?;
    println!("✅ message sent in {} ms", t2.elapsed().as_millis());
    println!(
        "   message_id = {}",
        send_resp
            .get("data")
            .and_then(|d| d.get("message_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("(missing)")
    );
    println!("   raw response: {}", send_resp);
    println!("\n→ Now check the target chat in the Feishu/Lark client.");
    println!("  Look for a native file attachment card with a clickable filename.");
    println!("  Compare against the existing 'interactive card with Download button' UX.");

    Ok(())
}

fn init_tracing() {
    use tracing_subscriber::EnvFilter;
    let _ = tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .with_target(false)
        .try_init();
}

/// Mirrors `LarkChannel::get_tenant_access_token` minimally: POSTs the app
/// credentials and returns `data.tenant_access_token`. Fails loudly on any
/// non-zero `code` (production code retries; spike does not — we want to see
/// the actual error).
async fn fetch_tenant_token(
    client: &reqwest::Client,
    api_base: &str,
    app_id: &str,
    app_secret: &str,
) -> Result<String> {
    let url = format!("{api_base}/auth/v3/tenant_access_token/internal");
    let body = serde_json::json!({
        "app_id": app_id,
        "app_secret": app_secret,
    });
    let resp = client
        .post(&url)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let json: Value = resp.json().await?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {json}"));
    }
    let code = json.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return Err(anyhow!(
            "tenant_access_token returned code {code}: {}",
            json.get("msg")
                .and_then(Value::as_str)
                .unwrap_or("(no msg)")
        ));
    }
    json.get("tenant_access_token")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("missing tenant_access_token in response: {json}"))
}

/// Upload to `/im/v1/files`. Returns `data.file_key` on success.
///
/// The endpoint is documented to accept multipart with these required fields:
/// - `file_type` — `opus | mp4 | pdf | doc | xls | ppt | stream`
/// - `file_name` — display name shown in chat
/// - `file` — the binary
///
/// Returns the platform error verbatim on non-zero `code` so the spike
/// surfaces scope/auth issues instead of swallowing them.
async fn upload_file(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    file_type: &str,
    file_name: &str,
    bytes: Vec<u8>,
) -> Result<String> {
    let url = format!("{api_base}/im/v1/files");
    // The `Part::bytes` filename here is what Lark uses on the wire; the
    // separate `file_name` form field is what shows up in chat. Both must
    // be set or the API returns a confusing 400.
    let form = Form::new()
        .text("file_type", file_type.to_string())
        .text("file_name", file_name.to_string())
        .part("file", Part::bytes(bytes).file_name(file_name.to_string()));

    let resp = client
        .post(&url)
        .bearer_auth(token)
        .multipart(form)
        .send()
        .await?;
    let status = resp.status();
    let json: Value = resp.json().await?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {json}"));
    }
    let code = json.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return Err(anyhow!(
            "upload returned code {code}: {} — full body: {json}",
            json.get("msg")
                .and_then(Value::as_str)
                .unwrap_or("(no msg)")
        ));
    }
    json.get("data")
        .and_then(|d| d.get("file_key"))
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("missing data.file_key: {json}"))
}

/// Send a file message via `/im/v1/messages?receive_id_type=...`.
async fn send_file_message(
    client: &reqwest::Client,
    api_base: &str,
    token: &str,
    receive_id_type: &str,
    receive_id: &str,
    file_key: &str,
) -> Result<Value> {
    let url = format!("{api_base}/im/v1/messages?receive_id_type={receive_id_type}");
    // Lark API requires `content` to be a JSON-encoded *string*, not an object.
    // This is the same envelope shape used in `LarkChannel::send`.
    let inner_content = serde_json::json!({ "file_key": file_key }).to_string();
    let body = serde_json::json!({
        "receive_id": receive_id,
        "msg_type": "file",
        "content": inner_content,
    });
    let resp = client
        .post(&url)
        .bearer_auth(token)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let json: Value = resp.json().await?;
    if !status.is_success() {
        return Err(anyhow!("HTTP {status}: {json}"));
    }
    let code = json.get("code").and_then(Value::as_i64).unwrap_or(-1);
    if code != 0 {
        return Err(anyhow!(
            "send returned code {code}: {} — body: {json}",
            json.get("msg")
                .and_then(Value::as_str)
                .unwrap_or("(no msg)")
        ));
    }
    Ok(json)
}

/// Map a filename's extension to the `file_type` value the Lark API requires.
///
/// The spike intentionally inlines this rather than reusing the production
/// `mime_for_extension` helper — the API takes a small fixed set of strings,
/// and we want the spike to show exactly what we send on the wire.
fn guess_file_type(filename: &str) -> &'static str {
    let ext = filename
        .rsplit('.')
        .next()
        .map(str::to_ascii_lowercase)
        .unwrap_or_default();
    match ext.as_str() {
        "pdf" => "pdf",
        "doc" => "doc",
        "docx" => "doc", // Lark groups .docx under `doc` — confirm in spike output
        "xls" => "xls",
        "xlsx" => "xls",
        "ppt" => "ppt",
        "pptx" => "ppt",
        "opus" => "opus",
        "mp4" => "mp4",
        // Catch-all for everything else (.csv, .md, .zip, .json, .png, …).
        // PR 3 may need to special-case images via `image_key` upload — the
        // spike does NOT cover that path; this is a separate question.
        _ => "stream",
    }
}
