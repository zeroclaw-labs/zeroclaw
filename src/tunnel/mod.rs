#[allow(unused_imports)]
pub use zeroclaw_runtime::tunnel::*;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::schema::{
        CloudflareTunnelConfig, CustomTunnelConfig, NgrokTunnelConfig, OpenVpnTunnelConfig,
        PinggyTunnelConfig, TunnelConfig,
    };
    use tokio::process::Command;

    fn spawn_lifecycle_test_child() -> std::io::Result<tokio::process::Child> {
        #[cfg(windows)]
        {
            Command::new("cmd")
                .args(["/C", "ping 127.0.0.1 -n 30 >NUL"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        }

        #[cfg(not(windows))]
        {
            Command::new("sleep")
                .arg("30")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
        }
    }

    /// Helper: assert `create_tunnel` returns an error containing `needle`.
    fn assert_tunnel_err(cfg: &TunnelConfig, needle: &str) {
        match create_tunnel(cfg) {
            Err(e) => assert!(
                e.to_string().contains(needle),
                "Expected error containing \"{needle}\", got: {e}"
            ),
            Ok(_) => panic!("Expected error containing \"{needle}\", but got Ok"),
        }
    }

    #[test]
    fn factory_none_returns_none() {
        let cfg = TunnelConfig::default();
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_none());
    }

    #[test]
    fn factory_empty_string_returns_none() {
        let cfg = TunnelConfig {
            provider: String::new(),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_none());
    }

    #[test]
    fn factory_unknown_provider_errors() {
        let cfg = TunnelConfig {
            provider: "wireguard".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "Unknown tunnel provider");
    }

    #[test]
    fn factory_cloudflare_missing_config_errors() {
        let cfg = TunnelConfig {
            provider: "cloudflare".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.cloudflare]");
    }

    #[test]
    fn factory_cloudflare_with_config_ok() {
        let cfg = TunnelConfig {
            provider: "cloudflare".into(),
            cloudflare: Some(CloudflareTunnelConfig {
                token: "test-token".into(),
            }),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_some());
        assert_eq!(t.unwrap().name(), "cloudflare");
    }

    #[test]
    fn factory_tailscale_defaults_ok() {
        let cfg = TunnelConfig {
            provider: "tailscale".into(),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_some());
        assert_eq!(t.unwrap().name(), "tailscale");
    }

    #[test]
    fn factory_ngrok_missing_config_errors() {
        let cfg = TunnelConfig {
            provider: "ngrok".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.ngrok]");
    }

    #[test]
    fn factory_ngrok_with_config_ok() {
        let cfg = TunnelConfig {
            provider: "ngrok".into(),
            ngrok: Some(NgrokTunnelConfig {
                auth_token: "tok".into(),
                domain: None,
            }),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_some());
        assert_eq!(t.unwrap().name(), "ngrok");
    }

    #[test]
    fn factory_custom_missing_config_errors() {
        let cfg = TunnelConfig {
            provider: "custom".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.custom]");
    }

    #[test]
    fn factory_custom_with_config_ok() {
        let cfg = TunnelConfig {
            provider: "custom".into(),
            custom: Some(CustomTunnelConfig {
                start_command: "echo tunnel".into(),
                health_url: None,
                url_pattern: None,
            }),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_some());
        assert_eq!(t.unwrap().name(), "custom");
    }

    #[test]
    fn factory_pinggy_missing_config_errors() {
        let cfg = TunnelConfig {
            provider: "pinggy".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.pinggy]");
    }

    #[test]
    fn factory_pinggy_with_config_ok() {
        let cfg = TunnelConfig {
            provider: "pinggy".into(),
            pinggy: Some(PinggyTunnelConfig {
                token: Some("tok".into()),
                region: None,
            }),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_some());
        assert_eq!(t.unwrap().name(), "pinggy");
    }

    #[test]
    fn none_tunnel_name() {
        let t = NoneTunnel;
        assert_eq!(t.name(), "none");
    }

    #[test]
    fn none_tunnel_public_url_is_none() {
        let t = NoneTunnel;
        assert!(t.public_url().is_none());
    }

    #[tokio::test]
    async fn none_tunnel_health_always_true() {
        let t = NoneTunnel;
        assert!(t.health_check().await);
    }

    #[tokio::test]
    async fn none_tunnel_start_returns_local() {
        let t = NoneTunnel;
        let url = t.start("127.0.0.1", 8080).await.unwrap();
        assert_eq!(url, "http://127.0.0.1:8080");
    }

    #[test]
    fn cloudflare_tunnel_name() {
        let t = CloudflareTunnel::new("tok".into());
        assert_eq!(t.name(), "cloudflare");
        assert!(t.public_url().is_none());
    }

    #[test]
    fn tailscale_tunnel_name() {
        let t = TailscaleTunnel::new(false, None);
        assert_eq!(t.name(), "tailscale");
        assert!(t.public_url().is_none());
    }

    #[test]
    fn tailscale_funnel_mode() {
        let t = TailscaleTunnel::new(true, Some("myhost".into()));
        assert_eq!(t.name(), "tailscale");
    }

    #[test]
    fn ngrok_tunnel_name() {
        let t = NgrokTunnel::new("tok".into(), None);
        assert_eq!(t.name(), "ngrok");
        assert!(t.public_url().is_none());
    }

    #[test]
    fn ngrok_with_domain() {
        let t = NgrokTunnel::new("tok".into(), Some("my.ngrok.io".into()));
        assert_eq!(t.name(), "ngrok");
    }

    #[test]
    fn custom_tunnel_name() {
        let t = CustomTunnel::new("echo hi".into(), None, None);
        assert_eq!(t.name(), "custom");
        assert!(t.public_url().is_none());
    }

    #[test]
    fn factory_openvpn_missing_config_errors() {
        let cfg = TunnelConfig {
            provider: "openvpn".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.openvpn]");
    }

    #[test]
    fn factory_openvpn_with_config_ok() {
        let cfg = TunnelConfig {
            provider: "openvpn".into(),
            openvpn: Some(OpenVpnTunnelConfig {
                config_file: "client.ovpn".into(),
                auth_file: None,
                advertise_address: None,
                connect_timeout_secs: 30,
                extra_args: vec![],
            }),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_some());
        assert_eq!(t.unwrap().name(), "openvpn");
    }

    #[test]
    fn openvpn_tunnel_name() {
        let t = OpenVpnTunnel::new("client.ovpn".into(), None, None, 30, vec![]);
        assert_eq!(t.name(), "openvpn");
        assert!(t.public_url().is_none());
    }

    #[tokio::test]
    async fn openvpn_health_false_before_start() {
        let tunnel = OpenVpnTunnel::new("client.ovpn".into(), None, None, 30, vec![]);
        assert!(!tunnel.health_check().await);
    }

    #[tokio::test]
    async fn kill_shared_no_process_is_ok() {
        let proc = new_shared_process();
        let result = kill_shared(&proc).await;

        assert!(result.is_ok());
        assert!(proc.lock().await.is_none());
    }

    #[tokio::test]
    async fn kill_shared_terminates_and_clears_child() {
        let proc = new_shared_process();

        let child = spawn_lifecycle_test_child()
            .expect("platform lifecycle command should spawn for tunnel test");

        {
            let mut guard = proc.lock().await;
            *guard = Some(TunnelProcess {
                child,
                public_url: "https://example.test".into(),
            });
        }

        kill_shared(&proc).await.unwrap();

        let guard = proc.lock().await;
        assert!(guard.is_none());
    }

    #[tokio::test]
    async fn cloudflare_health_false_before_start() {
        let tunnel = CloudflareTunnel::new("tok".into());
        assert!(!tunnel.health_check().await);
    }

    #[tokio::test]
    async fn ngrok_health_false_before_start() {
        let tunnel = NgrokTunnel::new("tok".into(), None);
        assert!(!tunnel.health_check().await);
    }

    #[tokio::test]
    async fn tailscale_health_false_before_start() {
        let tunnel = TailscaleTunnel::new(false, None);
        assert!(!tunnel.health_check().await);
    }

    #[tokio::test]
    async fn custom_health_false_before_start_without_health_url() {
        let tunnel = CustomTunnel::new("echo hi".into(), None, Some("https://".into()));
        assert!(!tunnel.health_check().await);
    }

    #[test]
    fn pinggy_tunnel_name() {
        let t = PinggyTunnel::new(Some("tok".into()), None);
        assert_eq!(t.name(), "pinggy");
        assert!(t.public_url().is_none());
    }

    #[test]
    fn pinggy_without_token() {
        let t = PinggyTunnel::new(None, None);
        assert_eq!(t.name(), "pinggy");
    }

    #[tokio::test]
    async fn pinggy_health_false_before_start() {
        let tunnel = PinggyTunnel::new(None, None);
        assert!(!tunnel.health_check().await);
    }
}
