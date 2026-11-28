// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;

/// Requests paging through a potentially big set of items (represented by an
/// UID).
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::RSM, name = "set")]
pub struct SetQuery {
    /// Limit the number of items, or use the recipient’s defaults if None.
    #[xml(extract(default, fields(text(type_ = usize))))]
    pub max: Option<usize>,

    /// The UID after which to give results, or if None it is the element
    /// “before” the first item, effectively an index of negative one.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub after: Option<String>,

    /// The UID before which to give results, or if None it starts with the
    /// last page of the full set.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub before: Option<String>,

    /// Numerical index of the page (deprecated).
    #[xml(extract(default, fields(text(type_ = usize))))]
    pub index: Option<usize>,
}

/// The first item of the page.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::RSM, name = "first")]
pub struct First {
    /// The position of the [first item](#structfield.item) in the full set
    /// (which may be approximate).
    #[xml(attribute(default))]
    pub index: Option<usize>,

    /// The UID of the first item of the page.
    #[xml(text)]
    pub item: String,
}

/// Describes the paging result of a [query](struct.SetQuery.html).
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::RSM, name = "set")]
pub struct SetResult {
    /// The first item of the page.
    #[xml(child(default))]
    pub first: Option<First>,

    /// The UID of the last item of the page.
    #[xml(extract(default, fields(text(type_ = String))))]
    pub last: Option<String>,

    /// How many items there are in the full set (which may be approximate).
    #[xml(extract(default, fields(text(type_ = usize))))]
    pub count: Option<usize>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;
    use xso::error::{Error, FromElementError};

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(SetQuery, 40);
        assert_size!(SetResult, 40);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(SetQuery, 80);
        assert_size!(SetResult, 80);
    }

    #[test]
    fn test_simple() {
        let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'/>"
            .parse()
            .unwrap();
        let set = SetQuery::try_from(elem).unwrap();
        assert_eq!(set.max, None);
        assert_eq!(set.after, None);
        assert_eq!(set.before, None);
        assert_eq!(set.index, None);

        let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'/>"
            .parse()
            .unwrap();
        let set = SetResult::try_from(elem).unwrap();
        match set.first {
            Some(_) => panic!(),
            None => (),
        }
        assert_eq!(set.last, None);
        assert_eq!(set.count, None);
    }

    #[test]
    fn test_unknown() {
        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0'/>"
            .parse()
            .unwrap();
        let error = SetQuery::try_from(elem.clone()).unwrap_err();
        let returned_elem = match error {
            FromElementError::Mismatch(elem) => elem,
            _ => panic!(),
        };
        assert_eq!(elem, returned_elem);

        let elem: Element = "<replace xmlns='urn:xmpp:message-correct:0'/>"
            .parse()
            .unwrap();
        let error = SetResult::try_from(elem.clone()).unwrap_err();
        let returned_elem = match error {
            FromElementError::Mismatch(elem) => elem,
            _ => panic!(),
        };
        assert_eq!(elem, returned_elem);
    }

    #[test]
    #[cfg_attr(not(feature = "pedantic"), should_panic = "Result::unwrap_err")]
    fn test_invalid_child() {
        let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'><coucou/></set>"
            .parse()
            .unwrap();
        let error = SetQuery::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in SetQuery element.");

        let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'><coucou/></set>"
            .parse()
            .unwrap();
        let error = SetResult::try_from(elem).unwrap_err();
        let message = match error {
            FromElementError::Invalid(Error::Other(string)) => string,
            _ => panic!(),
        };
        assert_eq!(message, "Unknown child in SetResult element.");
    }

    #[test]
    fn test_serialise() {
        let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'/>"
            .parse()
            .unwrap();
        let rsm = SetQuery {
            max: None,
            after: None,
            before: None,
            index: None,
        };
        let elem2 = rsm.into();
        assert_eq!(elem, elem2);

        let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'/>"
            .parse()
            .unwrap();
        let rsm = SetResult {
            first: None,
            last: None,
            count: None,
        };
        let elem2 = rsm.into();
        assert_eq!(elem, elem2);
    }

    // TODO: This test is only ignored because <before/> and <before></before> aren’t equal in
    // minidom, let’s fix that instead!
    #[test]
    #[ignore]
    fn test_serialise_empty_before() {
        let elem: Element = "<set xmlns='http://jabber.org/protocol/rsm'><before/></set>"
            .parse()
            .unwrap();
        let rsm = SetQuery {
            max: None,
            after: None,
            before: Some("".into()),
            index: None,
        };
        let elem2 = rsm.into();
        assert_eq!(elem, elem2);
    }

    #[test]
    fn test_first_index() {
        let elem: Element =
            "<set xmlns='http://jabber.org/protocol/rsm'><first index='4'>coucou</first></set>"
                .parse()
                .unwrap();
        let elem1 = elem.clone();
        let set = SetResult::try_from(elem).unwrap();
        let first = set.first.unwrap();
        assert_eq!(first.item, "coucou");
        assert_eq!(first.index, Some(4));

        let set2 = SetResult {
            first: Some(First {
                item: String::from("coucou"),
                index: Some(4),
            }),
            last: None,
            count: None,
        };
        let elem2 = set2.into();
        assert_eq!(elem1, elem2);
    }
}
