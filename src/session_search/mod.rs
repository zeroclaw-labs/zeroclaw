//! Cross-Session Recall — FTS5 search over past conversation transcripts.
//!
//! While `unified_search` queries memories and documents, session_search
//! queries raw conversation history — answering questions like
//! "what did we discuss last week about X?"

pub mod factory;
pub mod lifecycle;
pub mod schema;
pub mod store;

pub use factory::build_store;
pub use lifecycle::SessionHandle;
pub use store::{ChatMessage, ChatSession, SessionSearchHit, SessionSearchStore};
