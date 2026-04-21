//! `/kakao/share/{token}` endpoint — one-tap share-back via Kakao JS SDK.
//!
//! ## Flow
//!
//! 1. MoA AI reply rendered in the user's 1:1 KakaoTalk chat carries a
//!    quick-reply button labeled `📤 단톡방으로 보내기`. The button URL
//!    points at this endpoint with a one-shot UUID token.
//! 2. User taps the button → mobile browser opens this page → page
//!    loads the pinned Kakao JavaScript SDK and immediately calls
//!    `Kakao.Share.sendDefault({ objectType: 'text', text: '<prefix><body>' })`.
//! 3. KakaoTalk's native share picker appears → user selects the target
//!    단톡방 → message is posted to that chat with the `[🤖 AI 답변]`
//!    prefix visible to all participants.
//! 4. JS success callback `POST`s `/kakao/share/{token}/consume` so the
//!    token cannot be replayed.
//!
//! ## Why the prefix lives only here
//!
//! The user's 1:1 chat with MoA shows the AI's reply *without* the
//! `[🤖 AI 답변]` prefix (cleaner UX). The prefix is added only at the
//! moment of share-back so it appears in the 단톡방 next to the
//! forwarded body, making MoA's involvement explicit to all
//! participants. See `KakaoShareStore::SHARE_PREFIX`.

use super::AppState;
use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{Html, IntoResponse},
};

use crate::channels::kakao_share_store::render_share_text;

/// Pinned Kakao JS SDK URL. Pinning a specific minor version
/// (rather than `latest`) is intentional — auto-upgrading a CDN-loaded
/// SDK in a critical path violates determinism (CLAUDE.md §3.7).
/// Bump deliberately when validating against a known-good release.
const KAKAO_JS_SDK_URL: &str = "https://t1.kakaocdn.net/kakao_js_sdk/2.7.2/kakao.min.js";

/// GET /kakao/share/{token}
///
/// Renders the self-submitting HTML page that calls
/// `Kakao.Share.sendDefault`. Token is looked up but not consumed —
/// consume happens on the JS success callback (POST `/consume`).
pub async fn handle_share_page(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> impl IntoResponse {
    let store = match state.kakao_share_store.as_ref() {
        Some(s) => s,
        None => {
            return Html(render_error_page(
                "공유 기능이 활성화되어 있지 않습니다.\nThis deployment has no share token store configured.",
            ))
            .into_response()
        }
    };

    let entry = match store.lookup_token(&token) {
        Some(e) => e,
        None => {
            return Html(render_error_page(
                "이 공유 링크는 만료되었거나 이미 사용되었습니다.\n채팅창에서 새 답변을 받아 다시 시도해주세요.\n\nThis share link expired or was already used. Tap a fresh AI reply in your chat to get a new link.",
            ))
            .into_response();
        }
    };

    // Read the JS app key from current config — graceful degradation
    // if it has been removed since the token was minted.
    let js_app_key = state
        .config
        .lock()
        .channels_config
        .kakao
        .as_ref()
        .and_then(|k| k.javascript_app_key.clone());

    let js_app_key = match js_app_key {
        Some(k) if !k.trim().is_empty() => k,
        _ => {
            return Html(render_error_page(
                "Kakao JavaScript SDK 키가 설정되어 있지 않습니다.\nKakao JavaScript SDK key not configured. Ask the operator to set channels.kakao.javascript_app_key.",
            ))
            .into_response();
        }
    };

    let body = render_share_text(&entry.message_text);
    Html(render_share_page(&token, &js_app_key, &body)).into_response()
}

/// POST /kakao/share/{token}/consume
///
/// Marks the token consumed so the URL cannot be replayed. Idempotent:
/// returns 204 whether the token existed or not, so the JS callback
/// can fire-and-forget without surfacing transient errors.
pub async fn handle_share_consume(
    State(state): State<AppState>,
    Path(token): Path<String>,
) -> StatusCode {
    if let Some(store) = state.kakao_share_store.as_ref() {
        let _ = store.consume_token(&token);
    }
    StatusCode::NO_CONTENT
}

/// Escape user-controlled strings for safe embedding inside an HTML
/// `<script>` JSON literal. Same shape as `pair::html_escape`, kept
/// local to this module per CLAUDE.md §3.3 (rule of three) — only one
/// caller today.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Escape for embedding inside a JavaScript string literal in a
/// `<script>` block. Defense in depth:
/// - `serde_json::to_string` quotes the value and escapes `"`, `\`,
///   newlines, and other control characters.
/// - We additionally rewrite `</` to `<\/` because `serde_json` does
///   NOT escape forward slashes by default — without this rewrite a
///   payload containing `</script>` would close our inline `<script>`
///   block. This is the standard "JSON inside HTML" mitigation.
fn js_string_literal(s: &str) -> String {
    let json = serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string());
    json.replace("</", "<\\/")
}

fn render_share_page(token: &str, js_app_key: &str, body: &str) -> String {
    let token_escaped = html_escape(token);
    let key_escaped = html_escape(js_app_key);
    let body_js = js_string_literal(body);
    let preview_html = html_escape(body);

    format!(
        r#"<!DOCTYPE html>
<html lang="ko"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>MoA — 단톡방으로 공유</title>
<style>{style}</style>
</head><body>
<div class="card">
  <div class="logo"><h1>MoA</h1><p>단톡방으로 공유</p></div>
  <p class="hint">아래 버튼을 눌러 보낼 단톡방을 선택해주세요.<br>Tap below to pick a chat to send this to.</p>
  <pre class="preview">{preview_html}</pre>
  <button id="share-btn" class="btn btn-primary" disabled>📤 단톡방 선택하기</button>
  <p id="status" class="status"></p>
</div>
<script src="{sdk_url}" integrity="" crossorigin="anonymous"></script>
<script>
(function() {{
  var statusEl = document.getElementById('status');
  var btn = document.getElementById('share-btn');
  var token = "{token_js}";
  var body = {body_js};
  var jsKey = "{key_js}";

  if (typeof Kakao === 'undefined') {{
    statusEl.textContent = 'Kakao SDK 로드 실패. Kakao SDK failed to load.';
    return;
  }}
  try {{
    if (!Kakao.isInitialized()) {{
      Kakao.init(jsKey);
    }}
  }} catch (e) {{
    statusEl.textContent = 'Kakao SDK 초기화 실패: ' + (e && e.message ? e.message : e);
    return;
  }}

  btn.disabled = false;

  function consume() {{
    fetch('/kakao/share/' + encodeURIComponent(token) + '/consume', {{ method: 'POST' }})
      .catch(function() {{ /* fire-and-forget */ }});
  }}

  btn.addEventListener('click', function() {{
    btn.disabled = true;
    try {{
      Kakao.Share.sendDefault({{
        objectType: 'text',
        text: body,
        link: {{ mobileWebUrl: '', webUrl: '' }},
        success: function() {{
          statusEl.textContent = '✅ 전송 완료. 단톡방을 확인해주세요.';
          consume();
        }},
        fail: function(err) {{
          statusEl.textContent = '전송에 실패했습니다: ' + (err && err.msg ? err.msg : err);
          btn.disabled = false;
        }},
      }});
    }} catch (e) {{
      statusEl.textContent = '공유 중 오류: ' + (e && e.message ? e.message : e);
      btn.disabled = false;
    }}
  }});
}})();
</script>
</body></html>"#,
        style = base_style(),
        sdk_url = KAKAO_JS_SDK_URL,
        token_js = token_escaped,
        key_js = key_escaped,
        body_js = body_js,
        preview_html = preview_html,
    )
}

fn render_error_page(message: &str) -> String {
    let msg_escaped = html_escape(message);
    format!(
        r#"<!DOCTYPE html>
<html lang="ko"><head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>MoA — 공유 오류</title>
<style>{style}</style>
</head><body>
<div class="card">
  <div class="logo"><h1>MoA</h1><p>공유 오류 / Share error</p></div>
  <p class="hint">{msg_escaped}</p>
</div>
</body></html>"#,
        style = base_style(),
        msg_escaped = msg_escaped,
    )
}

fn base_style() -> &'static str {
    r#"
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
        background: #f5f5f5; color: #333;
        display: flex; justify-content: center; align-items: flex-start;
        min-height: 100vh; padding: 20px;
    }
    .card {
        background: #fff; border-radius: 16px; padding: 24px;
        max-width: 480px; width: 100%; box-shadow: 0 4px 24px rgba(0,0,0,0.08);
    }
    .logo { text-align: center; margin-bottom: 20px; }
    .logo h1 { font-size: 26px; color: #1a1a2e; }
    .logo p { font-size: 13px; color: #666; margin-top: 4px; }
    .hint { font-size: 14px; color: #555; margin-bottom: 16px; line-height: 1.5; }
    .preview {
        background: #f7f7fa; border-radius: 10px; padding: 14px;
        font-size: 14px; line-height: 1.5; white-space: pre-wrap; word-break: break-word;
        max-height: 240px; overflow-y: auto; margin-bottom: 16px;
        font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
    }
    .btn {
        width: 100%; padding: 14px; border: none; border-radius: 10px;
        font-size: 16px; font-weight: 600; cursor: pointer; transition: background 0.2s;
    }
    .btn-primary { background: #fee500; color: #1a1a2e; }
    .btn-primary:hover { background: #fada0c; }
    .btn-primary:disabled { background: #eee; color: #aaa; cursor: not-allowed; }
    .status { text-align: center; font-size: 14px; color: #555; margin-top: 14px; min-height: 1.4em; }
    "#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn share_page_renders_token_body_and_prefix() {
        let html = render_share_page("tok_abc", "jsapp_key", &render_share_text("hello"));
        assert!(html.contains("tok_abc"));
        assert!(html.contains("jsapp_key"));
        // SHARE_PREFIX appears inside the JS string literal (escaped)
        // and inside the human preview (HTML-escaped).
        assert!(html.contains("AI 답변"));
        // Pinned SDK URL must be present.
        assert!(html.contains("kakao.min.js"));
    }

    #[test]
    fn share_page_html_escapes_user_input() {
        // Ensure `<script>` injection in the message body is neutralised.
        let nasty = "<script>alert(1)</script>";
        let html = render_share_page("tok", "key", &render_share_text(nasty));
        // Neither raw nor unescaped version should appear in the preview
        // section (the JS string literal handles it via JSON encoding).
        assert!(!html.contains("<script>alert(1)</script>"));
        assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    }

    #[test]
    fn share_page_html_escapes_token_into_attributes() {
        let html = render_share_page("\"&<>", "key", &render_share_text("body"));
        // The token appears inside a JS string literal — check it was
        // not embedded raw, breaking out of the quote.
        assert!(!html.contains("\"\"&<>"));
        assert!(html.contains("&quot;"));
    }

    #[test]
    fn error_page_renders_message_escaped() {
        let html = render_error_page("<dangerous>");
        assert!(html.contains("&lt;dangerous&gt;"));
        assert!(!html.contains("<dangerous>"));
    }

    #[test]
    fn js_string_literal_round_trips_simple_text() {
        // serde_json wraps the value in quotes and escapes — used to
        // pass user-controlled strings into the inline <script>.
        assert_eq!(js_string_literal("hello"), "\"hello\"");
        assert!(js_string_literal("a\"b").contains("\\\""));
        assert!(js_string_literal("line1\nline2").contains("\\n"));
    }
}
