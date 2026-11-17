// Copyright (c) 2024 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;
use core::num::NonZeroU32;

/// Advertises limits on this stream.
#[derive(FromXml, AsXml, Debug, Clone, PartialEq)]
#[xml(namespace = ns::STREAM_LIMITS, name = "limits")]
pub struct Limits {
    /// Maximum size of any first-level stream elements (including stanzas), in bytes the
    /// announcing entity is willing to accept.
    #[xml(extract(default, name = "max-bytes", fields(text(type_ = NonZeroU32))))]
    pub max_bytes: Option<NonZeroU32>,

    /// Number of seconds without any traffic from the initiating entity after which the server may
    /// consider the stream idle, and either perform liveness checks or terminate the stream.
    #[xml(extract(default, name = "idle-seconds", fields(text(type_ = NonZeroU32))))]
    pub idle_seconds: Option<NonZeroU32>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use minidom::Element;

    #[test]
    fn test_size() {
        assert_size!(Limits, 8);
    }

    #[test]
    fn test_simple() {
        let elem: Element =
            "<limits xmlns='urn:xmpp:stream-limits:0'><max-bytes>262144</max-bytes></limits>"
                .parse()
                .unwrap();
        let limits = Limits::try_from(elem).unwrap();
        assert_eq!(limits.max_bytes.unwrap(), NonZeroU32::new(262144).unwrap());
        assert!(limits.idle_seconds.is_none());
    }
}
