// Copyright (c) 2017 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use alloc::borrow::Cow;
use core::str::FromStr;
use minidom::{IntoAttributeValue, Node};
use time::{format_description::well_known::Rfc3339, OffsetDateTime, UtcOffset};
use xso::{error::Error, AsXmlText, FromXmlText, TextCodec};

/// Text codec for the XEP-0082 DateTime profile.
pub struct Xep0082;

impl TextCodec<OffsetDateTime> for Xep0082 {
    fn decode(&self, s: String) -> Result<OffsetDateTime, Error> {
        OffsetDateTime::parse(&s, &Rfc3339).map_err(Error::text_parse_error)
    }

    fn encode<'x>(&self, value: &'x OffsetDateTime) -> Result<Option<Cow<'x, str>>, Error> {
        value
            .format(&Rfc3339)
            .map(|s| Some(Cow::Owned(s)))
            .map_err(Error::text_parse_error)
    }
}

impl<T> TextCodec<Option<T>> for Xep0082
where
    Xep0082: TextCodec<T>,
{
    fn decode(&self, s: String) -> Result<Option<T>, Error> {
        Ok(Some(self.decode(s)?))
    }

    fn encode<'x>(&self, value: &'x Option<T>) -> Result<Option<Cow<'x, str>>, Error> {
        value
            .as_ref()
            .and_then(|x| self.encode(x).transpose())
            .transpose()
    }
}

/// A non-recurring instant with an explicit UTC offset and fractional seconds.
#[derive(Debug, Clone, PartialEq)]
pub struct DateTime(pub OffsetDateTime);

impl DateTime {
    /// Retrieves the associated UTC offset.
    pub fn timezone(&self) -> UtcOffset {
        self.0.offset()
    }

    /// Returns the same instant at a different UTC offset.
    pub fn with_timezone(&self, tz: UtcOffset) -> DateTime {
        DateTime(self.0.to_offset(tz))
    }

    /// Formats with a time format description.
    pub fn format(&self, fmt: &str) -> Result<String, time::Error> {
        Ok(self
            .0
            .format(&time::format_description::parse_borrowed::<2>(fmt)?)?)
    }

    // minidom conversion traits are infallible. Render the numeric fields
    // directly, retaining fractional precision and the explicit UTC offset.
    fn xml_string(&self) -> String {
        let dt = self.0;
        let mut text = format!(
            "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
            dt.year(),
            u8::from(dt.month()),
            dt.day(),
            dt.hour(),
            dt.minute(),
            dt.second()
        );
        if dt.nanosecond() != 0 {
            text.push('.');
            text.push_str(format!("{:09}", dt.nanosecond()).trim_end_matches('0'));
        }
        let seconds = dt.offset().whole_seconds();
        let sign = if seconds < 0 { '-' } else { '+' };
        let seconds = seconds.unsigned_abs();
        text.push_str(&format!(
            "{sign}{:02}:{:02}",
            seconds / 3600,
            seconds / 60 % 60
        ));
        text
    }
}

impl FromStr for DateTime {
    type Err = time::error::Parse;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(OffsetDateTime::parse(s, &Rfc3339)?))
    }
}

impl FromXmlText for DateTime {
    fn from_xml_text(s: String) -> Result<Self, Error> {
        s.parse().map_err(Error::text_parse_error)
    }
}

impl AsXmlText for DateTime {
    fn as_xml_text(&self) -> Result<Cow<'_, str>, Error> {
        Ok(Cow::Owned(self.xml_string()))
    }
}

impl IntoAttributeValue for DateTime {
    fn into_attribute_value(self) -> Option<String> {
        Some(self.xml_string())
    }
}

impl From<DateTime> for Node {
    fn from(date: DateTime) -> Node {
        Node::Text(date.xml_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_and_fractions_round_trip() {
        for text in [
            "2002-09-10T23:08:25Z",
            "2017-05-21T20:19:55+01:00",
            "2026-01-01T00:00:00.123456789-00:30",
        ] {
            let date: DateTime = text.parse().unwrap();
            let encoded = date.clone().into_attribute_value().unwrap();
            assert_eq!(encoded.parse::<DateTime>().unwrap(), date);
            assert_eq!(
                DateTime::from_xml_text(date.as_xml_text().unwrap().into_owned()).unwrap(),
                date
            );
        }
    }

    #[test]
    fn rejects_invalid_dates() {
        for text in [
            "2017-13-01T12:23:34Z",
            "2017-05-27T12:11:02+25:00",
            "2017-05-27T12:11:02+0100",
            "2017-05-27T12:11+01:00",
            "20170527T12:11:02+01:00",
            "2017-05-27T12:11:02",
        ] {
            assert!(text.parse::<DateTime>().is_err(), "{text}");
        }
    }

    #[test]
    fn offset_conversion_preserves_instant() {
        let date: DateTime = "2026-01-01T00:10:00.123Z".parse().unwrap();
        let converted = date.with_timezone(UtcOffset::from_hms(-1, 0, 0).unwrap());
        assert_eq!(
            date.0.unix_timestamp_nanos(),
            converted.0.unix_timestamp_nanos()
        );
        assert_eq!(converted.0.day(), 31);
    }
}
