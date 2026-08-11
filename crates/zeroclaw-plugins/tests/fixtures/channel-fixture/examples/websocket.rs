//! Component fixture exercising the host-mediated WebSocket resource.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0", "plugins-wit-v0-websocket"],
    });

    use std::cell::RefCell;

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities, Guest as Channel, InboundMessage,
        SendMessage,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    use zeroclaw::plugin::config::get as config_get;
    use zeroclaw::plugin::websocket::{
        self, ConnectOptions, Connection, Event, Header, Message, WebsocketError,
    };

    struct FixtureChannel;

    thread_local! {
        static CONNECTION: RefCell<Option<Connection>> = const { RefCell::new(None) };
        static NEXT_ID: RefCell<u64> = const { RefCell::new(0) };
    }

    fn describe_error(error: WebsocketError) -> String {
        format!("websocket host call failed: {error:?}")
    }

    fn inbound(content: String) -> InboundMessage {
        let id = NEXT_ID.with(|next| {
            let mut next = next.borrow_mut();
            *next += 1;
            format!("websocket-{next}")
        });
        InboundMessage {
            id,
            sender: "echo-server".to_string(),
            reply_target: "echo-server".to_string(),
            content,
            channel: "untrusted-guest-channel".to_string(),
            channel_alias: Some("untrusted-guest-alias".to_string()),
            timestamp: 1,
            thread_ts: None,
            interruption_scope_id: None,
            attachments: Vec::new(),
            subject: None,
        }
    }

    impl PluginInfo for FixtureChannel {
        fn plugin_name() -> String {
            "channel-websocket-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Channel for FixtureChannel {
        fn name() -> String {
            "channel-websocket-fixture".to_string()
        }

        fn configure() -> Result<(), String> {
            let config = config_get().map_err(|_| "expected websocket fixture config")?;
            let config: serde_json::Value =
                serde_json::from_str(&config).map_err(|_| "expected config object")?;
            let url = config
                .get("url")
                .and_then(serde_json::Value::as_str)
                .ok_or("expected websocket URL")?;
            let connection = websocket::connect(&ConnectOptions {
                url: url.to_string(),
                headers: vec![Header {
                    name: "x-fixture".to_string(),
                    value: "channel-websocket".to_string(),
                }],
                subprotocols: vec!["echo.v1".to_string()],
                tls_profile: None,
            })
            .map_err(describe_error)?;
            let selected = connection
                .negotiated_subprotocol()
                .map_err(describe_error)?
                .ok_or("expected negotiated subprotocol")?;
            if selected != "echo.v1" {
                return Err("unexpected negotiated subprotocol".to_string());
            }
            connection
                .send(&Message::Text("component-text".to_string()))
                .map_err(describe_error)?;
            connection
                .send(&Message::Binary(vec![0, 1, 2, 255]))
                .map_err(describe_error)?;
            CONNECTION.with(|slot| {
                *slot.borrow_mut() = Some(connection);
            });
            Ok(())
        }

        fn send(_message: SendMessage) -> Result<(), String> {
            Ok(())
        }

        fn poll_message() -> Option<InboundMessage> {
            CONNECTION.with(|slot| {
                let mut slot = slot.borrow_mut();
                let connection = slot.as_ref()?;
                match connection.receive() {
                    Ok(Some(Event::Message(Message::Text(text)))) => {
                        Some(inbound(format!("text:{text}:echo.v1")))
                    }
                    Ok(Some(Event::Message(Message::Binary(bytes)))) => Some(inbound(format!(
                        "binary:{}",
                        bytes
                            .iter()
                            .map(|byte| format!("{byte:02x}"))
                            .collect::<String>()
                    ))),
                    Ok(Some(Event::Closed(_))) | Ok(Some(Event::Failed(_))) | Err(_) => {
                        slot.take();
                        None
                    }
                    Ok(None) => None,
                }
            })
        }

        fn get_channel_capabilities() -> ChannelCapabilities {
            ChannelCapabilities::empty()
        }

        fn health_check() -> bool {
            true
        }

        fn self_handle() -> Option<String> {
            None
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
            _question: String,
            _choices: Vec<String>,
            _timeout_secs: u64,
        ) -> Result<Option<String>, String> {
            Ok(None)
        }

        fn supports_free_form_ask() -> bool {
            true
        }
    }

    export!(FixtureChannel);
}
