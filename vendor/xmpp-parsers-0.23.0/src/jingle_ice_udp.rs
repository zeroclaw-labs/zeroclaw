// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use core::net::IpAddr;

use xso::{AsXml, FromXml};

use crate::jingle_dtls_srtp::Fingerprint;
use crate::ns;

/// Wrapper element for an ICE-UDP transport.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone, Default)]
#[xml(namespace = ns::JINGLE_ICE_UDP, name = "transport")]
pub struct Transport {
    /// A Password as defined in ICE-CORE.
    #[xml(attribute(default))]
    pwd: Option<String>,

    /// A User Fragment as defined in ICE-CORE.
    #[xml(attribute(default))]
    ufrag: Option<String>,

    /// List of candidates for this ICE-UDP session.
    #[xml(child(n = ..))]
    candidates: Vec<Candidate>,

    /// Fingerprint of the key used for the DTLS handshake.
    #[xml(child(default))]
    fingerprint: Option<Fingerprint>,
}

impl Transport {
    /// Create a new ICE-UDP transport.
    pub fn new() -> Transport {
        Transport::default()
    }

    /// Add a candidate to this transport.
    pub fn add_candidate(mut self, candidate: Candidate) -> Self {
        self.candidates.push(candidate);
        self
    }

    /// Set the DTLS-SRTP fingerprint of this transport.
    pub fn with_fingerprint(mut self, fingerprint: Fingerprint) -> Self {
        self.fingerprint = Some(fingerprint);
        self
    }
}

generate_attribute!(
    /// A Candidate Type as defined in ICE-CORE.
    Type, "type", {
        /// Host candidate.
        Host => "host",

        /// Peer reflexive candidate.
        Prflx => "prflx",

        /// Relayed candidate.
        Relay => "relay",

        /// Server reflexive candidate.
        Srflx => "srflx",
    }
);

/// A candidate for an ICE-UDP session.
#[derive(FromXml, AsXml, Debug, PartialEq, Clone)]
#[xml(namespace = ns::JINGLE_ICE_UDP, name = "candidate")]
pub struct Candidate {
    /// A Component ID as defined in ICE-CORE.
    #[xml(attribute)]
    pub component: u8,

    /// A Foundation as defined in ICE-CORE.
    #[xml(attribute)]
    pub foundation: String,

    /// An index, starting at 0, that enables the parties to keep track of updates to the
    /// candidate throughout the life of the session.
    #[xml(attribute)]
    pub generation: u8,

    /// A unique identifier for the candidate.
    #[xml(attribute)]
    pub id: String,

    /// The Internet Protocol (IP) address for the candidate transport mechanism; this can be
    /// either an IPv4 address or an IPv6 address.
    #[xml(attribute)]
    pub ip: IpAddr,

    /// The port at the candidate IP address.
    #[xml(attribute)]
    pub port: u16,

    /// A Priority as defined in ICE-CORE.
    #[xml(attribute)]
    pub priority: u32,

    /// The protocol to be used. The only value defined by this specification is "udp".
    #[xml(attribute)]
    pub protocol: String,

    /// A related address as defined in ICE-CORE.
    #[xml(attribute(default, name = "rel-addr"))]
    pub rel_addr: Option<IpAddr>,

    /// A related port as defined in ICE-CORE.
    #[xml(attribute(default, name = "rel-port"))]
    pub rel_port: Option<u16>,

    /// An index, starting at 0, referencing which network this candidate is on for a given
    /// peer.
    #[xml(attribute(default))]
    pub network: Option<u8>,

    /// A Candidate Type as defined in ICE-CORE.
    #[xml(attribute(name = "type"))]
    pub type_: Type,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hashes::Algo;
    use crate::jingle_dtls_srtp::Setup;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Transport, 64);
        assert_size!(Type, 1);
        assert_size!(Candidate, 88);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Transport, 128);
        assert_size!(Type, 1);
        assert_size!(Candidate, 128);
    }

    #[test]
    fn test_gajim() {
        let elem: Element = "<transport xmlns='urn:xmpp:jingle:transports:ice-udp:1' pwd='wakMJ8Ydd5rqnPaFerws5o' ufrag='aeXX'>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='11b72719-6a1b-4c51-8ae6-9f1538047568' ip='192.168.0.12' network='0' port='56715' priority='1010828030' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='7e07b22d-db50-4e17-9ed9-eafeb96f4f63' ip='192.168.0.12' network='0' port='0' priority='1015022334' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='431de362-c45f-40a8-bf10-9ed898a71d86' ip='192.168.0.12' network='0' port='36480' priority='2013266428' protocol='udp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='b1197df3-abca-413b-99ee-3660d91bcfa7' ip='192.168.0.12' network='0' port='50387' priority='1010828031' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='adaf3a85-3a57-4df0-a2d8-0c7d28d3ca01' ip='192.168.0.12' network='0' port='0' priority='1015022335' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='ef4e0a62-81f2-4fe3-87ae-46cb5d1d1e1d' ip='192.168.0.12' network='0' port='43132' priority='2013266429' protocol='udp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='51891e8a-4c1e-4540-b173-8637aeb0143c' ip='fe80::24eb:646f:7d78:cb6' network='0' port='38881' priority='2013266431' protocol='udp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='73f82655-eb84-4fa1-b05c-1ea76f695d32' ip='fe80::24eb:646f:7d78:cb6' network='0' port='0' priority='1015023103' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='a2a8fa62-6f2e-41e8-b218-ba095540d60f' ip='fe80::24eb:646f:7d78:cb6' network='0' port='55819' priority='1010828799' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='23e66735-9515-414c-81ad-2455569a57f8' ip='2a01:e35:2e2f:fbb0:43aa:33b5:5535:8905' network='0' port='39967' priority='2013266430' protocol='udp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='9a8dff18-e138-4fb2-a956-89d71216da84' ip='2a01:e35:2e2f:fbb0:43aa:33b5:5535:8905' network='0' port='0' priority='1015022079' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='1' foundation='1' generation='0' id='f0c73ac3-9b7d-4032-abe3-6dd9a57d0f03' ip='2a01:e35:2e2f:fbb0:43aa:33b5:5535:8905' network='0' port='37487' priority='1010827775' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='a6199a00-34df-46f5-a608-847b75c5250e' ip='fe80::24eb:646f:7d78:cb6' network='0' port='43521' priority='2013266430' protocol='udp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='83bc2600-39ce-4c9e-8b0b-cc7aa7e6a293' ip='fe80::24eb:646f:7d78:cb6' network='0' port='0' priority='1015023102' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='7e3606ca-46de-4de8-8802-068dd69ef01a' ip='fe80::24eb:646f:7d78:cb6' network='0' port='52279' priority='1010828798' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='a7c2472a-8462-412c-a64c-d3528f0abfa4' ip='2a01:e35:2e2f:fbb0:43aa:33b5:5535:8905' network='0' port='34088' priority='2013266429' protocol='udp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='5a12c345-9643-4d2c-b770-695ec6affcaf' ip='2a01:e35:2e2f:fbb0:43aa:33b5:5535:8905' network='0' port='0' priority='1015022078' protocol='tcp' type='host'/>
    <candidate xmlns='urn:xmpp:jingle:transports:ice-udp:1' component='2' foundation='1' generation='0' id='67f65b0b-8cee-421a-9f37-1f2ca2211c87' ip='2a01:e35:2e2f:fbb0:43aa:33b5:5535:8905' network='0' port='39431' priority='1010827774' protocol='tcp' type='host'/>
</transport>"
                .parse()
                .unwrap();
        let transport = Transport::try_from(elem).unwrap();
        assert_eq!(transport.pwd.unwrap(), "wakMJ8Ydd5rqnPaFerws5o");
        assert_eq!(transport.ufrag.unwrap(), "aeXX");
    }

    #[test]
    fn test_jitsi_meet() {
        let elem: Element = "<transport ufrag='2acq51d4p07v2m' pwd='7lk9uul39gckit6t02oavv2r9j' xmlns='urn:xmpp:jingle:transports:ice-udp:1'>
    <fingerprint hash='sha-1' setup='actpass' xmlns='urn:xmpp:jingle:apps:dtls:0'>97:F2:B5:BE:DB:A6:00:B1:3E:40:B2:41:3C:0D:FC:E0:BD:B2:A0:E8</fingerprint>
    <candidate type='host' protocol='udp' id='186cb069513c2bbe546192c93cc4ab3b05ab0d426' ip='2a05:d014:fc7:54a1:8bfc:7248:3d1c:51a4' component='1' port='10000' foundation='1' generation='0' priority='2130706431' network='0'/>
    <candidate type='host' protocol='udp' id='186cb069513c2bbe546192c93cc4ab3b063daeefd' ip='10.15.1.120' component='1' port='10000' foundation='2' generation='0' priority='2130706431' network='0'/>
    <candidate rel-port='10000' type='srflx' protocol='udp' id='186cb069513c2bbe546192c93cc4ab3b05d449db8' ip='3.120.176.51' component='1' port='10000' foundation='3' generation='0' network='0' priority='1677724415' rel-addr='10.15.1.120'/>
</transport>"
                .parse()
                .unwrap();
        let transport = Transport::try_from(elem).unwrap();
        assert_eq!(transport.pwd.unwrap(), "7lk9uul39gckit6t02oavv2r9j");
        assert_eq!(transport.ufrag.unwrap(), "2acq51d4p07v2m");

        let fingerprint = transport.fingerprint.unwrap();
        assert_eq!(fingerprint.hash, Algo::Sha_1);
        assert_eq!(fingerprint.setup, Setup::Actpass);
        assert_eq!(
            fingerprint.value,
            [
                151, 242, 181, 190, 219, 166, 0, 177, 62, 64, 178, 65, 60, 13, 252, 224, 189, 178,
                160, 232
            ]
        );
    }

    #[test]
    fn test_serialize_transport() {
        let reference: Element =
            "<transport xmlns='urn:xmpp:jingle:transports:ice-udp:1'><fingerprint xmlns='urn:xmpp:jingle:apps:dtls:0' hash='sha-256' setup='actpass'>02:1A:CC:54:27:AB:EB:9C:53:3F:3E:4B:65:2E:7D:46:3F:54:42:CD:54:F1:7A:03:A2:7D:F9:B0:7F:46:19:B2</fingerprint></transport>"
                .parse()
                .unwrap();

        let elem: Element = "<fingerprint xmlns='urn:xmpp:jingle:apps:dtls:0' hash='sha-256' setup='actpass'>02:1A:CC:54:27:AB:EB:9C:53:3F:3E:4B:65:2E:7D:46:3F:54:42:CD:54:F1:7A:03:A2:7D:F9:B0:7F:46:19:B2</fingerprint>"
                .parse()
                .unwrap();
        let fingerprint = Fingerprint::try_from(elem).unwrap();

        let transport = Transport {
            pwd: None,
            ufrag: None,
            candidates: vec![],
            fingerprint: Some(fingerprint),
        };

        let serialized: Element = transport.into();
        assert_eq!(serialized, reference);
    }
}
