// Copyright (c) 2017-2021 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::data_forms::DataForm;
use crate::date::DateTime;
use crate::forwarding::Forwarded;
use crate::iq::{IqGetPayload, IqResultPayload, IqSetPayload};
use crate::message::MessagePayload;
use crate::ns;
use crate::pubsub::NodeName;
use crate::rsm::{SetQuery, SetResult};

generate_id!(
    /// An identifier matching a result message to the query requesting it.
    QueryId
);

/// Starts a query to the archive.
#[derive(FromXml, AsXml, Debug)]
#[xml(namespace = ns::MAM, name = "query")]
pub struct Query {
    /// An optional identifier for matching forwarded messages to this
    /// query.
    #[xml(attribute(default))]
    pub queryid: Option<QueryId>,

    /// Must be set to Some when querying a PubSub node’s archive.
    #[xml(attribute(default))]
    pub node: Option<NodeName>,

    /// Used for filtering the results.
    #[xml(child(default))]
    pub form: Option<DataForm>,

    /// Used for paging through results.
    #[xml(child(default))]
    pub set: Option<SetQuery>,

    /// Used for reversing the order of the results.
    #[xml(flag(name = "flip-page"))]
    pub flip_page: bool,
}

impl IqGetPayload for Query {}
impl IqSetPayload for Query {}
impl IqResultPayload for Query {}

/// The wrapper around forwarded stanzas.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::MAM, name = "result")]
pub struct Result_ {
    /// The stanza-id under which the archive stored this stanza.
    #[xml(attribute)]
    pub id: String,

    /// The same queryid as the one requested in the
    /// [query](struct.Query.html).
    #[xml(attribute(default))]
    pub queryid: Option<QueryId>,

    /// The actual stanza being forwarded.
    #[xml(child)]
    pub forwarded: Forwarded,
}

impl MessagePayload for Result_ {}

/// Notes the end of a page in a query.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::MAM, name = "fin")]
pub struct Fin {
    /// True when the end of a MAM query has been reached.
    #[xml(attribute(default))]
    pub complete: bool,

    /// Describes the current page, it should contain at least [first]
    /// (with an [index]) and [last], and generally [count].
    ///
    /// [first]: ../rsm/struct.SetResult.html#structfield.first
    /// [index]: ../rsm/struct.SetResult.html#structfield.first_index
    /// [last]: ../rsm/struct.SetResult.html#structfield.last
    /// [count]: ../rsm/struct.SetResult.html#structfield.count
    #[xml(child)]
    pub set: SetResult,
}

impl IqResultPayload for Fin {}

/// Metadata of the first message in the archive.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::MAM, name = "start")]
pub struct Start {
    /// The id of the first message in the archive.
    #[xml(attribute)]
    pub id: String,

    /// Time at which that message was sent.
    #[xml(attribute)]
    pub timestamp: DateTime,
}

/// Metadata of the last message in the archive.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::MAM, name = "end")]
pub struct End {
    /// The id of the last message in the archive.
    #[xml(attribute)]
    pub id: String,

    /// Time at which that message was sent.
    #[xml(attribute)]
    pub timestamp: DateTime,
}

/// Request an archive for its metadata.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::MAM, name = "metadata")]
pub struct MetadataQuery;

impl IqGetPayload for MetadataQuery {}

/// Response from the archive, containing the start and end metadata if it isn’t empty.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::MAM, name = "metadata")]
pub struct MetadataResponse {
    /// Metadata about the first message in the archive.
    #[xml(child(default))]
    pub start: Option<Start>,

    /// Metadata about the last message in the archive.
    #[xml(child(default))]
    pub end: Option<End>,
}

impl IqResultPayload for MetadataResponse {}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(QueryId, 12);
        assert_size!(Query, 108);
        assert_size!(Result_, 176);
        assert_size!(Fin, 44);
        assert_size!(Start, 28);
        assert_size!(End, 28);
        assert_size!(MetadataQuery, 0);
        assert_size!(MetadataResponse, 56);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(QueryId, 24);
        assert_size!(Query, 216);
        assert_size!(Result_, 336);
        assert_size!(Fin, 88);
        assert_size!(Start, 40);
        assert_size!(End, 40);
        assert_size!(MetadataQuery, 0);
        assert_size!(MetadataResponse, 80);
    }

    #[test]
    fn test_query() {
        let elem: Element = "<query xmlns='urn:xmpp:mam:2'/>".parse().unwrap();
        Query::try_from(elem).unwrap();
    }

    #[test]
    fn test_result() {
        #[cfg(not(feature = "component"))]
        let elem: Element = r#"<result xmlns='urn:xmpp:mam:2' queryid='f27' id='28482-98726-73623'>
  <forwarded xmlns='urn:xmpp:forward:0'>
    <delay xmlns='urn:xmpp:delay' stamp='2010-07-10T23:08:25Z'/>
    <message xmlns='jabber:client' from="witch@shakespeare.lit" to="macbeth@shakespeare.lit">
      <body>Hail to thee</body>
    </message>
  </forwarded>
</result>
"#
        .parse()
        .unwrap();
        #[cfg(feature = "component")]
        let elem: Element = r#"<result xmlns='urn:xmpp:mam:2' queryid='f27' id='28482-98726-73623'>
  <forwarded xmlns='urn:xmpp:forward:0'>
    <delay xmlns='urn:xmpp:delay' stamp='2010-07-10T23:08:25Z'/>
    <message xmlns='jabber:component:accept' from="witch@shakespeare.lit" to="macbeth@shakespeare.lit">
      <body>Hail to thee</body>
    </message>
  </forwarded>
</result>
"#.parse().unwrap();
        Result_::try_from(elem).unwrap();
    }

    #[test]
    fn test_fin() {
        let elem: Element = r#"<fin xmlns='urn:xmpp:mam:2'>
  <set xmlns='http://jabber.org/protocol/rsm'>
    <first index='0'>28482-98726-73623</first>
    <last>09af3-cc343-b409f</last>
  </set>
</fin>
"#
        .parse()
        .unwrap();
        Fin::try_from(elem).unwrap();
    }

    #[test]
    fn test_query_x() {
        let elem: Element = r#"<query xmlns='urn:xmpp:mam:2'>
  <x xmlns='jabber:x:data' type='submit'>
    <field var='FORM_TYPE' type='hidden'>
      <value>urn:xmpp:mam:2</value>
    </field>
    <field var='with'>
      <value>juliet@capulet.lit</value>
    </field>
  </x>
</query>
"#
        .parse()
        .unwrap();
        Query::try_from(elem).unwrap();
    }

    #[test]
    fn test_query_x_set() {
        let elem: Element = r#"<query xmlns='urn:xmpp:mam:2'>
  <x xmlns='jabber:x:data' type='submit'>
    <field var='FORM_TYPE' type='hidden'>
      <value>urn:xmpp:mam:2</value>
    </field>
    <field var='start'>
      <value>2010-08-07T00:00:00Z</value>
    </field>
  </x>
  <set xmlns='http://jabber.org/protocol/rsm'>
    <max>10</max>
  </set>
</query>
"#
        .parse()
        .unwrap();
        Query::try_from(elem).unwrap();
    }

    #[test]
    fn test_query_x_set_flipped() {
        let elem: Element = r#"<query xmlns='urn:xmpp:mam:2'>
  <x xmlns='jabber:x:data' type='submit'>
    <field var='FORM_TYPE' type='hidden'>
      <value>urn:xmpp:mam:2</value>
    </field>
    <field var='start'>
      <value>2010-08-07T00:00:00Z</value>
    </field>
  </x>
  <set xmlns='http://jabber.org/protocol/rsm'>
    <max>10</max>
  </set>
  <flip-page/>
</query>
"#
        .parse()
        .unwrap();
        Query::try_from(elem).unwrap();
    }

    #[test]
    fn test_metadata() {
        let elem: Element = r"<metadata xmlns='urn:xmpp:mam:2'/>".parse().unwrap();
        MetadataQuery::try_from(elem).unwrap();

        let elem: Element = r"<metadata xmlns='urn:xmpp:mam:2'>
  <start id='YWxwaGEg' timestamp='2008-08-22T21:09:04Z' />
  <end id='b21lZ2Eg' timestamp='2020-04-20T14:34:21Z' />
</metadata>"
            .parse()
            .unwrap();
        let metadata = MetadataResponse::try_from(elem).unwrap();
        let start = metadata.start.unwrap();
        let end = metadata.end.unwrap();
        assert_eq!(start.id, "YWxwaGEg");
        assert_eq!(start.timestamp.0.unix_timestamp(), 1219439344);
        assert_eq!(end.id, "b21lZ2Eg");
        assert_eq!(end.timestamp.0.unix_timestamp(), 1587393261);
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element = "<query xmlns='urn:xmpp:mam:2'><coucou/></query>"
            .parse()
            .unwrap();
        let error = Query::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in Query element.");
    }

    #[test]
    fn test_serialise_empty() {
        let elem: Element = "<query xmlns='urn:xmpp:mam:2'/>".parse().unwrap();
        let replace = Query {
            queryid: None,
            node: None,
            form: None,
            set: None,
            flip_page: false,
        };
        let elem2 = replace.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_serialize_query_with_form() {
        let reference: Element = "<query xmlns='urn:xmpp:mam:2'><x xmlns='jabber:x:data' type='submit'><field xmlns='jabber:x:data' var='FORM_TYPE' type='hidden'><value xmlns='jabber:x:data'>urn:xmpp:mam:2</value></field><field xmlns='jabber:x:data' var='with'><value xmlns='jabber:x:data'>juliet@capulet.lit</value></field></x><flip-page/></query>"
        .parse()
        .unwrap();

        let elem: Element = "<x xmlns='jabber:x:data' type='submit'><field xmlns='jabber:x:data' var='FORM_TYPE' type='hidden'><value xmlns='jabber:x:data'>urn:xmpp:mam:2</value></field><field xmlns='jabber:x:data' var='with'><value xmlns='jabber:x:data'>juliet@capulet.lit</value></field></x>"
          .parse()
          .unwrap();

        let form = DataForm::try_from(elem).unwrap();

        let query = Query {
            queryid: None,
            node: None,
            set: None,
            form: Some(form),
            flip_page: true,
        };
        let serialized: Element = query.into();
        assert_eq!(serialized, reference);
    }

    #[test]
    #[cfg(not(feature = "component"))] // feature = "component" changes <message/> namespace
    fn test_serialize_result() {
        let reference: Element = "<result xmlns='urn:xmpp:mam:2' queryid='f27' id='28482-98726-73623'><forwarded xmlns='urn:xmpp:forward:0'><delay xmlns='urn:xmpp:delay' stamp='2002-09-10T23:08:25+00:00'/><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded></result>"
        .parse()
        .unwrap();

        let elem: Element = "<forwarded xmlns='urn:xmpp:forward:0'><delay xmlns='urn:xmpp:delay' stamp='2002-09-10T23:08:25+00:00'/><message xmlns='jabber:client' to='juliet@capulet.example/balcony' from='romeo@montague.example/home'/></forwarded>"
          .parse()
          .unwrap();

        let forwarded = Forwarded::try_from(elem).unwrap();

        let result = Result_ {
            id: String::from("28482-98726-73623"),
            queryid: Some(QueryId(String::from("f27"))),
            forwarded,
        };
        let serialized: Element = result.into();
        assert_eq!(serialized, reference);
    }

    #[test]
    fn test_serialize_fin() {
        let reference: Element = "<fin xmlns='urn:xmpp:mam:2' complete='false'><set xmlns='http://jabber.org/protocol/rsm'><first index='0'>28482-98726-73623</first><last>09af3-cc343-b409f</last></set></fin>"
        .parse()
        .unwrap();

        let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'><first index='0'>28482-98726-73623</first><last>09af3-cc343-b409f</last></set>"
          .parse()
          .unwrap();

        let set = SetResult::try_from(elem).unwrap();

        let fin = Fin {
            set,
            complete: false,
        };
        let serialized: Element = fin.into();
        assert_eq!(serialized, reference);
    }
}
