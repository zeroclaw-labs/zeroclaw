use std::time::{Duration, Instant};

use zeroclaw_gateway::nodes::mdns::{
    Announcement, Bye, MdnsAdvertisedGateway, MdnsPeer, MdnsPeerRegistry, PeerPacket,
    evict_stale_peers, handle_datagram, is_advertisable_gateway_addr,
};

#[test]
fn advertised_gateway_uses_runtime_port_and_path_prefix() {
    let gateway = MdnsAdvertisedGateway::new(42617, Some("/zeroclaw"));

    let packet = gateway.announcement("lab-node", "0.8.1");

    assert_eq!(packet.id, "");
    assert_eq!(packet.name, "lab-node");
    assert_eq!(packet.port, 42617);
    assert_eq!(packet.path_prefix.as_deref(), Some("/zeroclaw"));

    let peer = MdnsPeer {
        name: "peer-a".into(),
        addr: "192.168.1.42".into(),
        port: packet.port,
        version: packet.version,
        path_prefix: packet.path_prefix,
        last_seen: Instant::now(),
    };
    assert_eq!(peer.base_url(), "http://192.168.1.42:42617/zeroclaw");
}

#[test]
fn advertisable_gateway_addr_rejects_loopback_only_binds() {
    assert!(!is_advertisable_gateway_addr(
        &"127.0.0.1:42617".parse().unwrap()
    ));
    assert!(is_advertisable_gateway_addr(
        &"0.0.0.0:42617".parse().unwrap()
    ));
}

#[test]
fn handle_announce_fills_source_address_and_ignores_self_id() {
    let registry = MdnsPeerRegistry::default();
    let packet = PeerPacket::Announce(Announcement {
        id: "peer-a-id".into(),
        name: "peer-a".into(),
        addr: String::new(),
        port: 42617,
        version: "0.8.1".into(),
        path_prefix: Some("/zc".into()),
    });
    let data = serde_json::to_vec(&packet).unwrap();

    handle_datagram(&data, "10.0.0.2", &registry, "me-id");
    handle_datagram(&data, "10.0.0.2", &registry, "peer-a-id");

    let peers = registry.snapshots();
    assert_eq!(peers.len(), 1);
    let peer = &peers[0];
    assert_eq!(peer.id, "peer-a-id");
    assert_eq!(peer.name, "peer-a");
    assert_eq!(peer.addr, "10.0.0.2");
    assert_eq!(peer.port, 42617);
    assert_eq!(peer.path_prefix.as_deref(), Some("/zc"));
    assert_eq!(peer.base_url, "http://10.0.0.2:42617/zc");
}

#[test]
fn duplicate_display_names_do_not_overwrite_peer_identity() {
    let registry = MdnsPeerRegistry::default();
    for id in ["peer-1", "peer-2"] {
        let packet = PeerPacket::Announce(Announcement {
            id: id.into(),
            name: "lab-node".into(),
            addr: String::new(),
            port: 42617,
            version: "0.8.1".into(),
            path_prefix: None,
        });
        handle_datagram(
            &serde_json::to_vec(&packet).unwrap(),
            "10.0.0.2",
            &registry,
            "me-id",
        );
    }

    let snapshots = registry.snapshots();
    assert_eq!(snapshots.len(), 2);
    assert_eq!(snapshots[0].id, "peer-1");
    assert_eq!(snapshots[1].id, "peer-2");
    assert_eq!(snapshots[0].name, "lab-node");
    assert_eq!(snapshots[0].base_url, "http://10.0.0.2:42617");
}

#[test]
fn invalid_announcements_are_ignored() {
    let registry = MdnsPeerRegistry::default();
    for packet in [
        PeerPacket::Announce(Announcement {
            id: "".into(),
            name: "peer".into(),
            addr: String::new(),
            port: 42617,
            version: "0.8.1".into(),
            path_prefix: None,
        }),
        PeerPacket::Announce(Announcement {
            id: "peer-empty-name".into(),
            name: " ".into(),
            addr: String::new(),
            port: 42617,
            version: "0.8.1".into(),
            path_prefix: None,
        }),
        PeerPacket::Announce(Announcement {
            id: "peer-zero-port".into(),
            name: "peer".into(),
            addr: String::new(),
            port: 0,
            version: "0.8.1".into(),
            path_prefix: None,
        }),
        PeerPacket::Announce(Announcement {
            id: "peer-bad-prefix".into(),
            name: "peer".into(),
            addr: String::new(),
            port: 42617,
            version: "0.8.1".into(),
            path_prefix: Some("not/absolute".into()),
        }),
    ] {
        handle_datagram(
            &serde_json::to_vec(&packet).unwrap(),
            "10.0.0.2",
            &registry,
            "me-id",
        );
    }

    assert!(registry.snapshots().is_empty());
}

#[test]
fn bye_removes_peer_by_id_and_ttl_evicts_stale_entries() {
    let registry = MdnsPeerRegistry::default();
    registry.insert(
        "peer-b-id".into(),
        MdnsPeer {
            name: "peer-b".into(),
            addr: "10.0.0.3".into(),
            port: 42617,
            version: "0.8.1".into(),
            path_prefix: None,
            last_seen: Instant::now(),
        },
    );
    registry.insert(
        "stale-id".into(),
        MdnsPeer {
            name: "stale".into(),
            addr: "10.0.0.4".into(),
            port: 42617,
            version: "0.8.1".into(),
            path_prefix: None,
            last_seen: Instant::now() - Duration::from_secs(120),
        },
    );

    let bye = PeerPacket::Bye(Bye {
        id: "peer-b-id".into(),
    });
    handle_datagram(
        &serde_json::to_vec(&bye).unwrap(),
        "10.0.0.3",
        &registry,
        "me",
    );
    evict_stale_peers(&registry, Duration::from_secs(90));

    assert!(registry.snapshots().is_empty());
}
