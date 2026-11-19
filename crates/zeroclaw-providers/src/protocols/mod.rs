//! Wire protocol implementations shared by provider vendors.
//!
//! These modules do not select a provider kind or inspect the config registry.
//! Capability factories resolve vendor policy before invoking a protocol.

pub(crate) mod openai;
