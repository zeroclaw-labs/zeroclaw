use zeroclaw_config::schema::Config;
use zeroclaw_runtime::flow::{ConfiguredItem, FlowTransport, Outcome};

use crate::spec_builder::build_spec;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("section {0:?} has no configurable fields")]
    EmptySection(String),
    #[error(transparent)]
    Walk(#[from] zeroclaw_runtime::flow::WalkError),
}

pub struct FlowRequest<'section> {
    pub section_prefix: &'section str,
    pub layer: &'section str,
    pub instance: &'section str,
}

pub async fn run_flow(
    config: &mut Config,
    request: &FlowRequest<'_>,
    transport: &mut dyn FlowTransport,
) -> Result<Outcome, DriverError> {
    let success = Outcome::Completed {
        configured: vec![ConfiguredItem {
            layer: request.layer.to_string(),
            instance: request.instance.to_string(),
        }],
    };
    let spec = build_spec(
        config.prop_fields(),
        request.section_prefix,
        request.layer,
        request.instance,
        success,
    )
    .ok_or_else(|| DriverError::EmptySection(request.section_prefix.to_string()))?;
    Ok(spec.walk(transport, config).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use zeroclaw_config::schema::MatrixConfig;
    use zeroclaw_runtime::flow::{Prompt, TransportError, TransportResult};
    use zeroclaw_runtime::response_type::{ResponseValue, SecretValue};

    const SECTION: &str = "channels.matrix.home";

    struct ScriptedTransport {
        answers: VecDeque<ResponseValue>,
        emitted: Vec<Outcome>,
    }

    impl ScriptedTransport {
        fn new(answers: Vec<ResponseValue>) -> Self {
            Self {
                answers: answers.into(),
                emitted: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl FlowTransport for ScriptedTransport {
        async fn ask(&mut self, _prompt: &Prompt) -> TransportResult<ResponseValue> {
            self.answers.pop_front().ok_or(TransportError::Closed)
        }

        async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
            self.emitted.push(outcome.clone());
            Ok(())
        }
    }

    fn config_with_matrix_alias() -> Config {
        let mut config = Config::default();
        config
            .channels
            .matrix
            .insert("home".to_string(), MatrixConfig::default());
        config
    }

    fn answer_for(prompt: &Prompt) -> ResponseValue {
        match &prompt.response_type {
            zeroclaw_runtime::response_type::ResponseType::Secret => {
                ResponseValue::Secret(SecretValue::new("tok".into()))
            }
            zeroclaw_runtime::response_type::ResponseType::YesNo => ResponseValue::YesNo(true),
            zeroclaw_runtime::response_type::ResponseType::Choice { options } => {
                ResponseValue::Choice(options[0].value.clone())
            }
            zeroclaw_runtime::response_type::ResponseType::FreeformText => {
                ResponseValue::FreeformText("42".into())
            }
        }
    }

    struct AutoTransport {
        emitted: Vec<Outcome>,
    }

    #[async_trait]
    impl FlowTransport for AutoTransport {
        async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
            Ok(answer_for(prompt))
        }

        async fn emit(&mut self, outcome: &Outcome) -> TransportResult<()> {
            self.emitted.push(outcome.clone());
            Ok(())
        }
    }

    #[tokio::test]
    async fn run_flow_walks_section_and_completes() {
        let mut config = config_with_matrix_alias();
        let mut transport = AutoTransport {
            emitted: Vec::new(),
        };
        let request = FlowRequest {
            section_prefix: SECTION,
            layer: "channel",
            instance: "home",
        };
        let outcome = run_flow(&mut config, &request, &mut transport)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Completed { .. }));
    }

    #[tokio::test]
    async fn run_flow_persists_answers_into_config() {
        let mut config = config_with_matrix_alias();
        let mut transport = AutoTransport {
            emitted: Vec::new(),
        };
        let request = FlowRequest {
            section_prefix: SECTION,
            layer: "channel",
            instance: "home",
        };
        run_flow(&mut config, &request, &mut transport)
            .await
            .unwrap();
        assert_eq!(config.channels.matrix.get("home").unwrap().homeserver, "42");
    }

    #[tokio::test]
    async fn run_flow_rejects_a_section_with_no_fields() {
        let mut config = Config::default();
        let mut transport = ScriptedTransport::new(Vec::new());
        let request = FlowRequest {
            section_prefix: "channels.matrix.absent",
            layer: "channel",
            instance: "absent",
        };
        let error = run_flow(&mut config, &request, &mut transport)
            .await
            .unwrap_err();
        assert!(matches!(error, DriverError::EmptySection(_)));
    }
}
