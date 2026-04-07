//! Gateway channel-pairing web flow (one-click auto-pair).
//!
//! ## Flow
//!
//! 1. User clicks "Connect" button in chat → opens `/pair/auto/{token}`
//! 2. Gateway looks up token → if already linked, auto-pair immediately
//! 3. Otherwise → login page → authenticate → auto-pair → success
//! 4. User returns to chat → next message is accepted automatically
//!
//! For unregistered users:
//! 5. `GET /pair/signup?token={token}` → signup form
//! 6. `POST /pair/signup` → create account → auto-pair → success

use super::AppState;
use axum::{
    extract::{Path, Query, State},
    response::{Html, IntoResponse},
    Form,
};

/// Form data for the login submission.
#[derive(Debug, serde::Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
    pub token: String,
}

/// Form data for the signup submission.
#[derive(Debug, serde::Deserialize)]
pub struct SignupForm {
    pub username: String,
    pub password: String,
    pub password_confirm: String,
    pub token: Option<String>,
}

/// GET /pair/auto/{token}
/// One-click entry point. Looks up the token and either auto-pairs or shows login.
pub async fn handle_auto_pair_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> impl IntoResponse {
    let store = match state.channel_pairing.as_ref() {
        Some(s) => s,
        None => return Html(render_error("Pairing service is not available.")).into_response(),
    };

    // Look up token
    let pairing_token = match store.lookup_token(&token) {
        Some(t) => t,
        None => return Html(render_error(
            "이 링크는 만료되었거나 이미 사용되었습니다.\n\nThis link has expired or was already used.\n\n채팅앱에서 다시 메시지를 보내면 새 링크를 받을 수 있습니다.\nSend another message in your chat to get a new link."
        )).into_response(),
    };

    let channel = &pairing_token.channel;
    let uid = &pairing_token.platform_uid;

    // Check if already linked (via auth store)
    if let Some(ref auth_store) = state.auth_store {
        if let Ok(Some(_user)) = auth_store.find_channel_link(channel, uid) {
            // Auto-pair: consume token, mark paired, persist
            let _ = store.consume_token(&token);
            store.mark_paired(channel, uid, "");
            let ch = channel.to_string();
            let u = uid.to_string();
            tokio::spawn(async move {
                if let Err(e) = tokio::task::spawn_blocking(move || {
                    crate::channels::pairing::persist_channel_allowlist(&ch, &u)
                })
                .await
                .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))
                {
                    tracing::error!("Failed to persist auto-pair: {e}");
                }
            });
            return Html(render_success(channel)).into_response();
        }
    }

    // Show login page with token
    Html(render_login_page(&token, channel, None)).into_response()
}

/// POST /pair/auto/{token}
/// Processes login form with embedded token, then auto-pairs.
pub async fn handle_auto_pair_login(
    State(state): State<AppState>,
    Form(form): Form<LoginForm>,
) -> impl IntoResponse {
    let store = match state.channel_pairing.as_ref() {
        Some(s) => s,
        None => return Html(render_error("Pairing service is not available.")).into_response(),
    };

    // Look up token (don't consume yet)
    let pairing_token = match store.lookup_token(&form.token) {
        Some(t) => t,
        None => return Html(render_error(
            "이 링크는 만료되었습니다. 채팅앱에서 다시 메시지를 보내주세요.\n\nThis link has expired. Send another message in your chat app."
        )).into_response(),
    };

    let auth_store = match state.auth_store {
        Some(ref s) => s,
        None => {
            return Html(render_login_page(
                &form.token,
                &pairing_token.channel,
                Some("Authentication service is not enabled."),
            ))
            .into_response();
        }
    };

    // Authenticate
    let user = match auth_store.authenticate(&form.username, &form.password) {
        Ok(u) => u,
        Err(_) => {
            return Html(render_login_page(
                &form.token,
                &pairing_token.channel,
                Some("아이디 또는 비밀번호가 올바르지 않습니다.\nInvalid username or password."),
            ))
            .into_response();
        }
    };

    let channel = pairing_token.channel.clone();
    let uid = pairing_token.platform_uid.clone();

    // Link channel identity to user account
    // device_id will be selected later (in channel chat) if user has multiple devices.
    if let Err(e) = auth_store.link_channel(&channel, &uid, &user.id, None) {
        tracing::warn!("Failed to link channel: {e}");
    }

    // Consume token + mark paired + persist
    let _ = store.consume_token(&form.token);
    store.mark_paired(&channel, &uid, &user.id);

    let ch = channel.clone();
    let u = uid.clone();
    tokio::spawn(async move {
        if let Err(e) = tokio::task::spawn_blocking(move || {
            crate::channels::pairing::persist_channel_allowlist(&ch, &u)
        })
        .await
        .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))
        {
            tracing::error!("Failed to persist pairing: {e}");
        }
    });

    Html(render_success(&channel)).into_response()
}

/// GET /pair/signup?token={token}
/// Renders the signup page.
#[allow(clippy::implicit_hasher)]
pub async fn handle_pair_signup_page(
    State(state): State<AppState>,
    Query(query): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let token = query.get("token").map(|s| s.as_str()).unwrap_or("");

    // Validate token exists
    if !token.is_empty() {
        if let Some(ref store) = state.channel_pairing {
            if store.lookup_token(token).is_none() {
                return Html(render_error(
                    "이 링크는 만료되었습니다.\n\nThis link has expired.",
                ))
                .into_response();
            }
        }
    }

    Html(render_signup_page(token, None)).into_response()
}

/// POST /pair/signup
/// Creates a new account and auto-pairs.
pub async fn handle_pair_signup_submit(
    State(state): State<AppState>,
    Form(form): Form<SignupForm>,
) -> impl IntoResponse {
    let token_str = form.token.as_deref().unwrap_or("");

    let auth_store = match state.auth_store {
        Some(ref s) => s,
        None => {
            return Html(render_signup_page(
                token_str,
                Some("Authentication service is not enabled."),
            ))
            .into_response();
        }
    };

    if !state.auth_allow_registration {
        return Html(render_signup_page(
            token_str,
            Some("회원가입이 비활성화되어 있습니다.\nRegistration is currently disabled."),
        ))
        .into_response();
    }

    if form.password != form.password_confirm {
        return Html(render_signup_page(
            token_str,
            Some("비밀번호가 일치하지 않습니다.\nPasswords do not match."),
        ))
        .into_response();
    }

    // Register
    let user_id = match auth_store.register(&form.username, &form.password) {
        Ok(id) => id,
        Err(e) => {
            let msg = e.to_string();
            return Html(render_signup_page(token_str, Some(&msg))).into_response();
        }
    };

    // Auto-pair if token is valid
    if !token_str.is_empty() {
        if let Some(ref store) = state.channel_pairing {
            if let Some(pairing_token) = store.consume_token(token_str) {
                let channel = &pairing_token.channel;
                let uid = &pairing_token.platform_uid;

                if let Err(e) = auth_store.link_channel(channel, uid, &user_id, None) {
                    tracing::warn!("Failed to link channel after signup: {e}");
                }

                store.mark_paired(channel, uid, &user_id);

                let ch = channel.to_string();
                let u = uid.to_string();
                tokio::spawn(async move {
                    if let Err(e) = tokio::task::spawn_blocking(move || {
                        crate::channels::pairing::persist_channel_allowlist(&ch, &u)
                    })
                    .await
                    .unwrap_or_else(|e| Err(anyhow::anyhow!("{e}")))
                    {
                        tracing::error!("Failed to persist pairing after signup: {e}");
                    }
                });

                return Html(render_success(channel)).into_response();
            }
        }
    }

    Html(render_account_created()).into_response()
}

// ── HTML Templates ────────────────────────────────────────────────────

/// Escape user-controlled strings for safe HTML interpolation (prevents XSS).
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

fn base_style() -> &'static str {
    r#"
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
        background: #f5f5f5; color: #333;
        display: flex; justify-content: center; align-items: center;
        min-height: 100vh; padding: 20px;
    }
    .card {
        background: #fff; border-radius: 16px; padding: 32px;
        max-width: 400px; width: 100%; box-shadow: 0 4px 24px rgba(0,0,0,0.08);
    }
    .logo { text-align: center; margin-bottom: 24px; }
    .logo h1 { font-size: 28px; color: #1a1a2e; }
    .logo p { font-size: 14px; color: #666; margin-top: 4px; }
    .form-group { margin-bottom: 16px; }
    .form-group label { display: block; font-size: 14px; font-weight: 500; margin-bottom: 6px; color: #444; }
    .form-group input {
        width: 100%; padding: 12px 14px; border: 1.5px solid #ddd;
        border-radius: 10px; font-size: 16px; outline: none; transition: border-color 0.2s;
    }
    .form-group input:focus { border-color: #4a6cf7; }
    .btn {
        width: 100%; padding: 14px; border: none; border-radius: 10px;
        font-size: 16px; font-weight: 600; cursor: pointer; transition: background 0.2s;
    }
    .btn-primary { background: #4a6cf7; color: #fff; }
    .btn-primary:hover { background: #3b5de7; }
    .error { background: #fff0f0; color: #d32f2f; padding: 10px 14px; border-radius: 8px; font-size: 13px; margin-bottom: 16px; }
    .link { text-align: center; margin-top: 16px; font-size: 14px; color: #666; }
    .link a { color: #4a6cf7; text-decoration: none; }
    .link a:hover { text-decoration: underline; }
    .success-icon { text-align: center; font-size: 64px; margin-bottom: 16px; }
    "#
}

fn render_login_page(token: &str, channel: &str, error: Option<&str>) -> String {
    let channel_display = html_escape(channel_display_name(channel));
    let token_escaped = html_escape(token);
    let error_html = error
        .map(|e| format!(r#"<div class="error">{}</div>"#, html_escape(e)))
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="ko"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>MoA - {channel_display} 연결</title>
<style>{style}</style>
</head><body>
<div class="card">
  <div class="logo"><h1>MoA</h1><p>{channel_display} 연결</p></div>
  {error_html}
  <form method="POST" action="/pair/auto/{token_escaped}">
    <input type="hidden" name="token" value="{token_escaped}">
    <div class="form-group">
      <label>아이디 / Username</label>
      <input type="text" name="username" required autocomplete="username" placeholder="Enter username">
    </div>
    <div class="form-group">
      <label>비밀번호 / Password</label>
      <input type="password" name="password" required autocomplete="current-password" placeholder="Enter password">
    </div>
    <button type="submit" class="btn btn-primary">로그인하고 연결하기 / Login & Connect</button>
  </form>
  <div class="link">
    계정이 없으신가요? / No account?<br>
    <a href="/pair/signup?token={token_escaped}">회원가입 / Sign Up</a>
  </div>
</div>
</body></html>"#,
        style = base_style(),
    )
}

fn render_success(channel: &str) -> String {
    let channel_display = html_escape(channel_display_name(channel));

    format!(
        r#"<!DOCTYPE html>
<html lang="ko"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>MoA - 연결 완료!</title>
<style>{style}</style>
</head><body>
<div class="card">
  <div class="success-icon">&#x2705;</div>
  <div class="logo"><h1>연결 완료!</h1><p>Connected Successfully</p></div>
  <p style="text-align:center;font-size:16px;color:#333;margin-top:16px;">
    {channel_display}에서 바로 대화를 시작하세요!<br><br>
    Go back to {channel_display} and start chatting!
  </p>
  <p style="text-align:center;font-size:13px;color:#999;margin-top:24px;">
    이 페이지를 닫아도 됩니다. / You can close this page.
  </p>
</div>
</body></html>"#,
        style = base_style(),
    )
}

fn render_signup_page(token: &str, error: Option<&str>) -> String {
    let token_escaped = html_escape(token);
    let error_html = error
        .map(|e| format!(r#"<div class="error">{}</div>"#, html_escape(e)))
        .unwrap_or_default();

    format!(
        r#"<!DOCTYPE html>
<html lang="ko"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>MoA - 회원가입</title>
<style>{style}</style>
</head><body>
<div class="card">
  <div class="logo"><h1>MoA</h1><p>회원가입 / Sign Up</p></div>
  {error_html}
  <form method="POST" action="/pair/signup">
    <input type="hidden" name="token" value="{token_escaped}">
    <div class="form-group">
      <label>아이디 / Username</label>
      <input type="text" name="username" required autocomplete="username" placeholder="Choose a username">
    </div>
    <div class="form-group">
      <label>비밀번호 / Password</label>
      <input type="password" name="password" required autocomplete="new-password" placeholder="Min 8 characters" minlength="8">
    </div>
    <div class="form-group">
      <label>비밀번호 확인 / Confirm Password</label>
      <input type="password" name="password_confirm" required autocomplete="new-password" placeholder="Re-enter password" minlength="8">
    </div>
    <button type="submit" class="btn btn-primary">가입하고 연결하기 / Sign Up & Connect</button>
  </form>
  <div class="link">
    이미 계정이 있으신가요? / Already have an account?<br>
    <a href="/pair/auto/{token_escaped}">로그인 / Login</a>
  </div>
</div>
</body></html>"#,
        style = base_style(),
    )
}

fn render_error(message: &str) -> String {
    let message_html = html_escape(message).replace('\n', "<br>");
    format!(
        r#"<!DOCTYPE html>
<html lang="ko"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>MoA</title>
<style>{style}</style>
</head><body>
<div class="card">
  <div class="success-icon">&#x26A0;&#xFE0F;</div>
  <div class="logo"><h1>MoA</h1></div>
  <p style="text-align:center;font-size:14px;color:#666;margin-top:16px;">
    {message_html}
  </p>
</div>
</body></html>"#,
        style = base_style(),
    )
}

fn render_account_created() -> String {
    format!(
        r#"<!DOCTYPE html>
<html lang="ko"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>MoA - 가입 완료</title>
<style>{style}</style>
</head><body>
<div class="card">
  <div class="success-icon">&#x1F389;</div>
  <div class="logo"><h1>가입 완료!</h1><p>Account Created</p></div>
  <p style="text-align:center;font-size:14px;color:#666;margin-top:16px;">
    MoA 계정이 생성되었습니다.<br>
    메시징 앱에서 다시 연결하기 버튼을 눌러주세요.<br><br>
    Your MoA account has been created.<br>
    Tap the Connect button again in your messaging app.
  </p>
</div>
</body></html>"#,
        style = base_style(),
    )
}

fn channel_display_name(channel: &str) -> &str {
    match channel {
        "kakao" => "KakaoTalk",
        "telegram" => "Telegram",
        "whatsapp" => "WhatsApp",
        "discord" => "Discord",
        "slack" => "Slack",
        "imessage" => "iMessage",
        "signal" => "Signal",
        "matrix" => "Matrix",
        "email" => "Email",
        "irc" => "IRC",
        "lark" => "Lark",
        "dingtalk" => "DingTalk",
        "qq" => "QQ",
        _ => channel,
    }
}
