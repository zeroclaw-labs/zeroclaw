use zeroclaw_config::schema::{Config, MdnsConfig, NodesConfig};

#[test]
fn mdns_config_defaults_to_disabled_without_gateway_port_copy() {
    let cfg = NodesConfig::default();

    assert!(!cfg.mdns.enabled);
    assert_eq!(cfg.mdns.node_name, None);
    assert_eq!(cfg.mdns.announce_interval_secs, 30);
    assert_eq!(cfg.mdns.max_peers, 16);
    assert_eq!(cfg.mdns.peer_ttl_secs, 90);

    let value = serde_json::to_value(&cfg.mdns).unwrap();
    assert!(
        value.get("port").is_none(),
        "mDNS config must not duplicate gateway.port; advertise the runtime gateway port instead"
    );
}

#[test]
fn mdns_config_deserializes_under_nodes() {
    let cfg: NodesConfig = toml::from_str(
        r#"
enabled = true

[mdns]
enabled = true
node_name = "lab-node"
max_peers = 24
announce_interval_secs = 15
peer_ttl_secs = 60
"#,
    )
    .unwrap();

    assert!(cfg.enabled);
    assert!(cfg.mdns.enabled);
    assert_eq!(cfg.mdns.node_name.as_deref(), Some("lab-node"));
    assert_eq!(cfg.mdns.announce_interval_secs, 15);
    assert_eq!(cfg.mdns.max_peers, 24);
    assert_eq!(cfg.mdns.peer_ttl_secs, 60);
}

#[test]
fn mdns_config_rejects_zero_intervals() {
    let mut cfg = Config::default();
    cfg.nodes.mdns = MdnsConfig {
        enabled: false,
        announce_interval_secs: 0,
        peer_ttl_secs: 90,
        ..MdnsConfig::default()
    };

    let err = cfg
        .validate()
        .expect_err("enabled mDNS must reject zero announce interval");
    assert!(
        err.to_string()
            .contains("nodes.mdns.announce_interval_secs")
    );
}

#[test]
fn mdns_config_rejects_zero_peer_capacity() {
    let mut cfg = Config::default();
    cfg.nodes.mdns = MdnsConfig {
        max_peers: 0,
        ..MdnsConfig::default()
    };

    let err = cfg
        .validate()
        .expect_err("mDNS peer capacity must be greater than zero");
    assert!(err.to_string().contains("nodes.mdns.max_peers"));
}

#[test]
fn mdns_config_rejects_ttl_at_or_below_announce_interval() {
    let mut cfg = Config::default();
    cfg.nodes.mdns = MdnsConfig {
        announce_interval_secs: 30,
        peer_ttl_secs: 30,
        ..MdnsConfig::default()
    };

    let err = cfg
        .validate()
        .expect_err("mDNS peer TTL must exceed announce interval");
    assert!(err.to_string().contains("nodes.mdns.peer_ttl_secs"));
}
