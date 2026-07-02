use zeroclaw_config::schema::Config;
use zeroclaw_runtime::flow::{ConfiguredItem, FlowTransport, Outcome};

use crate::spec_builder::{FieldScope, build_spec_scoped};

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
    pub scope: FieldScope,
}

fn family_of(section_prefix: &str, instance: &str) -> String {
    section_prefix
        .strip_suffix(&format!(".{instance}"))
        .unwrap_or(section_prefix)
        .to_string()
}

const LOCALE_PROMPT_ID: &str = "onboard-flow-locale-prompt";

fn locale_prompt() -> zeroclaw_runtime::flow::Prompt {
    use zeroclaw_runtime::flow::{Localizable, Prompt};
    use zeroclaw_runtime::response_type::{ChoiceOption, ResponseType};

    let options = zeroclaw_runtime::i18n::available_locales()
        .iter()
        .map(|locale| ChoiceOption {
            value: locale.code.clone(),
            label: locale.label.clone(),
        })
        .collect();
    let text = crate::i18n::get_required_onboard_string(LOCALE_PROMPT_ID);
    Prompt::new(text, ResponseType::Choice { options })
        .with_message(Localizable::new(LOCALE_PROMPT_ID))
}

/// First onboarding step: select the active locale before any section walk.
///
/// The options are derived from the `locales.toml` registry via
/// `available_locales()`, never a literal list. On selection `i18n::init` is
/// called so every later string reflects the choice, and the chosen code is
/// returned for the guide to speak. The flow engine stays locale-free: this is
/// a driver-level step, not an engine node.
pub async fn select_locale(
    transport: &mut dyn FlowTransport,
) -> Result<String, zeroclaw_runtime::flow::WalkError> {
    use zeroclaw_runtime::response_type::ResponseValue;

    let prompt = locale_prompt();
    let response = transport.ask(&prompt).await?;
    let code = match response {
        ResponseValue::Choice(value) => value,
        _ => zeroclaw_runtime::i18n::detect_locale(),
    };
    zeroclaw_runtime::i18n::init(&code);
    Ok(code)
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
    let mut spec = build_spec_scoped(
        config.prop_fields(),
        request.section_prefix,
        request.layer,
        request.instance,
        success.clone(),
        request.scope,
    )
    .ok_or_else(|| DriverError::EmptySection(request.section_prefix.to_string()))?;
    if request.create && request.section_prefix.starts_with("channels.") {
        spec = crate::spec_builder::append_peer_group_branch(
            spec,
            request.section_prefix,
            request.instance,
            config,
            success.clone(),
        );
    }
    if request.section_prefix.starts_with("agents.") {
        let ctx = zeroclaw_runtime::agent::personality_templates::TemplateContext {
            agent: request.instance.to_string(),
            ..Default::default()
        };
        spec =
            crate::spec_builder::append_personality_branch(spec, request.instance, &ctx, success);
    }
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
            scope: FieldScope::All,
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
            scope: FieldScope::All,
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
            scope: FieldScope::All,
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
            scope: FieldScope::All,
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
            scope: FieldScope::All,
        };
        let error = run_flow(&mut config, &request, &mut transport)
            .await
            .unwrap_err();
        assert!(matches!(error, DriverError::AliasCreate { .. }));
    }

    #[tokio::test]
    async fn run_flow_required_only_skips_optional_fields() {
        let mut config = config_with_matrix_alias();
        let mut transport = AutoTransport {
            emitted: Vec::new(),
        };
        let request = FlowRequest {
            section_prefix: SECTION,
            layer: "channel",
            instance: "home",
            create: false,
            scope: FieldScope::RequiredOnly,
        };
        run_flow(&mut config, &request, &mut transport)
            .await
            .unwrap();
        let matrix = config.channels.matrix.get("home").unwrap();
        assert_eq!(
            matrix.homeserver, "42",
            "a required field is still walked and persisted"
        );
        assert!(
            matrix.access_token.is_none(),
            "an Option field is never asked, so it keeps its default"
        );
    }

    #[tokio::test]
    async fn select_locale_offers_every_registered_locale_and_returns_the_choice() {
        let available = zeroclaw_runtime::i18n::available_locales();
        let target = available
            .get(if available.len() > 1 { 1 } else { 0 })
            .expect("registry lists at least one locale")
            .code
            .clone();
        let mut transport = ScriptedTransport::new(vec![ResponseValue::Choice(target.clone())]);
        let chosen = select_locale(&mut transport).await.unwrap();
        assert_eq!(chosen, target);
    }

    #[tokio::test]
    async fn select_locale_options_come_from_the_registry_not_a_literal_list() {
        struct CapturingTransport {
            seen_options: Vec<String>,
        }
        #[async_trait]
        impl FlowTransport for CapturingTransport {
            async fn ask(&mut self, prompt: &Prompt) -> TransportResult<ResponseValue> {
                if let zeroclaw_runtime::response_type::ResponseType::Choice { options } =
                    &prompt.response_type
                {
                    self.seen_options = options.iter().map(|o| o.value.clone()).collect();
                    return Ok(ResponseValue::Choice(options[0].value.clone()));
                }
                Err(TransportError::Closed)
            }
            async fn emit(&mut self, _outcome: &Outcome) -> TransportResult<()> {
                Ok(())
            }
        }
        let mut transport = CapturingTransport {
            seen_options: Vec::new(),
        };
        select_locale(&mut transport).await.unwrap();
        let registry: Vec<String> = zeroclaw_runtime::i18n::available_locales()
            .iter()
            .map(|l| l.code.clone())
            .collect();
        assert_eq!(
            transport.seen_options, registry,
            "locale options must mirror the registry exactly, in order"
        );
    }

    #[tokio::test]
    async fn the_selected_locale_reaches_the_guide_system_prompt() {
        use crate::agent_responder::{AgentResponder, AgentTurn};
        use crate::llm_transport::LlmResponder;
        use std::sync::{Arc, Mutex};

        let target = zeroclaw_runtime::i18n::available_locales()
            .iter()
            .find(|l| l.code != "en")
            .cloned()
            .expect("at least one non-English locale is registered");

        struct CapturingTurn {
            seen: Arc<Mutex<Vec<String>>>,
        }
        #[async_trait]
        impl AgentTurn for CapturingTurn {
            async fn run_single(&mut self, message: &str) -> TransportResult<String> {
                self.seen.lock().unwrap().push(message.to_string());
                Ok("ANSWER: ok".to_string())
            }
        }

        struct NoOperator;
        #[async_trait]
        impl crate::agent_responder::OperatorIo for NoOperator {
            async fn say(&mut self, _text: &str) -> TransportResult<()> {
                Err(TransportError::Closed)
            }
            async fn hear(&mut self) -> TransportResult<String> {
                Err(TransportError::Closed)
            }
        }

        let mut selector = ScriptedTransport::new(vec![ResponseValue::Choice(target.code.clone())]);
        let chosen = select_locale(&mut selector).await.unwrap();

        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut responder = AgentResponder::new(
            CapturingTurn {
                seen: Arc::clone(&seen),
            },
            NoOperator,
        )
        .with_locale(chosen);
        responder
            .respond("Provide the homeserver URL")
            .await
            .unwrap();

        assert!(
            seen.lock()
                .unwrap()
                .iter()
                .any(|message| message.contains(&target.label)),
            "the locale chosen by the selector must reach the guide's system prompt"
        );
    }
}
