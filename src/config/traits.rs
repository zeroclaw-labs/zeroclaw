/// Describes a single secret field discovered via `#[derive(HasSecrets)]`.
#[derive(Debug, Clone)]
pub struct SecretFieldInfo {
    /// Full dotted name (e.g. `channels.matrix.access-token`)
    pub name: &'static str,
    /// Category for grouping in `zeroclaw secret list`
    pub category: &'static str,
    /// Whether this field currently has a non-empty value
    pub is_set: bool,
}

/// The trait for describing a channel
pub trait ChannelConfig {
    /// human-readable name
    fn name() -> &'static str;
    /// short description
    fn desc() -> &'static str;
}

// Maybe there should be a `&self` as parameter for custom channel/info or what...

pub trait ConfigHandle {
    fn name(&self) -> &'static str;
    fn desc(&self) -> &'static str;
}
