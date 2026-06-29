use crate::response_type::ResponseValue;
use zeroclaw_config::schema::Config;

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error(transparent)]
    Config(#[from] anyhow::Error),
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
        write_response(&mut config, &path("mention_only"), &ResponseValue::YesNo(true)).unwrap();
        assert_eq!(config.get_prop(&path("mention_only")).unwrap(), "true");
    }

    #[test]
    fn yes_no_false_writes_false() {
        let (_tmp, mut config) = config_with_matrix();
        write_response(&mut config, &path("mention_only"), &ResponseValue::YesNo(false)).unwrap();
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
            config.channels.matrix.get(ALIAS).unwrap().access_token.as_deref(),
            Some("sk-super-secret")
        );
        assert!(!config.get_prop(&path("access_token")).unwrap().contains("sk-super-secret"));
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
}
