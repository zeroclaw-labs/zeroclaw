use crate::response_type::ResponseValue;
use zeroclaw_config::schema::Config;

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Config(#[from] anyhow::Error),
    #[error("filesystem write to {path} failed: {source}")]
    Filesystem {
        path: String,
        source: std::io::Error,
    },
}

/// Where a node's validated response is written. The default is a config
/// property; personality-file nodes instead write into the agent workspace, so
/// the seam carries both the config-prop path and the two workspace-file
/// shapes. A workspace file either takes the operator's authored response or a
/// fixed pre-rendered template body baked at spec-build time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteTarget {
    ConfigProp {
        prop: String,
    },
    WorkspaceFileFromResponse {
        agent_alias: String,
        filename: String,
    },
    WorkspaceFileLiteral {
        agent_alias: String,
        filename: String,
        content: String,
    },
}

pub fn write_to_target(
    config: &mut Config,
    target: &WriteTarget,
    response: &ResponseValue,
) -> Result<(), WriteError> {
    match target {
        WriteTarget::ConfigProp { prop } => write_response(config, prop, response),
        WriteTarget::WorkspaceFileFromResponse {
            agent_alias,
            filename,
        } => write_workspace_file(config, agent_alias, filename, &response_text(response)),
        WriteTarget::WorkspaceFileLiteral {
            agent_alias,
            filename,
            content,
        } => write_workspace_file(config, agent_alias, filename, content),
    }
}

fn response_text(response: &ResponseValue) -> String {
    match response {
        ResponseValue::Secret(secret) => secret.expose().to_string(),
        ResponseValue::FreeformText(text) => text.clone(),
        ResponseValue::Number(number) => number.clone(),
        ResponseValue::Choice(choice) => choice.clone(),
        ResponseValue::YesNo(value) => {
            if *value {
                "true".to_string()
            } else {
                "false".to_string()
            }
        }
    }
}

fn write_workspace_file(
    config: &Config,
    agent_alias: &str,
    filename: &str,
    content: &str,
) -> Result<(), WriteError> {
    let workspace = config.agent_workspace_dir(agent_alias);
    let path = workspace.join(filename);
    std::fs::create_dir_all(&workspace).map_err(|source| WriteError::Filesystem {
        path: workspace.display().to_string(),
        source,
    })?;
    std::fs::write(&path, content).map_err(|source| WriteError::Filesystem {
        path: path.display().to_string(),
        source,
    })
}

pub fn write_response(
    config: &mut Config,
    prop: &str,
    response: &ResponseValue,
) -> Result<(), WriteError> {
    match response {
        ResponseValue::Secret(secret) => {
            config.set_secret_persistent(prop, secret.expose().to_string())?;
        }
        ResponseValue::YesNo(value) => {
            let rendered = if *value { "true" } else { "false" };
            config.set_prop_persistent(prop, rendered)?;
        }
        ResponseValue::FreeformText(text) => {
            config.set_prop_persistent(prop, text)?;
        }
        ResponseValue::Number(number) => {
            config.set_prop_persistent(prop, number)?;
        }
        ResponseValue::Choice(selected) => {
            config.set_prop_persistent(prop, selected)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::response_type::SecretValue;
    use tempfile::TempDir;
    use zeroclaw_config::schema::MatrixConfig;

    const ALIAS: &str = "home";

    fn config_with_matrix() -> (TempDir, Config) {
        let tmp = TempDir::new().unwrap();
        let mut config = Config {
            config_path: tmp.path().join("config.toml"),
            ..Default::default()
        };
        config
            .channels
            .matrix
            .insert(ALIAS.to_string(), MatrixConfig::default());
        (tmp, config)
    }

    fn path(field: &str) -> String {
        format!("channels.matrix.{ALIAS}.{field}")
    }

    #[test]
    fn yes_no_writes_bool_prop() {
        let (_tmp, mut config) = config_with_matrix();
        write_response(
            &mut config,
            &path("mention_only"),
            &ResponseValue::YesNo(true),
        )
        .unwrap();
        assert_eq!(config.get_prop(&path("mention_only")).unwrap(), "true");
    }

    #[test]
    fn yes_no_false_writes_false() {
        let (_tmp, mut config) = config_with_matrix();
        write_response(
            &mut config,
            &path("mention_only"),
            &ResponseValue::YesNo(false),
        )
        .unwrap();
        assert_eq!(config.get_prop(&path("mention_only")).unwrap(), "false");
    }

    #[test]
    fn freeform_text_writes_string_prop() {
        let (_tmp, mut config) = config_with_matrix();
        write_response(
            &mut config,
            &path("homeserver"),
            &ResponseValue::FreeformText("https://example.org".into()),
        )
        .unwrap();
        assert_eq!(
            config.get_prop(&path("homeserver")).unwrap(),
            "https://example.org"
        );
    }

    #[test]
    fn choice_writes_selected_value() {
        let (_tmp, mut config) = config_with_matrix();
        write_response(
            &mut config,
            &path("stream_mode"),
            &ResponseValue::Choice("partial".into()),
        )
        .unwrap();
        assert_eq!(config.get_prop(&path("stream_mode")).unwrap(), "partial");
    }

    #[test]
    fn secret_writes_through_secret_path_and_never_appears_in_plaintext() {
        let (_tmp, mut config) = config_with_matrix();
        write_response(
            &mut config,
            "channels.matrix.access_token",
            &ResponseValue::Secret(SecretValue::new("sk-super-secret".into())),
        )
        .unwrap();
        assert_eq!(
            config
                .channels
                .matrix
                .get(ALIAS)
                .unwrap()
                .access_token
                .as_deref(),
            Some("sk-super-secret")
        );
        assert!(
            !config
                .get_prop(&path("access_token"))
                .unwrap()
                .contains("sk-super-secret")
        );
    }

    #[test]
    fn unknown_prop_path_errors() {
        let (_tmp, mut config) = config_with_matrix();
        let result = write_response(
            &mut config,
            "nonexistent.path",
            &ResponseValue::FreeformText("x".into()),
        );
        assert!(result.is_err());
    }

    fn config_with_agent(tmp: &TempDir) -> Config {
        Config {
            config_path: tmp.path().join("config.toml"),
            data_dir: tmp.path().to_path_buf(),
            ..Default::default()
        }
    }

    #[test]
    fn workspace_file_literal_writes_content_into_agent_workspace() {
        let tmp = TempDir::new().unwrap();
        let mut config = config_with_agent(&tmp);
        let target = WriteTarget::WorkspaceFileLiteral {
            agent_alias: "scout".to_string(),
            filename: "SOUL.md".to_string(),
            content: "rendered template body".to_string(),
        };
        write_to_target(
            &mut config,
            &target,
            &ResponseValue::Choice("template".into()),
        )
        .unwrap();
        let written =
            std::fs::read_to_string(config.agent_workspace_dir("scout").join("SOUL.md")).unwrap();
        assert_eq!(written, "rendered template body");
    }

    #[test]
    fn workspace_file_from_response_writes_authored_text() {
        let tmp = TempDir::new().unwrap();
        let mut config = config_with_agent(&tmp);
        let target = WriteTarget::WorkspaceFileFromResponse {
            agent_alias: "scout".to_string(),
            filename: "IDENTITY.md".to_string(),
        };
        write_to_target(
            &mut config,
            &target,
            &ResponseValue::FreeformText("authored identity".into()),
        )
        .unwrap();
        let written =
            std::fs::read_to_string(config.agent_workspace_dir("scout").join("IDENTITY.md"))
                .unwrap();
        assert_eq!(written, "authored identity");
    }

    #[test]
    fn config_prop_target_routes_through_config_write() {
        let (_tmp, mut config) = config_with_matrix();
        let target = WriteTarget::ConfigProp {
            prop: path("homeserver"),
        };
        write_to_target(
            &mut config,
            &target,
            &ResponseValue::FreeformText("https://target.test".into()),
        )
        .unwrap();
        assert_eq!(
            config.get_prop(&path("homeserver")).unwrap(),
            "https://target.test"
        );
    }
}
