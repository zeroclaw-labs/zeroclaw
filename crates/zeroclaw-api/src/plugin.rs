//! Shared plugin identifiers used across configuration and runtime crates.

/// A plugin package name that does not satisfy the canonical slug grammar.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error(
    "plugin package must be a 1-128 character lowercase ASCII slug, start with a letter or digit, end with a letter or digit, and use only '-', '_', or '.' as separators (got {name:?})"
)]
pub struct InvalidPluginPackageName {
    name: String,
}

/// Validate the package-name grammar shared by manifests, configuration, and
/// runtime instance construction.
///
/// One grammar keeps `[channels.plugin.<alias>].package` and the installed
/// manifest's `name` on the same spelling, so a declaration that passes config
/// validation cannot fail to match an admitted package for a reason the
/// operator never sees. The grammar is deliberately narrow: no uppercase, no
/// path separators, and no whitespace or control characters, so a package name
/// can be embedded in a log field or a derived key without quoting.
///
/// # Errors
///
/// Returns [`InvalidPluginPackageName`] when `name` is empty, longer than 128
/// bytes, or is not a lowercase ASCII slug with supported separators.
pub fn validate_plugin_package_name(name: &str) -> Result<(), InvalidPluginPackageName> {
    let bytes = name.as_bytes();
    let valid = (1..=128).contains(&bytes.len())
        && bytes
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit());
    let valid = valid
        && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
        && bytes.iter().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'.')
        });
    if !valid {
        return Err(InvalidPluginPackageName {
            name: name.to_string(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn package_name_grammar_accepts_canonical_slugs() {
        for name in ["a", "chat", "acme.chat", "chat-v2", "chat_bridge", "s3"] {
            assert!(validate_plugin_package_name(name).is_ok(), "{name}");
        }
    }

    #[test]
    fn package_name_grammar_rejects_ambiguous_or_unsafe_names() {
        for name in [
            "",
            "Chat",
            ".chat",
            "chat.",
            "chat-",
            "-chat",
            "chat/plugin",
            "../escape",
            "chat plugin",
            "chat\nplugin",
            &"a".repeat(129),
        ] {
            assert!(validate_plugin_package_name(name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn package_name_grammar_accepts_the_longest_supported_name() {
        assert!(validate_plugin_package_name(&"a".repeat(128)).is_ok());
    }
}
