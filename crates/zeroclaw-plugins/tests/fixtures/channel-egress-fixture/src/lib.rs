//! Minimal channel component that performs real `wasi:http` egress, used by the
//! host's channel egress-policy integration test.
//!
//! On `configure` the guest reads a `url` from its point-of-use resolved config
//! and issues **one** outbound `wasi:http` GET to it, recording the outcome. The
//! outcome is then surfaced through `self-handle`, which the host caches during
//! channel construction, so the test can read exactly what the guest saw:
//!
//! - `egress:status=200` — the request reached the destination, or
//! - `egress:error=<ErrorCode>` — the host-owned egress boundary denied it.
//!
//! The channel is always constructed regardless of the egress verdict; the only
//! thing that changes is whether the packet left the sandbox and what the guest
//! observed. That separation is what lets the test prove the policy gates reach,
//! not construction.

#[cfg(target_family = "wasm")]
mod component {
    wit_bindgen::generate!({
        path: "../../../../../wit/v0",
        world: "channel-plugin",
        features: ["plugins-wit-v0"],
    });

    use exports::zeroclaw::plugin::channel::{
        ApprovalRequest, ApprovalResponse, ChannelCapabilities, Guest as Channel, InboundMessage,
        SendMessage,
    };
    use exports::zeroclaw::plugin::plugin_info::Guest as PluginInfo;
    // Point-of-use config import. The channel WIT world's `configure` takes no
    // argument: the guest resolves its (non-secret) config here, the same host
    // service the channel-fixture reads, rather than receiving a plaintext
    // snapshot as a parameter.
    use zeroclaw::plugin::config::get as config_get;

    use std::sync::Mutex;

    use wasi::http::outgoing_handler;
    use wasi::http::types::{Fields, Method, OutgoingBody, OutgoingRequest, Scheme};

    /// The single guest-observed egress outcome, recorded in `configure` and read
    /// back in `self-handle`. Guest globals persist across calls within one
    /// instance, so the value written during construction is still here when the
    /// host asks for the channel's self handle.
    static OUTCOME: Mutex<Option<String>> = Mutex::new(None);

    struct EgressChannel;

    impl PluginInfo for EgressChannel {
        fn plugin_name() -> String {
            "channel-egress-fixture".to_string()
        }

        fn plugin_version() -> String {
            "0.0.0".to_string()
        }
    }

    impl Channel for EgressChannel {
        fn name() -> String {
            "channel-egress-fixture".to_string()
        }

        fn configure() -> Result<(), String> {
            // Resolve the `url` from point-of-use config. `url` is a non-secret
            // property in the manifest schema, so it arrives through the public
            // config frame the host serves during construction.
            let config = config_get().map_err(|_| "expected point-of-use config".to_string())?;
            let url = extract_string(&config, "url")
                .ok_or_else(|| "missing \"url\" in resolved config".to_string())?;
            let outcome = format!("egress:{}", request(&url));
            *OUTCOME.lock().unwrap() = Some(outcome);
            Ok(())
        }

        fn send(_message: SendMessage) -> Result<(), String> {
            Ok(())
        }

        fn poll_message() -> Option<InboundMessage> {
            None
        }

        fn get_channel_capabilities() -> ChannelCapabilities {
            ChannelCapabilities::HEALTH_CHECK | ChannelCapabilities::SELF_HANDLE
        }

        fn health_check() -> bool {
            true
        }

        fn self_handle() -> Option<String> {
            // Surface the egress outcome the host recorded during `configure`.
            OUTCOME
                .lock()
                .unwrap()
                .clone()
                .or_else(|| Some("egress:not-attempted".to_string()))
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
            0
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

    /// Issue exactly one `wasi:http` GET. Returns `status=<code>` when the host
    /// let the request out, or `error=<ErrorCode>` when the egress boundary
    /// denied it. Mirrors the tool egress fixture's single-hop path.
    fn request(url: &str) -> String {
        let Some((scheme, rest)) = split_scheme(url) else {
            return "error=unparsable-url".to_string();
        };
        let (authority, path) = match rest.find('/') {
            Some(idx) => (&rest[..idx], &rest[idx..]),
            None => (rest, "/"),
        };

        let outgoing = OutgoingRequest::new(Fields::new());
        if outgoing.set_method(&Method::Get).is_err()
            || outgoing.set_scheme(Some(&scheme)).is_err()
            || outgoing.set_authority(Some(authority)).is_err()
            || outgoing.set_path_with_query(Some(path)).is_err()
        {
            return "error=request-setup-failed".to_string();
        }

        let Ok(body) = outgoing.body() else {
            return "error=body-unavailable".to_string();
        };

        let future = match outgoing_handler::handle(outgoing, None) {
            Ok(future) => future,
            // The host's policy denial surfaces here, synchronously.
            Err(code) => return format!("error={code:?}"),
        };
        let _ = OutgoingBody::finish(body, None);

        future.subscribe().block();
        match future.get() {
            Some(Ok(Ok(response))) => format!("status={}", response.status()),
            // A denial raised after the handler accepted the request lands here.
            Some(Ok(Err(code))) => format!("error={code:?}"),
            Some(Err(())) => "error=future-already-taken".to_string(),
            None => "error=future-not-ready".to_string(),
        }
    }

    fn split_scheme(url: &str) -> Option<(Scheme, &str)> {
        if let Some(rest) = url.strip_prefix("http://") {
            Some((Scheme::Http, rest))
        } else {
            url.strip_prefix("https://")
                .map(|rest| (Scheme::Https, rest))
        }
    }

    /// Pull `"key":"value"` out of a flat JSON object. The host test controls the
    /// exact config text, so a full JSON parser would only add a dependency
    /// without adding coverage.
    fn extract_string(json: &str, key: &str) -> Option<String> {
        let needle = format!("\"{key}\"");
        let after_key = &json[json.find(&needle)? + needle.len()..];
        let after_colon = &after_key[after_key.find(':')? + 1..];
        let open = after_colon.find('"')?;
        let value = &after_colon[open + 1..];
        let close = value.find('"')?;
        Some(value[..close].to_string())
    }

    export!(EgressChannel);
}
