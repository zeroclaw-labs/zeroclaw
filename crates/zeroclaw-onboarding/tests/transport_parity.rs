use std::collections::VecDeque;
use std::io::Cursor;

use async_trait::async_trait;
use tempfile::TempDir;
use zeroclaw_config::schema::{Config, MatrixConfig};
use zeroclaw_onboarding::{
    CliTransport, LlmResponder, LlmTransport, SecretReader, build_spec, section_fields,
};
use zeroclaw_runtime::flow::{ConfiguredItem, Outcome};

const SECTION: &str = "channels.matrix.home";

fn completed() -> Outcome {
    Outcome::Completed {
        configured: vec![ConfiguredItem {
            layer: "channel".into(),
            instance: "home".into(),
        }],
    }
}

fn fresh_config() -> (TempDir, Config) {
    let tmp = TempDir::new().unwrap();
    let mut config = Config {
        config_path: tmp.path().join("config.toml"),
        ..Default::default()
    };
    config
        .channels
        .matrix
        .insert("home".to_string(), MatrixConfig::default());
    (tmp, config)
}

struct ScriptedResponder {
    replies: VecDeque<String>,
}

#[async_trait]
impl LlmResponder for ScriptedResponder {
    async fn respond(
        &mut self,
        _prompt_text: &str,
    ) -> zeroclaw_runtime::flow::TransportResult<String> {
        self.replies
            .pop_front()
            .ok_or(zeroclaw_runtime::flow::TransportError::Closed)
    }
}

struct ScriptedSecretReader {
    replies: VecDeque<String>,
}

#[async_trait]
impl SecretReader for ScriptedSecretReader {
    async fn read_secret(
        &mut self,
        _prompt_text: &str,
    ) -> zeroclaw_runtime::flow::TransportResult<String> {
        self.replies
            .pop_front()
            .ok_or(zeroclaw_runtime::flow::TransportError::Closed)
    }
}

fn ordered_fields(config: &Config) -> Vec<zeroclaw_config::traits::PropFieldInfo> {
    let mut fields = section_fields(config.prop_fields(), SECTION);
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    fields
}

fn cli_script(config: &Config) -> String {
    let mut script = String::new();
    for field in ordered_fields(config) {
        let line = answer_for(&field);
        script.push_str(&line);
        script.push('\n');
    }
    script
}

fn llm_scripts(config: &Config) -> (Vec<String>, Vec<String>) {
    let mut llm = Vec::new();
    let mut secrets = Vec::new();
    for field in ordered_fields(config) {
        let answer = answer_for(&field);
        if field.is_secret {
            secrets.push(answer);
        } else {
            llm.push(answer);
        }
    }
    (llm, secrets)
}

fn answer_for(field: &zeroclaw_config::traits::PropFieldInfo) -> String {
    use zeroclaw_config::traits::PropKind;
    if field.is_secret {
        return "sk-secret".to_string();
    }
    match field.kind {
        PropKind::Bool => "yes".to_string(),
        PropKind::Enum => field.display_value.clone(),
        PropKind::Integer => "1".to_string(),
        PropKind::Float => "1.0".to_string(),
        PropKind::StringArray => "room-a".to_string(),
        _ => "value".to_string(),
    }
}

async fn walk_cli(config: &mut Config, script: &str) -> Outcome {
    let spec = build_spec(
        config.prop_fields(),
        SECTION,
        "channel",
        "home",
        completed(),
    )
    .expect("matrix section yields a spec");
    let mut output: Vec<u8> = Vec::new();
    let mut transport = CliTransport::new(Cursor::new(script.as_bytes().to_vec()), &mut output);
    spec.walk(&mut transport, config).await.unwrap()
}

async fn walk_llm(config: &mut Config, llm: Vec<String>, secrets: Vec<String>) -> Outcome {
    let spec = build_spec(
        config.prop_fields(),
        SECTION,
        "channel",
        "home",
        completed(),
    )
    .expect("matrix section yields a spec");
    let responder = ScriptedResponder {
        replies: llm.into(),
    };
    let secret_reader = ScriptedSecretReader {
        replies: secrets.into(),
    };
    let mut transport = LlmTransport::new(responder, secret_reader);
    spec.walk(&mut transport, config).await.unwrap()
}

#[tokio::test]
async fn cli_and_llm_build_identical_config_from_registry_spec() {
    let (_cli_tmp, mut cli_config) = fresh_config();
    let cli_outcome = {
        let script = cli_script(&cli_config);
        walk_cli(&mut cli_config, &script).await
    };

    let (_llm_tmp, mut llm_config) = fresh_config();
    let llm_outcome = {
        let (llm, secrets) = llm_scripts(&llm_config);
        walk_llm(&mut llm_config, llm, secrets).await
    };

    assert_eq!(cli_outcome, llm_outcome);
    assert_eq!(cli_outcome, completed());

    let matrix_cli = cli_config.channels.matrix.get("home").unwrap();
    let matrix_llm = llm_config.channels.matrix.get("home").unwrap();
    assert_eq!(matrix_cli.homeserver, matrix_llm.homeserver);
    assert_eq!(matrix_cli.mention_only, matrix_llm.mention_only);
    assert_eq!(matrix_cli.access_token, matrix_llm.access_token);
}
