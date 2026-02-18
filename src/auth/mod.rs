//! Authentication module for multi-provider OAuth and token flows.
//!
//! Provides unified entry points for authenticating with various AI providers:
//!
//! - **OpenAI Codex** — ChatGPT OAuth (Authorization Code + PKCE)
//! - **Anthropic** — Setup-token paste (`sk-ant-oat01-*`)
//! - **Google Gemini CLI** — Google OAuth (Authorization Code + PKCE)
//! - **Google Antigravity** — Cloud Code Assist OAuth (Authorization Code + PKCE)
//!
//! Each authentication method is implemented as an independent submodule
//! that extends the corresponding provider's credential resolution.

pub mod anthropic_setup_token;
pub mod antigravity_oauth;
pub mod codex_oauth;
pub mod common;
pub mod gemini_cli_oauth;

/// Supported authentication methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthMethod {
    /// OpenAI Codex — ChatGPT OAuth (browser-based authorization)
    CodexOAuth,
    /// Anthropic — paste setup-token from `claude setup-token`
    AnthropicSetupToken,
    /// Google Gemini CLI — Google OAuth (browser-based authorization)
    GeminiCliOAuth,
    /// Google Antigravity — Cloud Code Assist OAuth (browser-based authorization)
    AntigravityOAuth,
}

impl AuthMethod {
    /// User-facing display name for this authentication method.
    pub fn display_name(self) -> &'static str {
        match self {
            Self::CodexOAuth => "OpenAI Codex (ChatGPT OAuth)",
            Self::AnthropicSetupToken => "Anthropic (Setup Token)",
            Self::GeminiCliOAuth => "Google Gemini CLI (OAuth)",
            Self::AntigravityOAuth => "Google Antigravity (Cloud Code Assist OAuth)",
        }
    }

    /// The provider name used in config and factory registration.
    pub fn provider_name(self) -> &'static str {
        match self {
            Self::CodexOAuth => "openai-codex",
            Self::AnthropicSetupToken => "anthropic",
            Self::GeminiCliOAuth => "gemini",
            Self::AntigravityOAuth => "google-antigravity",
        }
    }

    /// Whether this auth method requires a browser for interactive login.
    pub fn requires_browser(self) -> bool {
        match self {
            Self::CodexOAuth | Self::GeminiCliOAuth | Self::AntigravityOAuth => true,
            Self::AnthropicSetupToken => false,
        }
    }
}

impl std::fmt::Display for AuthMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.display_name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_method_display_names_are_non_empty() {
        let methods = [
            AuthMethod::CodexOAuth,
            AuthMethod::AnthropicSetupToken,
            AuthMethod::GeminiCliOAuth,
            AuthMethod::AntigravityOAuth,
        ];
        for method in &methods {
            assert!(!method.display_name().is_empty());
            assert!(!method.provider_name().is_empty());
        }
    }

    #[test]
    fn auth_method_provider_names_are_lowercase() {
        let methods = [
            AuthMethod::CodexOAuth,
            AuthMethod::AnthropicSetupToken,
            AuthMethod::GeminiCliOAuth,
            AuthMethod::AntigravityOAuth,
        ];
        for method in &methods {
            let name = method.provider_name();
            assert_eq!(name, name.to_lowercase());
        }
    }

    #[test]
    fn anthropic_setup_token_does_not_require_browser() {
        assert!(!AuthMethod::AnthropicSetupToken.requires_browser());
    }

    #[test]
    fn oauth_methods_require_browser() {
        assert!(AuthMethod::CodexOAuth.requires_browser());
        assert!(AuthMethod::GeminiCliOAuth.requires_browser());
        assert!(AuthMethod::AntigravityOAuth.requires_browser());
    }
}
