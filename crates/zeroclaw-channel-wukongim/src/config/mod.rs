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
        };
        assert_eq!(cfg.device_id, "web-001");
        assert_eq!(cfg.device_flag, 2);
    }
}
