// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use alloc::borrow::Cow;

use xso::{AsXml, FromXml};

use crate::date::Xep0082;
use crate::iq::{IqGetPayload, IqResultPayload};
use crate::ns;
use time::{OffsetDateTime, UtcOffset};
use xso::{error::Error, text::TextCodec};

// XEP-0202 requires the entity timestamp to use UTC, independently of tzo.
struct UtcDateTime;
impl TextCodec<OffsetDateTime> for UtcDateTime {
    fn decode(&self, s: String) -> Result<OffsetDateTime, Error> {
        let value: OffsetDateTime = Xep0082.decode(s)?;
        Ok(value.to_offset(UtcOffset::UTC))
    }

    fn encode<'x>(&self, value: &'x OffsetDateTime) -> Result<Option<Cow<'x, str>>, Error> {
        Ok(Xep0082
            .encode(&value.to_offset(UtcOffset::UTC))?
            .map(|s| Cow::Owned(s.into_owned())))
    }
}

struct ColonSeparatedOffset;

impl TextCodec<UtcOffset> for ColonSeparatedOffset {
    fn decode(&self, s: String) -> Result<UtcOffset, Error> {
        UtcOffset::parse(
            &s,
            time::macros::format_description!("[offset_hour sign:mandatory]:[offset_minute]"),
        )
        .map_err(Error::text_parse_error)
    }

    fn encode<'x>(&self, value: &'x UtcOffset) -> Result<Option<Cow<'x, str>>, Error> {
        value
            .format(time::macros::format_description!(
                "[offset_hour sign:mandatory]:[offset_minute]"
            ))
            .map(|s| Some(Cow::Owned(s)))
            .map_err(Error::text_parse_error)
    }
}

/// An entity time query.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::TIME, name = "time")]
pub struct TimeQuery;

impl IqGetPayload for TimeQuery {}

/// An entity time result, containing an unique DateTime.
#[derive(Debug, Clone, Copy, FromXml, AsXml)]
#[xml(namespace = ns::TIME, name = "time")]
pub struct TimeResult {
    /// The UTC offset
    #[xml(extract(name = "tzo", fields(text(codec = ColonSeparatedOffset))))]
    pub tz_offset: UtcOffset,

    /// The UTC timestamp
    #[xml(extract(name = "utc", fields(text(codec = UtcDateTime))))]
    pub utc: OffsetDateTime,
}

impl IqResultPayload for TimeResult {}

impl From<TimeResult> for OffsetDateTime {
    fn from(time: TimeResult) -> Self {
        time.utc.to_offset(time.tz_offset)
    }
}

impl From<OffsetDateTime> for TimeResult {
    fn from(dt: OffsetDateTime) -> Self {
        Self {
            tz_offset: dt.offset(),
            utc: dt.to_offset(UtcOffset::UTC),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use minidom::Element;

    // DateTime’s size doesn’t depend on the architecture.
    #[test]
    fn test_size() {
        assert_size!(TimeQuery, 0);
        assert_eq!(
            core::mem::size_of::<TimeResult>(),
            core::mem::size_of::<(UtcOffset, OffsetDateTime)>()
        );
    }

    #[test]
    fn parse_response() {
        let elem: Element =
            "<time xmlns='urn:xmpp:time'><tzo>-06:00</tzo><utc>2006-12-19T17:58:35Z</utc></time>"
                .parse()
                .unwrap();
        let elem1 = elem.clone();
        let time = TimeResult::try_from(elem).unwrap();
        let dt = OffsetDateTime::from(time);
        assert_eq!(dt.offset(), UtcOffset::from_hms(-6, 0, 0).unwrap());
        assert_eq!(
            dt,
            OffsetDateTime::parse(
                "2006-12-19T12:58:35-05:00",
                &time::format_description::well_known::Rfc3339
            )
            .unwrap()
        );
        let elem2 = Element::from(time);
        assert_eq!(elem1, elem2);
    }
}
