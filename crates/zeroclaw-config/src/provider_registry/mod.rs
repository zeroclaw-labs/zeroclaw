//! Canonical provider connections and independently named capability models.
//!
//! Configuration stores explicit definitions only. Effective options are
//! resolved per admitted generation, never written into a second registry.

mod api;
mod connection;
mod ids;
mod model;
mod options;
mod override_value;
mod registry;
mod resolve;

pub use api::*;
pub use connection::*;
pub use ids::{ModelRef, PresetId, ProviderId};
pub use model::*;
pub use options::{
    AsrOptionsPatch, ChatOptionsPatch, ModelPreset, Preset, TextExtraction, TtsOptionsPatch,
};
pub use override_value::Override;
pub use registry::{ModelSelection, ProviderRegistry, RegistryError};
pub use resolve::{ResolvedAsr, ResolvedChat, ResolvedDecision, ResolvedEmbedding, ResolvedTts};
