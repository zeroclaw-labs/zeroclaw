mod cloudflare;
mod custom;
mod ngrok;
mod none;
mod openvpn;
mod pinggy;
mod tailscale;

pub use cloudflare::CloudflareTunnel;
pub use custom::CustomTunnel;
pub use ngrok::NgrokTunnel;
#[allow(unused_imports)]
pub use none::NoneTunnel;
pub use openvpn::OpenVpnTunnel;
pub use pinggy::PinggyTunnel;
pub use tailscale::{
    TailnetSans, TailscaleSelf, TailscaleTunnel, is_tailscale_ip, parse_tailscale_self,
    query_tailscale_self, tailnet_sans_from_status, tailscale_server_sans,
};

use anyhow::{Result, bail};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use tokio::sync::Mutex;
use zeroclaw_config::schema::{Config, TailscaleTunnelConfig, TunnelConfig};

// ── Daemon TCP services ──────────────────────────────────────────

/// A local daemon listener that terminates its OWN TLS (the mutually
/// authenticated WSS RPC plane, the server-authenticated enrollment endpoint).
///
/// A tunnel must publish these as raw TCP passthrough: terminating TLS at the
/// tunnel would strip the client certificate the WSS plane requires and
/// replace the daemon certificate that enrollment's short-auth-string binds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpService {
    /// Stable service label for logs and endpoint display (`wss`, `enroll`).
    pub name: &'static str,
    /// URL scheme a client uses to reach the service (`wss`, `https`).
    pub scheme: &'static str,
    /// Local address the tunnel forwards to.
    pub target: SocketAddr,
}

/// A [`TcpService`] the tunnel actually published, with the endpoint clients use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedTcpService {
    pub service: TcpService,
    pub endpoint: String,
}

/// Resolve the daemon's enabled self-TLS listeners from live config.
///
/// Enrollment is only listed when the daemon actually runs it: alongside WSS
/// (it refuses to start without `[wss]`) and not under a bring-your-own
/// client CA, where it holds no signing key and parks the endpoint. A bind address that does not parse as an IP is
/// skipped: the listener itself refuses such a bind, so there is nothing to
/// forward to.
pub fn daemon_tcp_services(config: &Config) -> Vec<TcpService> {
    let mut services = Vec::new();
    if !config.wss.enabled {
        return services;
    }
    if let Some(target) = local_forward_target(&config.wss.bind, config.wss.port) {
        services.push(TcpService {
            name: "wss",
            scheme: "wss",
            target,
        });
    }
    if config.enroll.enabled
        && config.wss.external_client_ca().is_none()
        && let Some(target) = local_forward_target(&config.enroll.bind, config.enroll.port)
    {
        services.push(TcpService {
            name: "enroll",
            scheme: "https",
            target,
        });
    }
    services
}

/// The address a same-host forwarder should dial for a listener bound to
/// `bind:port`. A wildcard bind is reached on the loopback of its family;
/// a specific bind is reached on that address.
fn local_forward_target(bind: &str, port: u16) -> Option<SocketAddr> {
    let ip: IpAddr = bind.trim().parse().ok()?;
    let ip = match ip {
        IpAddr::V4(v4) if v4.is_unspecified() => IpAddr::V4(Ipv4Addr::LOCALHOST),
        IpAddr::V6(v6) if v6.is_unspecified() => IpAddr::V6(Ipv6Addr::LOCALHOST),
        other => other,
    };
    Some(SocketAddr::new(ip, port))
}

// ── Tunnel trait ─────────────────────────────────────────────────

#[async_trait::async_trait]
pub trait Tunnel: Send + Sync {
    /// Human-readable model_provider name (e.g. "cloudflare", "tailscale")
    fn name(&self) -> &str;

    /// Start the tunnel, exposing `local_host:local_port` externally.
    /// Returns the public URL on success.
    async fn start(&self, local_host: &str, local_port: u16) -> Result<String>;

    /// Stop the tunnel, ending the processes it started and withdrawing what it
    /// published. The gateway calls this when it shuts down.
    async fn stop(&self) -> Result<()>;

    /// Check if the tunnel is still alive.
    async fn health_check(&self) -> bool;

    /// Return the public URL if the tunnel is running.
    fn public_url(&self) -> Option<String>;

    /// Publish daemon listeners that terminate their own TLS as raw TCP
    /// passthrough, after `start`. Returns only the services actually
    /// published. Providers without raw TCP passthrough publish nothing,
    /// which keeps those listeners at their configured bind.
    async fn publish_tcp_services(
        &self,
        _services: &[TcpService],
    ) -> Result<Vec<PublishedTcpService>> {
        Ok(Vec::new())
    }
}

// ── Shared child-process handle ──────────────────────────────────

/// Wraps a spawned tunnel child process so implementations can share it.
pub struct TunnelProcess {
    pub child: tokio::process::Child,
    pub public_url: String,
}

pub type SharedProcess = Arc<Mutex<Option<TunnelProcess>>>;

pub fn new_shared_process() -> SharedProcess {
    Arc::new(Mutex::new(None))
}

/// Kill a shared tunnel process if running.
pub async fn kill_shared(proc: &SharedProcess) -> Result<()> {
    let mut guard = proc.lock().await;
    if let Some(ref mut tp) = *guard {
        tp.child.kill().await.ok();
        tp.child.wait().await.ok();
    }
    *guard = None;
    Ok(())
}

// ── Factory ──────────────────────────────────────────────────────

/// Create a tunnel from config. Returns `None` for tunnel_provider "none".
pub fn create_tunnel(config: &TunnelConfig) -> Result<Option<Box<dyn Tunnel>>> {
    match config.tunnel_provider.as_str() {
        "none" | "" => Ok(None),

        "cloudflare" => {
            let cf = config.cloudflare.as_ref().ok_or_else(|| {
                {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"tunnel_provider": "cloudflare"})),
                    "tunnel create refused: provider selected but config block missing"
                );
                anyhow::Error::msg(
                    "tunnel.tunnel_provider = \"cloudflare\" but [tunnel.cloudflare] section is missing"
                )
            }
            })?;
            Ok(Some(Box::new(CloudflareTunnel::new(cf.token.clone()))))
        }

        "tailscale" => {
            let ts = config.tailscale.as_ref().unwrap_or(&TailscaleTunnelConfig {
                funnel: false,
                hostname: None,
            });
            Ok(Some(Box::new(TailscaleTunnel::new(
                ts.funnel,
                ts.hostname.clone(),
            ))))
        }

        "ngrok" => {
            let ng = config.ngrok.as_ref().ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"tunnel_provider": "ngrok"})),
                    "tunnel create refused: provider selected but config block missing"
                );
                anyhow::Error::msg(
                    "tunnel.tunnel_provider = \"ngrok\" but [tunnel.ngrok] section is missing",
                )
            })?;
            Ok(Some(Box::new(NgrokTunnel::new(
                ng.auth_token.clone(),
                ng.domain.clone(),
            ))))
        }

        "openvpn" => {
            let ov = config.openvpn.as_ref().ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"tunnel_provider": "openvpn"})),
                    "tunnel create refused: provider selected but config block missing"
                );
                anyhow::Error::msg(
                    "tunnel.tunnel_provider = \"openvpn\" but [tunnel.openvpn] section is missing",
                )
            })?;
            Ok(Some(Box::new(OpenVpnTunnel::new(
                ov.config_file.clone(),
                ov.auth_file.clone(),
                ov.advertise_address.clone(),
                ov.connect_timeout_secs,
                ov.extra_args.clone(),
            ))))
        }

        "custom" => {
            let cu = config.custom.as_ref().ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"tunnel_provider": "custom"})),
                    "tunnel create refused: provider selected but config block missing"
                );
                anyhow::Error::msg(
                    "tunnel.tunnel_provider = \"custom\" but [tunnel.custom] section is missing",
                )
            })?;
            Ok(Some(Box::new(CustomTunnel::new(
                cu.start_command.clone(),
                cu.health_url.clone(),
                cu.url_pattern.clone(),
            ))))
        }

        "pinggy" => {
            let pg = config.pinggy.as_ref().ok_or_else(|| {
                ::zeroclaw_log::record!(
                    ERROR,
                    ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Reject)
                        .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                        .with_attrs(::serde_json::json!({"tunnel_provider": "pinggy"})),
                    "tunnel create refused: provider selected but config block missing"
                );
                anyhow::Error::msg(
                    "tunnel.tunnel_provider = \"pinggy\" but [tunnel.pinggy] section is missing",
                )
            })?;
            Ok(Some(Box::new(PinggyTunnel::new(
                pg.token.clone(),
                pg.region.clone(),
            ))))
        }

        other => bail!(
            "Unknown tunnel_provider: \"{other}\". Valid: none, cloudflare, tailscale, ngrok, openvpn, pinggy, custom"
        ),
    }
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::process::Command;
    use zeroclaw_config::schema::{
        CloudflareTunnelConfig, CustomTunnelConfig, NgrokTunnelConfig, OpenVpnTunnelConfig,
        PinggyTunnelConfig, TunnelConfig,
    };

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
            tunnel_provider: String::new(),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_none());
    }

    #[test]
    fn factory_unknown_provider_errors() {
        let cfg = TunnelConfig {
            tunnel_provider: "wireguard".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "Unknown tunnel_provider");
    }

    #[test]
    fn factory_cloudflare_missing_config_errors() {
        let cfg = TunnelConfig {
            tunnel_provider: "cloudflare".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.cloudflare]");
    }

    #[test]
    fn factory_cloudflare_with_config_ok() {
        let cfg = TunnelConfig {
            tunnel_provider: "cloudflare".into(),
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
            tunnel_provider: "tailscale".into(),
            ..TunnelConfig::default()
        };
        let t = create_tunnel(&cfg).unwrap();
        assert!(t.is_some());
        assert_eq!(t.unwrap().name(), "tailscale");
    }

    #[test]
    fn factory_ngrok_missing_config_errors() {
        let cfg = TunnelConfig {
            tunnel_provider: "ngrok".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.ngrok]");
    }

    #[test]
    fn factory_ngrok_with_config_ok() {
        let cfg = TunnelConfig {
            tunnel_provider: "ngrok".into(),
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
            tunnel_provider: "custom".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.custom]");
    }

    #[test]
    fn factory_custom_with_config_ok() {
        let cfg = TunnelConfig {
            tunnel_provider: "custom".into(),
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
            tunnel_provider: "pinggy".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.pinggy]");
    }

    #[test]
    fn factory_pinggy_with_config_ok() {
        let cfg = TunnelConfig {
            tunnel_provider: "pinggy".into(),
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
            tunnel_provider: "openvpn".into(),
            ..TunnelConfig::default()
        };
        assert_tunnel_err(&cfg, "[tunnel.openvpn]");
    }

    #[test]
    fn factory_openvpn_with_config_ok() {
        let cfg = TunnelConfig {
            tunnel_provider: "openvpn".into(),
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

        let child = Command::new("sleep")
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("sleep should spawn for lifecycle test");

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

    #[test]
    fn local_forward_target_maps_wildcards_to_loopback() {
        assert_eq!(
            local_forward_target("0.0.0.0", 9781),
            Some("127.0.0.1:9781".parse().unwrap())
        );
        assert_eq!(
            local_forward_target("::", 9781),
            Some("[::1]:9781".parse().unwrap())
        );
    }

    #[test]
    fn local_forward_target_keeps_specific_binds() {
        assert_eq!(
            local_forward_target("127.0.0.1", 9782),
            Some("127.0.0.1:9782".parse().unwrap())
        );
        assert_eq!(
            local_forward_target(" 192.168.2.10 ", 9782),
            Some("192.168.2.10:9782".parse().unwrap())
        );
    }

    #[test]
    fn local_forward_target_rejects_non_ip_binds() {
        assert_eq!(local_forward_target("localhost", 9781), None);
        assert_eq!(local_forward_target("", 9781), None);
    }

    #[test]
    fn daemon_tcp_services_empty_by_default() {
        assert!(daemon_tcp_services(&Config::default()).is_empty());
    }

    #[test]
    fn daemon_tcp_services_lists_wss_and_enroll_from_config() {
        let mut config = Config::default();
        config.wss.enabled = true;
        config.wss.bind = "127.0.0.1".into();
        config.wss.port = 19781;
        config.enroll.enabled = true;
        config.enroll.bind = "0.0.0.0".into();
        config.enroll.port = 19782;

        assert_eq!(
            daemon_tcp_services(&config),
            vec![
                TcpService {
                    name: "wss",
                    scheme: "wss",
                    target: "127.0.0.1:19781".parse().unwrap(),
                },
                TcpService {
                    name: "enroll",
                    scheme: "https",
                    target: "127.0.0.1:19782".parse().unwrap(),
                },
            ]
        );
    }

    #[test]
    fn daemon_tcp_services_skips_enroll_without_wss() {
        // The daemon refuses to run enrollment without [wss]; there is no
        // listener to forward to.
        let mut config = Config::default();
        config.enroll.enabled = true;
        assert!(daemon_tcp_services(&config).is_empty());
    }

    #[test]
    fn daemon_tcp_services_skips_enroll_under_external_client_ca() {
        // With a bring-your-own client CA the daemon parks enrollment, so it
        // must not be published or announced.
        let mut config = Config::default();
        config.wss.enabled = true;
        config.enroll.enabled = true;
        // `EnrollConfig::default()` has an empty bind (serde supplies the real
        // default); set it so enrollment is only excluded by the CA rule.
        config.enroll.bind = "127.0.0.1".into();
        config.enroll.port = 9782;
        assert_eq!(
            daemon_tcp_services(&config).len(),
            2,
            "precondition: enrollment is publishable without an external CA"
        );
        config.wss.client_auth = Some(zeroclaw_config::schema::WssClientAuthConfig {
            enabled: true,
            ca_cert_path: "/etc/zeroclaw/client-ca.pem".into(),
            ..Default::default()
        });
        let services = daemon_tcp_services(&config);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "wss");

        // A CA path with client auth disabled is not an external CA (the WSS
        // starter rejects that combination); enrollment stays listed.
        config.wss.client_auth.as_mut().unwrap().enabled = false;
        assert_eq!(daemon_tcp_services(&config).len(), 2);
    }

    #[test]
    fn daemon_tcp_services_wss_only_when_enroll_disabled() {
        let mut config = Config::default();
        config.wss.enabled = true;
        let services = daemon_tcp_services(&config);
        assert_eq!(services.len(), 1);
        assert_eq!(services[0].name, "wss");
    }

    #[tokio::test]
    async fn providers_without_tcp_passthrough_publish_nothing() {
        let services = [TcpService {
            name: "wss",
            scheme: "wss",
            target: "127.0.0.1:9781".parse().unwrap(),
        }];
        let tunnel = NgrokTunnel::new("tok".into(), None);
        assert!(
            tunnel
                .publish_tcp_services(&services)
                .await
                .unwrap()
                .is_empty()
        );
    }
}
