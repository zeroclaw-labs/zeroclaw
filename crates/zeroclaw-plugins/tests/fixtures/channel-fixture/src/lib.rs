//! Minimal channel component used by the plugin-host scoped-secret tests.

#[cfg(target_family = "wasm")]
mod component {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities, Guest as Channel, InboundMessage,
        SendMessage, WebhookRejection, WebhookRequest, WebhookResponse,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use zeroclaw::plugin::config::{ConfigError, get as config_get};
    use zeroclaw::plugin::secrets::{SecretError, get as secret_get};
    use zeroclaw::plugin::state::{StateError, get as state_get, put as state_put};

    struct FixtureChannel;
    static SEND_CALL_IN_FLIGHT: AtomicBool = AtomicBool::new(false);
    /// Optional handle taken from the host-provided `configure` payload, so
    /// host tests can observe which config generation an instance was
    /// configured with (reconstruction must replay the constructor snapshot).
    static CONFIGURED_HANDLE: Mutex<Option<String>> = Mutex::new(None);
    /// Guest-held health, flipped by host-enqueued `health:down` and
    /// `health:up` messages the way a real plugin's connection state changes
    /// during its polls. A rebuilt instance starts healthy again.
    static HEALTHY: AtomicBool = AtomicBool::new(true);
    /// `health-check` calls this instance has answered, reported by a
    /// host-enqueued `health:count` message so host tests can bound how often
    /// the host asks.
    static HEALTH_CHECKS: AtomicU32 = AtomicU32::new(0);
    /// Set by a host-enqueued `health:trap` message: the next `health-check`
    /// traps, as a buggy plugin's would.
    static TRAP_HEALTH_CHECK: AtomicBool = AtomicBool::new(false);
    /// Host-held state key set by a host-enqueued `health:trap-always`
    /// message. It outlives a rebuilt instance, so every later `health-check`
    /// traps.
    const ALWAYS_TRAP_HEALTH_CHECK: &str = "health-trap";
    /// Host-held state key set by a host-enqueued `health:trap-alternate`
    /// message. Its revision counts later `health-check` calls across rebuilt
    /// instances, and every other one traps, starting with the first.
    const ALTERNATE_TRAP_HEALTH_CHECK: &str = "health-trap-alternate";

    /// Whether this `health-check` is one that `health:trap-alternate` makes
    /// trap. The count is recorded before the trap, so it survives it.
    fn alternate_health_check_traps() -> bool {
        let Ok(Some(entry)) = state_get(ALTERNATE_TRAP_HEALTH_CHECK) else {
            return false;
        };
        state_put(ALTERNATE_TRAP_HEALTH_CHECK, b"on", Some(entry.revision))
            .expect("fixture counts the health check")
            % 2
            == 0
    }

    /// How many times `configure` has run for this binding. Each run advances
    /// the host-held `channel-session` revision, which outlives a rebuilt
    /// instance, so host tests can count rebuilds. A `send` after a credential
    /// rotation advances it too.
    fn configure_count() -> u64 {
        state_get("channel-session")
            .ok()
            .flatten()
            .map_or(0, |entry| entry.revision)
    }

    fn current_public_config() -> Result<serde_json::Value, String> {
        let config = config_get().map_err(|_| "expected point-of-use public config".to_string())?;
        serde_json::from_str(&config).map_err(|_| "expected public config object".to_string())
    }

    impl PluginInfo for FixtureChannel {
        fn plugin_name() -> String {
            "channel-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Channel for FixtureChannel {
        fn name() -> String {
            "channel-fixture".to_string()
        }

        fn configure() -> Result<(), String> {
            let config = current_public_config()?;
            let public = config
                .as_object()
                .ok_or_else(|| "expected public config object".to_string())?;
            if public
                .get("retry_count")
                .and_then(serde_json::Value::as_u64)
                != Some(5)
            {
                return Err("expected typed retry_count config".to_string());
            }
            if public
                .get("credential_epoch")
                .and_then(serde_json::Value::as_str)
                .is_none_or(str::is_empty)
            {
                return Err("expected credential_epoch config".to_string());
            }
            // Public config carries only non-secret properties. `api_token` is
            // secret and must never surface here. `handle` is an optional
            // non-secret the reconstruction test supplies; stash it so the
            // probed `self-handle` can surface it without a live config frame.
            if public.contains_key("api_token") {
                return Err("secret property leaked into public config".to_string());
            }
            if let Some(handle) = public.get("handle").and_then(serde_json::Value::as_str) {
                *CONFIGURED_HANDLE.lock().unwrap() = Some(handle.to_string());
            }
            if !matches!(secret_get("retry_count"), Err(SecretError::NotFound)) {
                return Err("public property was exposed as a secret".to_string());
            }
            let token = secret_get("api_token")
                .map_err(|_| "expected scoped api_token secret".to_string())?;
            if token.is_empty() {
                return Err("expected non-empty api_token secret".to_string());
            }
            let current = state_get("channel-session")
                .map_err(|_| "expected scoped channel state".to_string())?;
            let expected = current.as_ref().map(|entry| entry.revision);
            let revision = state_put("channel-session", token.as_bytes(), expected)
                .map_err(|_| "expected scoped channel state write".to_string())?;
            if revision != expected.unwrap_or(0) + 1 {
                return Err("unexpected channel state revision".to_string());
            }

            Ok(())
        }

        fn send(message: SendMessage) -> Result<(), String> {
            // Deadline/interruption path: a `spin` message drives an unbounded
            // guest loop whose duration the host wall-clock deadline bounds.
            // Channels have no outbound-HTTP surface (the host's
            // `new_channel_store` withholds `wasi:http`), so the slow operation
            // under test is guest compute, not a network host import. The
            // in-flight guard proves an interrupted instance is discarded rather
            // than resumed: a rebuilt instance is a fresh store whose flag reads
            // false, while a wrongly resumed store would re-enter with it set.
            if message.content.starts_with("spin") {
                if SEND_CALL_IN_FLIGHT.swap(true, Ordering::SeqCst) {
                    return Err("interrupted channel instance was resumed".to_string());
                }
                let mut value = 0_u64;
                loop {
                    value = std::hint::black_box(value.wrapping_add(1));
                }
            }
            // A trap in an export that also has an error result of its own,
            // which the host must tell apart from a returned error string.
            if message.content == "send:trap" {
                panic!("fixture send traps on request");
            }
            // Scoped-secret path (this PR): the message must have been composed
            // from one current config+secret revision resolved at point of use.
            let config = current_public_config()?;
            let epoch = config
                .get("credential_epoch")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| "expected credential_epoch config".to_string())?;
            if !matches!(secret_get("retry_count"), Err(SecretError::NotFound)) {
                return Err("public property was exposed as a secret".to_string());
            }
            let token = secret_get("api_token")
                .map_err(|_| "expected api_token during channel operation".to_string())?;
            let state = state_get("channel-session")
                .map_err(|_| "expected channel state during operation".to_string())?
                .ok_or_else(|| "expected configured channel state".to_string())?;
            if state.value != token.as_bytes() {
                let next_revision =
                    state_put("channel-session", token.as_bytes(), Some(state.revision))
                        .map_err(|_| "expected CAS update after credential rotation".to_string())?;
                if next_revision != state.revision + 1 {
                    return Err("unexpected rotated channel state revision".to_string());
                }
            }
            if message.content != format!("{epoch}:{token}") {
                return Err("message did not use one current config revision".to_string());
            }

            Ok(())
        }

        fn poll_message() -> Option<InboundMessage> {
            let message = zeroclaw::plugin::inbound::inbound_poll()?;
            // Host tests use this to interrupt a poll after the message has
            // already been dequeued from the host-owned queue: the spin runs
            // until the host wall-clock deadline discards this instance.
            if message.content.starts_with("spin") {
                let mut value = 0_u64;
                loop {
                    value = std::hint::black_box(value.wrapping_add(1));
                }
            }
            let content = match message.content.as_str() {
                "health:down" | "health:up" => {
                    HEALTHY.store(message.content == "health:up", Ordering::SeqCst);
                    return None;
                }
                "health:trap" => {
                    TRAP_HEALTH_CHECK.store(true, Ordering::SeqCst);
                    return None;
                }
                "health:trap-always" | "health:trap-alternate" => {
                    let key = if message.content == "health:trap-always" {
                        ALWAYS_TRAP_HEALTH_CHECK
                    } else {
                        ALTERNATE_TRAP_HEALTH_CHECK
                    };
                    let current = state_get(key).ok().flatten().map(|entry| entry.revision);
                    state_put(key, b"on", current).expect("fixture records the health-check trap");
                    return None;
                }
                "health:count" => format!("health-checks:{}", HEALTH_CHECKS.load(Ordering::SeqCst)),
                // Traps after the message left the host queue, so the trap
                // consumes it, as an interrupted poll does.
                "poll:trap" => panic!("fixture poll-message traps on request"),
                "configure:count" => format!("configures:{}", configure_count()),
                _ => message.content,
            };
            Some(InboundMessage {
                id: message.id,
                sender: message.sender,
                reply_target: message.reply_target,
                content,
                // Deliberately untrusted: the host must replace both values
                // with its admitted logical endpoint.
                channel: "guest-channel".to_string(),
                channel_alias: Some("guest-alias".to_string()),
                timestamp: message.timestamp,
                thread_ts: message.thread_ts,
                interruption_scope_id: message.interruption_scope_id,
                attachments: Vec::new(),
                subject: message.subject,
            })
        }

        fn get_channel_capabilities() -> ChannelCapabilities {
            if matches!(config_get(), Err(ConfigError::Unavailable))
                && matches!(secret_get("api_token"), Err(SecretError::Unavailable))
                && matches!(state_get("channel-session"), Err(StateError::Unavailable))
            {
                ChannelCapabilities::HEALTH_CHECK
                    | ChannelCapabilities::SELF_HANDLE
                    | ChannelCapabilities::WEBHOOK_INGRESS
                    | ChannelCapabilities::REQUEST_CHOICE
            } else {
                ChannelCapabilities::empty()
            }
        }

        fn health_check() -> bool {
            HEALTH_CHECKS.fetch_add(1, Ordering::SeqCst);
            let alternate_traps = alternate_health_check_traps();
            assert!(
                !TRAP_HEALTH_CHECK.load(Ordering::SeqCst)
                    && !matches!(state_get(ALWAYS_TRAP_HEALTH_CHECK), Ok(Some(_)))
                    && !alternate_traps,
                "fixture health-check traps on request"
            );
            HEALTHY.load(Ordering::SeqCst)
        }

        fn self_handle() -> Option<String> {
            // Static discovery runs outside a service frame, so config and
            // secrets are unavailable here; surfacing either would mean the host
            // ran this probe in the wrong phase. The handle stashed during
            // `configure` is replayed so the reconstruction metadata check sees
            // a stable value across a rebuilt instance.
            (matches!(config_get(), Err(ConfigError::Unavailable))
                && matches!(secret_get("api_token"), Err(SecretError::Unavailable))
                && matches!(state_get("channel-session"), Err(StateError::Unavailable)))
            .then(|| {
                CONFIGURED_HANDLE
                    .lock()
                    .unwrap()
                    .clone()
                    .unwrap_or_else(|| "@fixture".to_string())
            })
        }

        fn self_addressed_mention() -> Option<String> {
            None
        }

        fn drop_self_message(_msg: InboundMessage) -> bool {
            false
        }

        fn start_typing(_recipient: String) -> Result<(), String> {
            Ok(())
        }

        fn stop_typing(_recipient: String) -> Result<(), String> {
            Ok(())
        }

        fn supports_draft_updates() -> bool {
            false
        }

        fn send_draft(_message: SendMessage) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn update_draft(
            _recipient: String,
            _message_id: String,
            _text: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn update_draft_progress(
            _recipient: String,
            _message_id: String,
            _text: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn finalize_draft(
            _recipient: String,
            _message_id: String,
            _final_text: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn cancel_draft(_recipient: String, _message_id: String) -> Result<(), String> {
            Ok(())
        }

        fn supports_multi_message_streaming() -> bool {
            false
        }

        fn multi_message_delay_ms() -> u64 {
            800
        }

        fn add_reaction(
            _channel: String,
            _message_id: String,
            _emoji: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn remove_reaction(
            _channel: String,
            _message_id: String,
            _emoji: String,
        ) -> Result<(), String> {
            Ok(())
        }

        fn pin_message(_channel: String, _message_id: String) -> Result<(), String> {
            Ok(())
        }

        fn unpin_message(_channel: String, _message_id: String) -> Result<(), String> {
            Ok(())
        }

        fn redact_message(
            _channel: String,
            _message_id: String,
            _reason: Option<String>,
        ) -> Result<(), String> {
            Ok(())
        }

        fn request_approval(
            _recipient: String,
            _request: ApprovalRequest,
        ) -> Result<Option<ApprovalResponse>, String> {
            Ok(None)
        }

        fn request_choice(
            question: String,
            _choices: Vec<String>,
            _timeout_secs: u64,
        ) -> Result<Option<String>, String> {
            // Stands in for a person who takes longer to answer than the
            // host deadline allows.
            if question.starts_with("spin") {
                let mut value = 0_u64;
                loop {
                    value = std::hint::black_box(value.wrapping_add(1));
                }
            }
            Ok(None)
        }

        fn supports_free_form_ask() -> bool {
            true
        }

        fn webhook_path() -> Option<String> {
            Some("fixture".to_string())
        }

        fn parse_webhook(request: WebhookRequest) -> Result<WebhookResponse, WebhookRejection> {
            let WebhookRequest {
                method,
                query,
                headers,
                body,
            } = request;
            if body == b"spin" {
                let mut value = 0_u64;
                loop {
                    value = std::hint::black_box(value.wrapping_add(1));
                }
            }

            let token = secret_get("api_token").map_err(|_| {
                WebhookRejection::Unauthorized("scoped webhook secret unavailable".to_string())
            })?;
            let supplied = headers
                .iter()
                .find(|(name, _)| name == "x-fixture-secret")
                .map(|(_, value)| value.as_str());
            if supplied != Some(token.as_str()) {
                return Err(WebhookRejection::Unauthorized(
                    "private signature mismatch diagnostic".to_string(),
                ));
            }

            if method == "GET" {
                return Ok(WebhookResponse::Reply(query));
            }
            let payload: serde_json::Value = serde_json::from_slice(&body).map_err(|error| {
                WebhookRejection::BadRequest(format!("private parser detail: {error}"))
            })?;
            if let Some(challenge) = payload.get("challenge").and_then(serde_json::Value::as_str) {
                return Ok(WebhookResponse::Reply(challenge.to_string()));
            }
            let field = |name: &str| {
                payload
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
                    .ok_or_else(|| {
                        WebhookRejection::BadRequest(format!(
                            "private parser detail: missing {name}"
                        ))
                    })
            };
            Ok(WebhookResponse::Messages(vec![InboundMessage {
                id: field("id")?,
                sender: field("sender")?,
                reply_target: field("reply_target")?,
                content: field("content")?,
                channel: payload
                    .get("channel")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("spoofed-channel")
                    .to_string(),
                channel_alias: Some("spoofed-alias".to_string()),
                timestamp: 7,
                thread_ts: None,
                interruption_scope_id: None,
                attachments: Vec::new(),
                subject: None,
            }]))
        }
    }

    export!(FixtureChannel);
}
