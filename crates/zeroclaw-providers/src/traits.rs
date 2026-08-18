pub use zeroclaw_api::model_provider::*;

/// Marks a provider failure as definitively non-retryable without coupling
/// shared retry policy to provider-specific error text.
#[derive(Debug)]
pub struct NonRetryableProviderError {
    message: String,
}

impl NonRetryableProviderError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for NonRetryableProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NonRetryableProviderError {}
