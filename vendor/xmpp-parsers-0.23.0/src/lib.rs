//! A crate parsing common XMPP elements into Rust structures.
//!
//! Each module implements the `TryFrom<Element>` trait, which takes a
//! minidom [`Element`] and returns a `Result` whose value is `Ok` if the
//! element parsed correctly, `Err(error::Error)` otherwise.
//!
//! The returned structure can be manipulated as any Rust structure, with each
//! field being public.  You can also create the same structure manually, with
//! some having `new()` and `with_*()` helper methods to create them.
//!
//! Once you are happy with your structure, you can serialise it back to an
//! [`Element`], using either `From` or `Into<Element>`, which give you what
//! you want to be sending on the wire.
//!
//! [`Element`]: ../minidom/element/struct.Element.html

// Copyright (c) 2017-2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
// Copyright (c) 2017-2019 Maxime “pep” Buquet <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

#![warn(missing_docs)]
#![deny(
    non_camel_case_types,
    non_snake_case,
    unsafe_code,
    unused_variables,
    unused_mut,
    dead_code
)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![cfg_attr(docsrs, doc(auto_cfg))]

extern crate alloc;

pub use blake2;
pub use jid;
pub use minidom;
pub use sha1;
pub use sha2;
pub use sha3;

// We normally only reexport entire crates, but xso is a special case since it uses proc macros
// which require it to be directly imported as a crate.  The only useful symbol we have to reexport
// are its error types, which we expose in our return types.
pub use xso::error::{Error, FromElementError};

/// XML namespace definitions used through XMPP.
pub mod ns;

#[macro_use]
mod util;

/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod bind;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod iq;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod message;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod presence;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod sasl;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod stanza;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod stanza_error;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod starttls;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod stream;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod stream_error;
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub mod stream_features;

/// RFC 6121: Extensible Messaging and Presence Protocol (XMPP): Instant Messaging and Presence
pub mod roster;

/// RFC 7395: An Extensible Messaging and Presence Protocol (XMPP) Subprotocol for WebSocket
pub mod websocket;

/// XEP-0004: Data Forms
pub mod data_forms;

/// XEP-0030: Service Discovery
pub mod disco;

/// XEP-0045: Multi-User Chat
pub mod muc;

/// XEP-0047: In-Band Bytestreams
pub mod ibb;

/// XEP-0048: Bookmarks
pub mod bookmarks;

/// XEP-0049: Private XML storage
pub mod private;

/// XEP-0054: vcard-temp
pub mod vcard;

/// XEP-0059: Result Set Management
pub mod rsm;

/// XEP-0060: Publish-Subscribe
pub mod pubsub;

/// XEP-0066: OOB
pub mod oob;

/// XEP-0070: Verifying HTTP Requests via XMPP
pub mod confirm;

/// XEP-0071: XHTML-IM
pub mod xhtml;

/// XEP-0077: In-Band Registration
pub mod ibr;

/// XEP-0082: XMPP Date and Time Profiles
pub mod date;

/// XEP-0084: User Avatar
pub mod avatar;

/// XEP-0085: Chat State Notifications
pub mod chatstates;

/// XEP-0092: Software Version
pub mod version;

/// XEP-0107: User Mood
pub mod mood;

/// XEP-0114: Jabber Component Protocol
pub mod component;

/// XEP-0115: Entity Capabilities
pub mod caps;

/// XEP-0118: User Tune
pub mod tune;

/// XEP-0122: Data Forms Validation
pub mod data_forms_validate;

///XEP-0153: vCard-Based Avatars
pub mod vcard_update;

/// XEP-0157: Contact Addresses for XMPP Services
pub mod server_info;

/// XEP-0166: Jingle
pub mod jingle;

/// XEP-0167: Jingle RTP Sessions
pub mod jingle_rtp;

/// XEP-0172: User Nickname
pub mod nick;

/// XEP-0176: Jingle ICE-UDP Transport Method
pub mod jingle_ice_udp;

/// XEP-0177: Jingle Raw UDP Transport Method
pub mod jingle_raw_udp;

/// XEP-0184: Message Delivery Receipts
pub mod receipts;

/// XEP-0191: Blocking Command
pub mod blocking;

/// XEP-0198: Stream Management
pub mod sm;

/// XEP-0199: XMPP Ping
pub mod ping;

/// XEP-0202: Entity Time
pub mod time;

/// XEP-0203: Delayed Delivery
pub mod delay;

/// XEP-0215: External Service Discovery
pub mod extdisco;

/// XEP-0221: Data Forms Media Element
pub mod media_element;

/// XEP-0224: Attention
pub mod attention;

/// XEP-0231: Bits of Binary
pub mod bob;

/// XEP-0234: Jingle File Transfer
pub mod jingle_ft;

/// XEP-0257: Client Certificate Management for SASL EXTERNAL
pub mod cert_management;

/// XEP-0260: Jingle SOCKS5 Bytestreams Transport Method
pub mod jingle_s5b;

/// XEP-0261: Jingle In-Band Bytestreams Transport Method
pub mod jingle_ibb;

/// XEP-0264: Jingle Content Thumbnails
pub mod jingle_thumbnails;

/// XEP-0280: Message Carbons
pub mod carbons;

/// XEP-0293: Jingle RTP Feedback Negotiation
pub mod jingle_rtcp_fb;

/// XEP-0294: Jingle RTP Header Extensions Negotiation
pub mod jingle_rtp_hdrext;

/// XEP-0297: Stanza Forwarding
pub mod forwarding;

/// XEP-0300: Use of Cryptographic Hash Functions in XMPP
pub mod hashes;

/// XEP-0301: In-Band Real Time Text
pub mod rtt;

/// XEP-0308: Last Message Correction
pub mod message_correct;

/// XEP-0313: Message Archive Management
pub mod mam;

/// XEP-0319: Last User Interaction in Presence
pub mod idle;

/// XEP-0320: Use of DTLS-SRTP in Jingle Sessions
pub mod jingle_dtls_srtp;

/// XEP-0328: JID Prep
pub mod jid_prep;

/// XEP-0333: Displayed markers
pub mod displayed_markers;

/// XEP-0335: JSON Containers
pub mod json_containers;

/// XEP-0338: Jingle Grouping Framework
pub mod jingle_grouping;

/// XEP-0339: Source-Specific Media Attributes in Jingle
pub mod jingle_ssma;

/// XEP-0352: Client State Indication
pub mod csi;

/// XEP-0353: Jingle Message Initiation
pub mod jingle_message;

/// XEP-0357: Push Notifications
pub mod push;

/// XEP-0359: Unique and Stable Stanza IDs
pub mod stanza_id;

/// XEP-0363: HTTP File Upload
pub mod http_upload;

/// XEP-0373: OpenPGP for XMPP
pub mod openpgp;

/// XEP-0377: Spam Reporting
pub mod spam_reporting;

/// XEP-0380: Explicit Message Encryption
pub mod eme;

/// XEP-0380: OMEMO Encryption (experimental version 0.3.0)
pub mod legacy_omemo;

/// XEP-0386: Bind 2
pub mod bind2;

/// XEP-0388: Extensible SASL Profile
pub mod sasl2;

/// XEP-0390: Entity Capabilities 2.0
pub mod ecaps2;

/// XEP-0402: PEP Native Bookmarks
pub mod bookmarks2;

/// XEP-0421: Anonymous unique occupant identifiers for MUCs
pub mod occupant_id;

/// XEP-0441: Message Archive Management Preferences
pub mod mam_prefs;

/// XEP-0440: SASL Channel-Binding Type Capability
pub mod sasl_cb;

/// XEP-0444: Message Reactions
pub mod reactions;

/// XEP-0478: Stream Limits Advertisement
pub mod stream_limits;

/// XEP-0484: Fast Authentication Streamlining Tokens
pub mod fast;

/// XEP-0490: Message Displayed Synchronization
pub mod message_displayed;

/// XEP-0494: Client Access Management
pub mod cam;

/// XEP-0507: Jingle Content Category
pub mod jingle_content;

#[cfg(test)]
mod tests {
    #[test]
    fn reexports() {
        #[allow(unused_imports)]
        use crate::blake2;
        #[allow(unused_imports)]
        use crate::jid;
        #[allow(unused_imports)]
        use crate::minidom;
        #[allow(unused_imports)]
        use crate::sha1;
        #[allow(unused_imports)]
        use crate::sha2;
        #[allow(unused_imports)]
        use crate::sha3;
    }
}
