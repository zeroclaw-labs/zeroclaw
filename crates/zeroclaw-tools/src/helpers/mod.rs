pub mod domain_guard;
pub(crate) mod response_body;

/// Read bounded HTTP response text with UTF-8-safe truncation.
pub use response_body::read_text as read_response_text;
