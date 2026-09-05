//! Safe, platform-native opening of validated HTTP(S) links.

use std::process::Stdio;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchSpec {
    pub(crate) program: &'static str,
    pub(crate) argument: String,
}

/// Validate a transcript URL before it reaches an operating-system launcher.
pub(crate) fn validate_url(input: &str) -> anyhow::Result<String> {
    if input
        .chars()
        .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        anyhow::bail!("URL contains whitespace or control characters");
    }
    let authority = input
        .strip_prefix("https://")
        .or_else(|| input.strip_prefix("http://"))
        .ok_or_else(|| anyhow::Error::msg("only HTTP(S) URLs can be opened"))?;
    if authority.is_empty() || authority.starts_with('/') {
        anyhow::bail!("URL is missing a host");
    }
    let parsed = url::Url::parse(input)?;
    if parsed.host().is_none() {
        anyhow::bail!("URL is missing a host");
    }
    Ok(input.to_string())
}

pub(crate) fn launch_spec(input: &str) -> anyhow::Result<LaunchSpec> {
    let argument = validate_url(input)?;
    let program = platform_program()?;

    Ok(LaunchSpec { program, argument })
}

fn platform_program() -> anyhow::Result<&'static str> {
    #[cfg(target_os = "macos")]
    let program = "/usr/bin/open";
    #[cfg(target_os = "linux")]
    let program = "xdg-open";
    #[cfg(target_os = "windows")]
    let program = "explorer.exe";

    #[cfg(any(target_os = "macos", target_os = "linux", target_os = "windows"))]
    {
        Ok(program)
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        anyhow::bail!("opening links is unsupported on this platform")
    }
}

/// Spawn the platform browser launcher without blocking the TUI event loop.
pub(crate) async fn open(input: &str) -> anyhow::Result<()> {
    let spec = launch_spec(input)?;
    let mut child = tokio::process::Command::new(spec.program)
        .arg(spec.argument)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    tokio::spawn(async move {
        let _ = child.wait().await;
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_http_and_https_with_hosts() {
        assert_eq!(
            validate_url("https://example.com/a?b=c").unwrap(),
            "https://example.com/a?b=c"
        );
        assert!(validate_url("http://localhost:8080").is_ok());
    }

    #[test]
    fn rejects_unsafe_or_malformed_urls() {
        for input in [
            "javascript:alert(1)",
            "file:///tmp/a",
            "https://",
            "https:///path",
            "https://example.com/a b",
            "https://example.com/\u{0000}",
        ] {
            assert!(validate_url(input).is_err(), "accepted {input:?}");
        }
    }

    #[test]
    fn constructs_one_argument_platform_command() {
        let spec = launch_spec("https://example.com").unwrap();
        assert_eq!(spec.argument, "https://example.com");
        #[cfg(target_os = "macos")]
        assert_eq!(spec.program, "/usr/bin/open");
        #[cfg(target_os = "linux")]
        assert_eq!(spec.program, "xdg-open");
        #[cfg(target_os = "windows")]
        assert_eq!(spec.program, "explorer.exe");
    }

    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    #[test]
    fn rejects_launches_on_unsupported_platforms() {
        let error = launch_spec("https://example.com").unwrap_err();
        assert_eq!(
            error.to_string(),
            "opening links is unsupported on this platform"
        );
    }
}
