#[path = "common/spec.rs"]
mod spec;

use std::collections::VecDeque;
use std::io::Cursor;

use async_trait::async_trait;
use spec::{INSTANCE, SECTION, completed_outcome, matrix_spec};
use tempfile::TempDir;
use zeroclaw_config::schema::{Config, MatrixConfig};
use zeroclaw_onboarding::{
    CliSecretSource, CliTransport, LlmResponder, LlmTransport, SecretReader, section_fields,
};
use zeroclaw_runtime::flow::Outcome;

fn fresh_config() -> (TempDir, Config) {
    let tmp = TempDir::new().unwrap();
    let mut config = Config {
        config_path: tmp.path().join("config.toml"),
        ..Default::default()
    };
    config
        .channels
        .matrix
        .insert(INSTANCE.to_string(), MatrixConfig::default());
    (tmp, config)
}

/// One scripted reply queue wearing all three response-source hats, so the
/// parity test scripts CLI and LLM walks through the same shape.
struct Scripted {
    replies: VecDeque<String>,
}

impl Scripted {
    fn new(replies: Vec<String>) -> Self {
        Self {
            replies: replies.into(),
        }
    }

    fn pop(&mut self) -> zeroclaw_runtime::flow::TransportResult<String> {
        self.replies
            .pop_front()
            .ok_or(zeroclaw_runtime::flow::TransportError::Closed)
    }
}

#[async_trait]
impl LlmResponder for Scripted {
    async fn respond(
        &mut self,
        _prompt_text: &str,
    ) -> zeroclaw_runtime::flow::TransportResult<String> {
        self.pop()
    }
}

#[async_trait]
impl SecretReader for Scripted {
    async fn read_secret(
        &mut self,
        _prompt_text: &str,
    ) -> zeroclaw_runtime::flow::TransportResult<String> {
        self.pop()
    }
}

#[async_trait]
impl CliSecretSource for Scripted {
    async fn read_secret(
        &mut self,
        _prompt_text: &str,
    ) -> zeroclaw_runtime::flow::TransportResult<String> {
        self.pop()
    }
}

/// The same field-ordered answer script both transports consume: non-secret
/// answers in walk order plus the secret answers routed to the secret source.
fn scripts(config: &Config) -> (Vec<String>, Vec<String>) {
    let mut fields = section_fields(config.prop_fields(), SECTION);
    fields.sort_by(|a, b| a.name.cmp(&b.name));
    let mut answers = Vec::new();
    let mut secrets = Vec::new();
    for field in fields {
        if field.is_secret {
            secrets.push("sk-secret".to_string());
        } else {
            answers.push(answer_for(&field));
        }
    }
    (answers, secrets)
}

fn answer_for(field: &zeroclaw_config::traits::PropFieldInfo) -> String {
    use zeroclaw_config::traits::PropKind;
    match field.kind {
        PropKind::Bool => "yes".to_string(),
        PropKind::Enum => field.display_value.clone(),
        PropKind::Integer => "1".to_string(),
        PropKind::Float => "1.0".to_string(),
        PropKind::StringArray => "room-a".to_string(),
        _ => "value".to_string(),
    }
}

async fn walk_cli(config: &mut Config, answers: &[String], secrets: Vec<String>) -> Outcome {
    let spec = matrix_spec();
    let mut output: Vec<u8> = Vec::new();
    let mut script = answers.join("\n");
    script.push('\n');
    let mut transport = CliTransport::with_secret_source(
        Cursor::new(script.into_bytes()),
        &mut output,
        Scripted::new(secrets),
    );
    spec.walk(&mut transport, config).await.unwrap()
}

async fn walk_llm(config: &mut Config, answers: Vec<String>, secrets: Vec<String>) -> Outcome {
    let spec = matrix_spec();
    let mut transport = LlmTransport::new(Scripted::new(answers), Scripted::new(secrets));
    spec.walk(&mut transport, config).await.unwrap()
}

#[tokio::test]
async fn cli_and_llm_build_identical_config_from_registry_spec() {
    let (_cli_tmp, mut cli_config) = fresh_config();
    let cli_outcome = {
        let (answers, secrets) = scripts(&cli_config);
        walk_cli(&mut cli_config, &answers, secrets).await
    };

    let (_llm_tmp, mut llm_config) = fresh_config();
    let llm_outcome = {
        let (answers, secrets) = scripts(&llm_config);
        walk_llm(&mut llm_config, answers, secrets).await
    };

    assert_eq!(cli_outcome, llm_outcome);
    assert_eq!(cli_outcome, completed_outcome());

    let matrix_cli = cli_config.channels.matrix.get(INSTANCE).unwrap();
    let matrix_llm = llm_config.channels.matrix.get(INSTANCE).unwrap();
    assert_eq!(matrix_cli.homeserver, matrix_llm.homeserver);
    assert_eq!(matrix_cli.mention_only, matrix_llm.mention_only);
    assert_eq!(matrix_cli.access_token, matrix_llm.access_token);
}
