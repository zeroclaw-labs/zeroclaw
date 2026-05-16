//! Configuration module for WuKongIM channel.
//!
//! Provides re-export of `WuKongIMConfig` and configuration-related utilities.

pub use zeroclaw_config::schema::WuKongIMConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_fields() {
        let cfg = WuKongIMConfig {
            enabled: true,
            ws_url: "ws://localhost:5200".to_string(),
            uid: "bot".to_string(),
            token: "tok".to_string(),
            device_id: "web-001".to_string(),
            device_flag: 2,
            allowed_users: vec!["*".to_string()],
            mention_only: false,
            approval_timeout_secs: 300,
            downloads_dir: "downloads".to_string(),
            dawn_url: "".to_string(),
            dawn_token: "".to_string(),
            ack_reactions: true,
            ack_reactions_message: String::new(),
            ack_reactions_delay: 60,
            progress_streaming: false,
        };
        assert_eq!(cfg.device_id, "web-001");
        assert_eq!(cfg.device_flag, 2);
        assert!(!cfg.progress_streaming);
    }

    #[test]
    fn progress_streaming_defaults_false_when_missing_from_toml() {
        let toml_input = r#"
            enabled = true
            ws_url = "ws://localhost:5200"
            uid = "bot"
            token = "tok"
            device_id = "dev"
            device_flag = 2
        "#;
        let cfg: WuKongIMConfig = toml::from_str(toml_input).unwrap();
        assert!(!cfg.progress_streaming, "must default to false");
    }
}
