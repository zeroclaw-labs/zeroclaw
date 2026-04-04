use crate::gateway::AppState;
use axum::{
    extract::{Query, State, WebSocketUpgrade},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};

#[derive(serde::Deserialize)]
pub(crate) struct WebChannelWsQuery {
    token: Option<String>,
}

pub(crate) async fn handle_ws_channel(
    State(state): State<AppState>,
    Query(params): Query<WebChannelWsQuery>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    if state.pairing.require_pairing() {
        let token = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|auth| auth.strip_prefix("Bearer "))
            .or_else(|| {
                headers
                    .get("sec-websocket-protocol")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|protos| {
                        protos
                            .split(',')
                            .find_map(|part| part.trim().strip_prefix("bearer."))
                    })
            })
            .or(params.token.as_deref())
            .unwrap_or("");
        if !state.pairing.is_authenticated(token) {
            return (
                StatusCode::UNAUTHORIZED,
                "Unauthorized — provide Authorization header or ?token= query param",
            )
                .into_response();
        }
    }

    let Some(message_tx) = super::web_channel::get_web_channel_tx() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "Web channel not initialized",
        )
            .into_response();
    };
    let web_channel = super::web_channel::get_or_init_web_channel();
    ws.on_upgrade(move |socket| {
        super::web_channel::handle_ws_connection(socket, web_channel, message_tx)
    })
    .into_response()
}
