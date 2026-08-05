use zeroclaw_config::platform::docker::DockerWorkspaceMountError;

pub(super) fn format_runtime_command_error(error: &anyhow::Error) -> String {
    if let Some(error) = error.downcast_ref::<DockerWorkspaceMountError>() {
        let (key, path, cause) = match error {
            DockerWorkspaceMountError::WorkspacePath { path, source } => (
                "tool-runtime-command-docker-workspace-path",
                path.as_str(),
                source.to_string(),
            ),
            DockerWorkspaceMountError::AllowedRoot { path, source } => (
                "tool-runtime-command-docker-allowed-root",
                path.as_str(),
                source.to_string(),
            ),
        };
        return crate::i18n::get_required_cli_string_with_args(
            key,
            &[("path", path), ("cause", cause.as_str())],
        );
    }

    let message = format!("{error:#}");
    crate::i18n::get_required_cli_string_with_args(
        "tool-runtime-command-build-failed",
        &[("error", message.as_str())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_allowed_root_error_preserves_path_and_os_cause_through_context() {
        let error = anyhow::Error::new(DockerWorkspaceMountError::AllowedRoot {
            path: "/missing/root".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "fixture OS cause"),
        })
        .context("outer runtime context");

        let message = format_runtime_command_error(&error);

        assert!(message.contains("/missing/root"));
        assert!(message.contains("fixture OS cause"));
    }

    #[test]
    fn docker_workspace_path_error_preserves_path_and_os_cause_through_context() {
        let error = anyhow::Error::new(DockerWorkspaceMountError::WorkspacePath {
            path: "/missing/workspace".to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "fixture OS cause"),
        })
        .context("outer runtime context");

        let message = format_runtime_command_error(&error);

        assert!(message.contains("/missing/workspace"));
        assert!(message.contains("fixture OS cause"));
    }

    #[test]
    fn generic_runtime_error_preserves_the_full_chain() {
        let error = anyhow::Error::msg("inner runtime cause").context("outer runtime context");

        let message = format_runtime_command_error(&error);

        assert!(message.contains("outer runtime context"));
        assert!(message.contains("inner runtime cause"));
    }
}
