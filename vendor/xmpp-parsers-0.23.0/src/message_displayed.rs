// Copyright (c) 2024 Emmanuel Gil Peyrot <linkmauve@linkmauve.fr>
//
// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at http://mozilla.org/MPL/2.0/.

use xso::{AsXml, FromXml};

use crate::ns;
use crate::stanza_id::StanzaId;

/// Mention that a particular message has been displayed by at least one client.
#[derive(FromXml, AsXml, PartialEq, Debug, Clone)]
#[xml(namespace = ns::MDS, name = "displayed")]
pub struct Displayed {
    /// Reference to the message having been displayed.
    #[xml(child)]
    pub stanza_id: StanzaId,
}
