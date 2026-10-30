//! Disposable full-text projection over an already authorized message window.
//! Positions refer only to the supplied window, never to a global lookup.
use super::ReadError;

pub trait SessionSearchPort: Send + Sync {
    fn matching_positions(
        &self,
        texts: &[&str],
        query: &str,
        limit: usize,
    ) -> Result<Vec<usize>, ReadError>;
}
