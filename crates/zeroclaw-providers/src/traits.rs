pub use zeroclaw_api::model_provider::*;

/// Marks a provider failure as definitively non-retryable without coupling
/// shared retry policy to provider-specific error text.
#[derive(Debug)]
pub struct NonRetryableProviderError {
    message: String,
    source: Option<Box<dyn std::error::Error + Send + Sync>>,
}

impl NonRetryableProviderError {
    #[must_use]
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            source: None,
        }
    }

    #[must_use]
    pub fn with_source(
        message: impl Into<String>,
        source: impl std::error::Error + Send + Sync + 'static,
    ) -> Self {
        Self {
            message: message.into(),
            source: Some(Box::new(source)),
        }
    }
}

impl std::fmt::Display for NonRetryableProviderError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NonRetryableProviderError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.source
            .as_deref()
            .map(|source| source as &(dyn std::error::Error + 'static))
    }
}
