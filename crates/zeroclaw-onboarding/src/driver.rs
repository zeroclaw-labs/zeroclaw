use zeroclaw_config::schema::Config;
use zeroclaw_runtime::flow::{ConfiguredItem, FlowTransport, Outcome};

use crate::spec_builder::build_spec;

#[derive(Debug, thiserror::Error)]
pub enum DriverError {
    #[error("section {0:?} has no configurable fields")]
    EmptySection(String),
    #[error("cannot create alias {alias:?} under {family:?}: {reason}")]
    AliasCreate {
        family: String,
        alias: String,
        reason: String,
    },
    #[error(transparent)]
    Walk(#[from] zeroclaw_runtime::flow::WalkError),
}

pub struct FlowRequest<'section> {
    pub section_prefix: &'section str,
    pub layer: &'section str,
    pub instance: &'section str,
    pub create: bool,
}

fn family_of(section_prefix: &str, instance: &str) -> String {
    section_prefix
        .strip_suffix(&format!(".{instance}"))
        .unwrap_or(section_prefix)
        .to_string()
}

pub async fn run_flow(
    config: &mut Config,
    request: &FlowRequest<'_>,
    transport: &mut dyn FlowTransport,
) -> Result<Outcome, DriverError> {
    if request.create {
        let family = family_of(request.section_prefix, request.instance);
        config
            .create_map_key(&family, request.instance)
            .map_err(|reason| DriverError::AliasCreate {
                family,
                alias: request.instance.to_string(),
                reason,
            })?;
    }
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
            zeroclaw_runtime::response_type::ResponseType::Number => {
                ResponseValue::Number("42".into())
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
            create: false,
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
            create: false,
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
            create: false,
        };
        let error = run_flow(&mut config, &request, &mut transport)
            .await
            .unwrap_err();
        assert!(matches!(error, DriverError::EmptySection(_)));
    }

    #[tokio::test]
    async fn run_flow_creates_a_brand_new_alias_then_walks_it() {
        let mut config = Config::default();
        assert!(
            !config.channels.matrix.contains_key("fresh"),
            "precondition: the alias must not exist before the flow"
        );
        let mut transport = AutoTransport {
            emitted: Vec::new(),
        };
        let request = FlowRequest {
            section_prefix: "channels.matrix.fresh",
            layer: "channel",
            instance: "fresh",
            create: true,
        };
        let outcome = run_flow(&mut config, &request, &mut transport)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Completed { .. }));
        assert_eq!(
            config.channels.matrix.get("fresh").unwrap().homeserver,
            "42",
            "the freshly-created alias must carry the walked answers"
        );
    }

    #[tokio::test]
    async fn run_flow_create_rejects_an_unknown_family() {
        let mut config = Config::default();
        let mut transport = ScriptedTransport::new(Vec::new());
        let request = FlowRequest {
            section_prefix: "channels.nonsense.fresh",
            layer: "channel",
            instance: "fresh",
            create: true,
        };
        let error = run_flow(&mut config, &request, &mut transport)
            .await
            .unwrap_err();
        assert!(matches!(error, DriverError::AliasCreate { .. }));
    }
}
