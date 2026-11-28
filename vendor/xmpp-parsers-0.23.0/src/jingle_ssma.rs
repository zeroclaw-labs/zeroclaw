// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;

/// Source element for the ssrc SDP attribute.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_SSMA, name = "source")]
pub struct Source {
    /// Maps to the ssrc-id parameter.
    #[xml(attribute = "ssrc")]
    pub id: u32,

    /// List of attributes for this source.
    #[xml(child(n = ..))]
    pub parameters: Vec<Parameter>,
}

impl Source {
    /// Create a new SSMA Source element.
    pub fn new(id: u32) -> Source {
        Source {
            id,
            parameters: Vec::new(),
        }
    }
}

/// Parameter associated with a ssrc.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_SSMA, name = "parameter")]
pub struct Parameter {
    /// The name of the parameter.
    #[xml(attribute)]
    pub name: String,

    /// The optional value of the parameter.
    #[xml(attribute(default))]
    pub value: Option<String>,
}

generate_attribute!(
    /// From RFC5888, the list of allowed semantics.
    Semantics, "semantics", {
        /// Lip Synchronization, defined in RFC5888.
        Ls => "LS",

        /// Flow Identification, defined in RFC5888.
        Fid => "FID",

        /// Single Reservation Flow, defined in RFC3524.
        Srf => "SRF",

        /// Alternative Network Address Types, defined in RFC4091.
        Anat => "ANAT",

        /// Forward Error Correction, defined in RFC4756.
        Fec => "FEC",

        /// Decoding Dependency, defined in RFC5583.
        Ddp => "DDP",
    }
);

/// Element grouping multiple ssrc.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::JINGLE_SSMA, name = "ssrc-group")]
pub struct Group {
    /// The semantics of this group.
    #[xml(attribute)]
    pub semantics: Semantics,

    /// The various ssrc concerned by this group.
    #[xml(child(n = ..))]
    pub sources: Vec<Source>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Source, 16);
        assert_size!(Parameter, 24);
        assert_size!(Semantics, 1);
        assert_size!(Group, 16);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Source, 32);
        assert_size!(Parameter, 48);
        assert_size!(Semantics, 1);
        assert_size!(Group, 32);
    }

    #[test]
    fn parse_source() {
        let elem: Element = "<source ssrc='1656081975' xmlns='urn:xmpp:jingle:apps:rtp:ssma:0'>
    <parameter name='cname' value='Yv/wvbCdsDW2Prgd'/>
    <parameter name='msid' value='MLTJKIHilGn71fNQoszkQ4jlPTuS5vJyKVIv MLTJKIHilGn71fNQoszkQ4jlPTuS5vJyKVIva0'/>
</source>"
                .parse()
                .unwrap();
        let mut ssrc = Source::try_from(elem).unwrap();
        assert_eq!(ssrc.id, 1656081975);
        assert_eq!(ssrc.parameters.len(), 2);
        let parameter = ssrc.parameters.pop().unwrap();
        assert_eq!(parameter.name, "msid");
        assert_eq!(
            parameter.value.unwrap(),
            "MLTJKIHilGn71fNQoszkQ4jlPTuS5vJyKVIv MLTJKIHilGn71fNQoszkQ4jlPTuS5vJyKVIva0"
        );
        let parameter = ssrc.parameters.pop().unwrap();
        assert_eq!(parameter.name, "cname");
        assert_eq!(parameter.value.unwrap(), "Yv/wvbCdsDW2Prgd");
    }

    #[test]
    fn parse_source_group() {
        let elem: Element = "<ssrc-group semantics='FID' xmlns='urn:xmpp:jingle:apps:rtp:ssma:0'>
    <source ssrc='2301230316'/>
    <source ssrc='386328120'/>
</ssrc-group>"
            .parse()
            .unwrap();
        let mut group = Group::try_from(elem).unwrap();
        assert_eq!(group.semantics, Semantics::Fid);
        assert_eq!(group.sources.len(), 2);
        let source = group.sources.pop().unwrap();
        assert_eq!(source.id, 386328120);
        let source = group.sources.pop().unwrap();
        assert_eq!(source.id, 2301230316);
    }
}
