// Copyright (c) 2019 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;
use crate::pubsub::PubSubPayload;

generate_elem_id!(
    /// The artist or performer of the song or piece.
    Artist,
    "artist",
    TUNE
);

generate_elem_id!(
    /// The duration of the song or piece in seconds.
    Length,
    "length",
    TUNE,
    u16
);

generate_elem_id!(
    /// The user's rating of the song or piece, from 1 (lowest) to 10 (highest).
    Rating,
    "rating",
    TUNE,
    u8
);

generate_elem_id!(
    /// The collection (e.g., album) or other source (e.g., a band website that hosts streams or
    /// audio files).
    Source,
    "source",
    TUNE
);

generate_elem_id!(
    /// The title of the song or piece.
    Title,
    "title",
    TUNE
);

generate_elem_id!(
    /// A unique identifier for the tune; e.g., the track number within a collection or the
    /// specific URI for the object (e.g., a stream or audio file).
    Track,
    "track",
    TUNE
);

generate_elem_id!(
    /// A URI or URL pointing to information about the song, collection, or artist.
    Uri,
    "uri",
    TUNE
);

/// Container for formatted text.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::TUNE, name = "tune")]
pub struct Tune {
    /// The artist or performer of the song or piece.
    #[xml(child(default))]
    artist: Option<Artist>,

    /// The duration of the song or piece in seconds.
    #[xml(child(default))]
    length: Option<Length>,

    /// The user's rating of the song or piece, from 1 (lowest) to 10 (highest).
    #[xml(child(default))]
    rating: Option<Rating>,

    /// The collection (e.g., album) or other source (e.g., a band website that hosts streams or
    /// audio files).
    #[xml(child(default))]
    source: Option<Source>,

    /// The title of the song or piece.
    #[xml(child(default))]
    title: Option<Title>,

    /// A unique identifier for the tune; e.g., the track number within a collection or the
    /// specific URI for the object (e.g., a stream or audio file).
    #[xml(child(default))]
    track: Option<Track>,

    /// A URI or URL pointing to information about the song, collection, or artist.
    #[xml(child(default))]
    uri: Option<Uri>,
}

impl PubSubPayload for Tune {}

impl Tune {
    /// Construct an empty `<tune/>` element.
    pub fn new() -> Tune {
        Tune {
            artist: None,
            length: None,
            rating: None,
            source: None,
            title: None,
            track: None,
            uri: None,
        }
    }
}

impl Default for Tune {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::str::FromStr;
    use minidom::Element;

    #[cfg(target_pointer_width = "32")]
    #[test]
    fn test_size() {
        assert_size!(Tune, 68);
        assert_size!(Artist, 12);
        assert_size!(Length, 2);
        assert_size!(Rating, 1);
        assert_size!(Source, 12);
        assert_size!(Title, 12);
        assert_size!(Track, 12);
        assert_size!(Uri, 12);
    }

    #[cfg(target_pointer_width = "64")]
    #[test]
    fn test_size() {
        assert_size!(Tune, 128);
        assert_size!(Artist, 24);
        assert_size!(Length, 2);
        assert_size!(Rating, 1);
        assert_size!(Source, 24);
        assert_size!(Title, 24);
        assert_size!(Track, 24);
        assert_size!(Uri, 24);
    }

    #[test]
    fn empty() {
        let elem: Element = "<tune xmlns='http://jabber.org/protocol/tune'/>"
            .parse()
            .unwrap();
        let elem2 = elem.clone();
        let tune = Tune::try_from(elem).unwrap();
        assert!(tune.artist.is_none());
        assert!(tune.length.is_none());
        assert!(tune.rating.is_none());
        assert!(tune.source.is_none());
        assert!(tune.title.is_none());
        assert!(tune.track.is_none());
        assert!(tune.uri.is_none());

        let elem3 = tune.into();
        assert_eq!(elem2, elem3);
    }

    #[test]
    fn full() {
        let elem: Element = "<tune xmlns='http://jabber.org/protocol/tune'><artist>Yes</artist><length>686</length><rating>8</rating><source>Yessongs</source><title>Heart of the Sunrise</title><track>3</track><uri>http://www.yesworld.com/lyrics/Fragile.html#9</uri></tune>"
            .parse()
            .unwrap();
        let tune = Tune::try_from(elem).unwrap();
        assert_eq!(tune.artist, Some(Artist::from_str("Yes").unwrap()));
        assert_eq!(tune.length, Some(Length(686)));
        assert_eq!(tune.rating, Some(Rating(8)));
        assert_eq!(tune.source, Some(Source::from_str("Yessongs").unwrap()));
        assert_eq!(
            tune.title,
            Some(Title::from_str("Heart of the Sunrise").unwrap())
        );
        assert_eq!(tune.track, Some(Track::from_str("3").unwrap()));
        assert_eq!(
            tune.uri,
            Some(Uri::from_str("http://www.yesworld.com/lyrics/Fragile.html#9").unwrap())
        );
    }
}
