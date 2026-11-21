// Copyright (c) 2017-2018 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
// Copyright (c) 2017 Maxime “pep” Buquet <pep@bouah.net>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub const JABBER_CLIENT: &str = "jabber:client";
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub const XMPP_STANZAS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub const XMPP_STREAMS: &str = "urn:ietf:params:xml:ns:xmpp-streams";
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub const STREAM: &str = "http://etherx.jabber.org/streams";
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub const TLS: &str = "urn:ietf:params:xml:ns:xmpp-tls";
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub const SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
/// RFC 6120: Extensible Messaging and Presence Protocol (XMPP): Core
pub const BIND: &str = "urn:ietf:params:xml:ns:xmpp-bind";

/// RFC 6121: Extensible Messaging and Presence Protocol (XMPP): Instant Messaging and Presence
pub const ROSTER: &str = "jabber:iq:roster";

/// RFC 7395: An Extensible Messaging and Presence Protocol (XMPP) Subprotocol for WebSocket
pub const WEBSOCKET: &str = "urn:ietf:params:xml:ns:xmpp-framing";

/// XEP-0004: Data Forms
pub const DATA_FORMS: &str = "jabber:x:data";

/// XEP-0030: Service Discovery
pub const DISCO_INFO: &str = "http://jabber.org/protocol/disco#info";
/// XEP-0030: Service Discovery
pub const DISCO_ITEMS: &str = "http://jabber.org/protocol/disco#items";

/// XEP-0045: Multi-User Chat
pub const MUC: &str = "http://jabber.org/protocol/muc";
/// XEP-0045: Multi-User Chat
pub const MUC_USER: &str = "http://jabber.org/protocol/muc#user";

/// XEP-0047: In-Band Bytestreams
pub const IBB: &str = "http://jabber.org/protocol/ibb";

/// XEP-0048: Bookmarks
pub const BOOKMARKS: &str = "storage:bookmarks";

/// XEP-0049: Private XML Storage
pub const PRIVATE: &str = "jabber:iq:private";

/// XEP-0054: vcard-temp
pub const VCARD: &str = "vcard-temp";

/// XEP-0059: Result Set Management
pub const RSM: &str = "http://jabber.org/protocol/rsm";

/// XEP-0060: Publish-Subscribe
pub const PUBSUB: &str = "http://jabber.org/protocol/pubsub";
/// XEP-0060: Publish-Subscribe
pub const PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";
/// XEP-0060: Publish-Subscribe
pub const PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
/// XEP-0060: Publish-Subscribe
pub const PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
/// XEP-0060: Publish-Subscribe node configuration
pub const PUBSUB_CONFIGURE: &str = "http://jabber.org/protocol/pubsub#node_config";

/// XEP-0066: Out of Band Data
pub const OOB: &str = "jabber:x:oob";

/// XEP-0070: Verifying HTTP Requests via XMPP
pub const HTTP_AUTH: &str = "http://jabber.org/protocol/http-auth";

/// XEP-0071: XHTML-IM
pub const XHTML_IM: &str = "http://jabber.org/protocol/xhtml-im";
/// XEP-0071: XHTML-IM
pub const XHTML: &str = "http://www.w3.org/1999/xhtml";

/// XEP-0077: In-Band Registration
pub const REGISTER: &str = "jabber:iq:register";
/// XEP-0077: In-Band Registration
pub const REGISTER_FEATURE: &str = "http://jabber.org/features/iq-register";

/// XEP-0084: User Avatar
pub const AVATAR_DATA: &str = "urn:xmpp:avatar:data";
/// XEP-0084: User Avatar
pub const AVATAR_METADATA: &str = "urn:xmpp:avatar:metadata";

/// XEP-0085: Chat State Notifications
pub const CHATSTATES: &str = "http://jabber.org/protocol/chatstates";

/// XEP-0092: Software Version
pub const VERSION: &str = "jabber:iq:version";

/// XEP-0107: User Mood
pub const MOOD: &str = "http://jabber.org/protocol/mood";

/// XEP-0114: Jabber Component Protocol
pub const COMPONENT_ACCEPT: &str = "jabber:component:accept";

/// XEP-0114: Jabber Component Protocol
pub const COMPONENT: &str = "jabber:component:accept";

/// XEP-0115: Entity Capabilities
pub const CAPS: &str = "http://jabber.org/protocol/caps";

/// XEP-0118: User Tune
pub const TUNE: &str = "http://jabber.org/protocol/tune";

/// XEP-0122: Data Forms Validation
pub const XDATA_VALIDATE: &str = "http://jabber.org/protocol/xdata-validate";

/// XEP-0153: vCard-Based Avatars
pub const VCARD_UPDATE: &str = "vcard-temp:x:update";

/// XEP-0157: Contact Addresses for XMPP Services
pub const SERVER_INFO: &str = "http://jabber.org/network/serverinfo";

/// XEP-0166: Jingle
pub const JINGLE: &str = "urn:xmpp:jingle:1";

/// XEP-0167: Jingle RTP Sessions
pub const JINGLE_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
/// XEP-0167: Jingle RTP Sessions
pub const JINGLE_RTP_AUDIO: &str = "urn:xmpp:jingle:apps:rtp:audio";
/// XEP-0167: Jingle RTP Sessions
pub const JINGLE_RTP_VIDEO: &str = "urn:xmpp:jingle:apps:rtp:video";

/// XEP-0172: User Nickname
pub const NICK: &str = "http://jabber.org/protocol/nick";

/// XEP-0176: Jingle ICE-UDP Transport Method
pub const JINGLE_ICE_UDP: &str = "urn:xmpp:jingle:transports:ice-udp:1";

/// XEP-0177: Jingle Raw UDP Transport Method
pub const JINGLE_RAW_UDP: &str = "urn:xmpp:jingle:transports:raw-udp:1";

/// XEP-0184: Message Delivery Receipts
pub const RECEIPTS: &str = "urn:xmpp:receipts";

/// XEP-0191: Blocking Command
pub const BLOCKING: &str = "urn:xmpp:blocking";
/// XEP-0191: Blocking Command
pub const BLOCKING_ERRORS: &str = "urn:xmpp:blocking:errors";

/// XEP-0198: Stream Management
pub const SM: &str = "urn:xmpp:sm:3";

/// XEP-0199: XMPP Ping
pub const PING: &str = "urn:xmpp:ping";

/// XEP-0202: Entity Time
pub const TIME: &str = "urn:xmpp:time";

/// XEP-0203: Delayed Delivery
pub const DELAY: &str = "urn:xmpp:delay";

/// XEP-0215: External Service Discovery
pub const EXT_DISCO: &str = "urn:xmpp:extdisco:2";

/// XEP-0221: Data Forms Media Element
pub const MEDIA_ELEMENT: &str = "urn:xmpp:media-element";

/// XEP-0224: Attention
pub const ATTENTION: &str = "urn:xmpp:attention:0";

/// XEP-0231: Bits of Binary
pub const BOB: &str = "urn:xmpp:bob";

/// XEP-0234: Jingle File Transfer
pub const JINGLE_FT: &str = "urn:xmpp:jingle:apps:file-transfer:5";
/// XEP-0234: Jingle File Transfer
pub const JINGLE_FT_ERROR: &str = "urn:xmpp:jingle:apps:file-transfer:errors:0";

/// XEP-0257: Client Certificate Management for SASL EXTERNAL
pub const SASL_CERT: &str = "urn:xmpp:saslcert:1";

/// XEP-0260: Jingle SOCKS5 Bytestreams Transport Method
pub const JINGLE_S5B: &str = "urn:xmpp:jingle:transports:s5b:1";

/// XEP-0261: Jingle In-Band Bytestreams Transport Method
pub const JINGLE_IBB: &str = "urn:xmpp:jingle:transports:ibb:1";

/// XEP-0264: Jingle Content Thumbnails
pub const JINGLE_THUMBNAILS: &str = "urn:xmpp:thumbs:1";

/// XEP-0277: Microblogging over XMPP
pub const MICROBLOG: &str = "urn:xmpp:microblog:0";

/// XEP-0280: Message Carbons
pub const CARBONS: &str = "urn:xmpp:carbons:2";

/// XEP-0293: Jingle RTP Feedback Negotiation
pub const JINGLE_RTCP_FB: &str = "urn:xmpp:jingle:apps:rtp:rtcp-fb:0";

/// XEP-0294: Jingle RTP Header Extensions Negotiation
pub const JINGLE_RTP_HDREXT: &str = "urn:xmpp:jingle:apps:rtp:rtp-hdrext:0";

/// XEP-0297: Stanza Forwarding
pub const FORWARD: &str = "urn:xmpp:forward:0";

/// XEP-0300: Use of Cryptographic Hash Functions in XMPP
pub const HASHES: &str = "urn:xmpp:hashes:2";
/// XEP-0300: Use of Cryptographic Hash Functions in XMPP
pub const HASH_ALGO_SHA_256: &str = "urn:xmpp:hash-function-text-names:sha-256";
/// XEP-0300: Use of Cryptographic Hash Functions in XMPP
pub const HASH_ALGO_SHA_512: &str = "urn:xmpp:hash-function-text-names:sha-512";
/// XEP-0300: Use of Cryptographic Hash Functions in XMPP
pub const HASH_ALGO_SHA3_256: &str = "urn:xmpp:hash-function-text-names:sha3-256";
/// XEP-0300: Use of Cryptographic Hash Functions in XMPP
pub const HASH_ALGO_SHA3_512: &str = "urn:xmpp:hash-function-text-names:sha3-512";
/// XEP-0300: Use of Cryptographic Hash Functions in XMPP
pub const HASH_ALGO_BLAKE2B_256: &str = "urn:xmpp:hash-function-text-names:id-blake2b256";
/// XEP-0300: Use of Cryptographic Hash Functions in XMPP
pub const HASH_ALGO_BLAKE2B_512: &str = "urn:xmpp:hash-function-text-names:id-blake2b512";

/// XEP-0301: In-Band Real Time Text
pub const RTT: &str = "urn:xmpp:rtt:0";

/// XEP-0308: Last Message Correction
pub const MESSAGE_CORRECT: &str = "urn:xmpp:message-correct:0";

/// XEP-0313: Message Archive Management
pub const MAM: &str = "urn:xmpp:mam:2";

/// XEP-0319: Last User Interaction in Presence
pub const IDLE: &str = "urn:xmpp:idle:1";

/// XEP-0320: Use of DTLS-SRTP in Jingle Sessions
pub const JINGLE_DTLS: &str = "urn:xmpp:jingle:apps:dtls:0";

/// XEP-0328: JID Prep
pub const JID_PREP: &str = "urn:xmpp:jidprep:0";

/// XEP-0333: Displayed markers
pub const DISPLAYED_MARKERS: &str = "urn:xmpp:chat-markers:0";

/// XEP-0335: JSON Containers
pub const JSON_CONTAINERS: &str = "urn:xmpp:json:0";

/// XEP-0338: Jingle Grouping Framework
pub const JINGLE_GROUPING: &str = "urn:xmpp:jingle:apps:grouping:0";

/// XEP-0339: Source-Specific Media Attributes in Jingle
pub const JINGLE_SSMA: &str = "urn:xmpp:jingle:apps:rtp:ssma:0";

/// XEP-0352: Client State Indication
pub const CSI: &str = "urn:xmpp:csi:0";

/// XEP-0353: Jingle Message Initiation
pub const JINGLE_MESSAGE: &str = "urn:xmpp:jingle-message:0";

/// XEP-0357: Push Notifications
pub const PUSH: &str = "urn:xmpp:push:0";

/// XEP-0359: Unique and Stable Stanza IDs
pub const SID: &str = "urn:xmpp:sid:0";

/// XEP-0363: HTTP File Upload
pub const HTTP_UPLOAD: &str = "urn:xmpp:http:upload:0";

/// XEP-0373: OpenPGP for XMPP
pub const OX: &str = "urn:xmpp:openpgp:0";
/// XEP-0373: OpenPGP for XMPP
pub const OX_PUBKEYS: &str = "urn:xmpp:openpgp:0:public-keys";

/// XEP-0377: Spam Reporting
pub const SPAM_REPORTING: &str = "urn:xmpp:reporting:1";

/// XEP-0380: Explicit Message Encryption
pub const EME: &str = "urn:xmpp:eme:0";

/// XEP-0384: OMEMO Encryption (experimental version 0.3.0)
pub const LEGACY_OMEMO: &str = "eu.siacs.conversations.axolotl";
/// XEP-0384: OMEMO Encryption (experimental version 0.3.0)
pub const LEGACY_OMEMO_DEVICELIST: &str = "eu.siacs.conversations.axolotl.devicelist";
/// XEP-0384: OMEMO Encryption (experimental version 0.3.0)
pub const LEGACY_OMEMO_BUNDLES: &str = "eu.siacs.conversations.axolotl.bundles";

/// XEP-0386: Bind 2
pub const BIND2: &str = "urn:xmpp:bind:0";

/// XEP-0388: Extensible SASL Profile
pub const SASL2: &str = "urn:xmpp:sasl:2";

/// XEP-0390: Entity Capabilities 2.0
pub const ECAPS2: &str = "urn:xmpp:caps";
/// XEP-0390: Entity Capabilities 2.0
pub const ECAPS2_OPTIMIZE: &str = "urn:xmpp:caps:optimize";

/// XEP-0402: PEP Native Bookmarks
pub const BOOKMARKS2: &str = "urn:xmpp:bookmarks:1";
/// XEP-0402: PEP Native Bookmarks
pub const BOOKMARKS2_COMPAT: &str = "urn:xmpp:bookmarks:1#compat";
/// XEP-0402: PEP Native Bookmarks
pub const BOOKMARKS2_COMPAT_PEP: &str = "urn:xmpp:bookmarks:1#compat-pep";

/// XEP-0421: Anonymous unique occupant identifiers for MUCs
pub const OID: &str = "urn:xmpp:occupant-id:0";

/// XEP-0440: SASL Channel-Binding Type Capability
pub const SASL_CB: &str = "urn:xmpp:sasl-cb:0";

/// XEP-0444: Message Reactions
pub const REACTIONS: &str = "urn:xmpp:reactions:0";

/// XEP-0463: MUC Affiliations Versioning
pub const MAV: &str = "urn:xmpp:muc:affiliations:1";

/// XEP-0478: Stream Limits Advertisement
pub const STREAM_LIMITS: &str = "urn:xmpp:stream-limits:0";

/// XEP-0484: Fast Authentication Streamlining Tokens
pub const FAST: &str = "urn:xmpp:fast:0";

/// XEP-0490: Message Displayed Synchronization
pub const MDS: &str = "urn:xmpp:mds:displayed:0";

/// XEP-0494: Client Access Management
pub const CAM: &str = "urn:xmpp:cam:0";

/// XEP-0507: Jingle Content Category
pub const JINGLE_CONTENT: &str = "urn:xmpp:jingle:apps:category:0";

/// Alias for the main namespace of the stream, that is "jabber:client" when
/// the component feature isn’t enabled.
#[cfg(not(feature = "component"))]
pub const DEFAULT_NS: &str = JABBER_CLIENT;

/// Alias for the main namespace of the stream, that is
/// "jabber:component:accept" when the component feature is enabled.
#[cfg(feature = "component")]
pub const DEFAULT_NS: &str = COMPONENT_ACCEPT;
