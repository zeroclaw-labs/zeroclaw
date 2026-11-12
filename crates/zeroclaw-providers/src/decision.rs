//! Native typed decision protocols: OpenAI Decisions and System One.
//!
//! These adapters call decision endpoints directly. Chat Completions and
//! Responses structured outputs belong to the chat adapters, not this module.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use serde::Deserialize;
use serde_json::{Value, json};

use zeroclaw_api::decision::{Answer, Answers, DecisionModel, Question, Usage};
use zeroclaw_config::provider_registry::ResolvedDecision;
use zeroclaw_config::schema::DecisionWireApi;

use crate::transport::HttpTransportConfig;

const DEFAULT_DECISION_TIMEOUT: Duration = Duration::from_secs(10);

/// Construction inputs resolved from one admitted provider configuration.
#[derive(Clone)]
pub struct DecisionProviderConfig {
    pub(crate) transport: HttpTransportConfig,
    pub model: String,
    pub wire_api: DecisionWireApi,
}

impl DecisionProviderConfig {
    /// Downstream consumers select a model without reconstructing provider
    /// defaults, authentication, or native protocol routing.
    pub fn from_resolved(resolved: &ResolvedDecision<'_>) -> Result<Self> {
        Ok(Self {
            transport: HttpTransportConfig::from_connection(
                resolved.selection.connection,
                resolved.request_timeout_secs.clone(),
            )?,
            model: resolved.model.to_owned(),
            wire_api: resolved.wire_api,
        })
    }
}

pub fn build_decision_provider(config: DecisionProviderConfig) -> Result<Box<dyn DecisionModel>> {
    Ok(Box::new(NativeDecisionClient::new(config)?))
}

fn endpoint_for(base_url: &str, wire_api: DecisionWireApi) -> Result<String> {
    let path = match wire_api {
        DecisionWireApi::OpenAiDecisions => "decisions",
        DecisionWireApi::SystemOne => "systemone",
    };
    crate::transport::append_endpoint(base_url, path)
}

struct NativeDecisionClient {
    http: reqwest::Client,
    endpoint: String,
    model: String,
    api_key: Option<String>,
    wire_api: DecisionWireApi,
}

impl NativeDecisionClient {
    fn new(config: DecisionProviderConfig) -> Result<Self> {
        let endpoint = endpoint_for(&config.transport.base_url, config.wire_api)?;
        Ok(Self {
            http: config
                .transport
                .client(DEFAULT_DECISION_TIMEOUT, "decision")?,
            endpoint,
            model: config.model,
            api_key: config
                .transport
                .api_key
                .filter(|key| !key.trim().is_empty()),
            wire_api: config.wire_api,
        })
    }
}

#[derive(Deserialize)]
struct OpenAiDecisionResponse {
    model: Option<String>,
    answers: Vec<OpenAiDecisionAnswer>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum OpenAiDecisionAnswer {
    Predicate {
        name: String,
        probability: f64,
    },
    Choice {
        name: String,
        choice: String,
        probabilities: Vec<ChoiceProbability>,
        confidence: f64,
    },
    Refusal {
        name: String,
    },
}

#[derive(Deserialize)]
struct ChoiceProbability {
    value: String,
    probability: f64,
}

fn probability_is_valid(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

fn openai_answers(
    response: OpenAiDecisionResponse,
    questions: &BTreeMap<String, Question>,
    model: &str,
) -> Result<Answers> {
    let mut answers = BTreeMap::new();
    for answer in response.answers {
        let (name, answer) = match answer {
            OpenAiDecisionAnswer::Predicate { name, probability } => {
                ensure!(
                    matches!(questions.get(&name), Some(Question::Noul { .. })),
                    "unexpected predicate answer for question `{name}`"
                );
                ensure!(
                    probability_is_valid(probability),
                    "invalid predicate probability for question `{name}`"
                );
                (name, Answer::Noul { noul: probability })
            }
            OpenAiDecisionAnswer::Choice {
                name,
                choice,
                probabilities,
                confidence,
            } => {
                let Some(Question::Choice { criteria, .. }) = questions.get(&name) else {
                    bail!("unexpected choice answer for question `{name}`");
                };
                ensure!(
                    criteria.contains_key(&choice),
                    "unoffered choice for question `{name}`"
                );
                ensure!(
                    probability_is_valid(confidence),
                    "invalid choice confidence for question `{name}`"
                );
                let mut distribution = BTreeMap::new();
                for item in probabilities {
                    ensure!(
                        criteria.contains_key(&item.value)
                            && probability_is_valid(item.probability),
                        "invalid choice probability for question `{name}`"
                    );
                    ensure!(
                        distribution.insert(item.value, item.probability).is_none(),
                        "duplicate choice probability for question `{name}`"
                    );
                }
                ensure!(
                    distribution.len() == criteria.len(),
                    "incomplete choice distribution for question `{name}`"
                );
                (
                    name,
                    Answer::Choice {
                        choice,
                        probabilities: distribution,
                        confidence,
                    },
                )
            }
            OpenAiDecisionAnswer::Refusal { name } => {
                bail!("decision model refused question `{name}`")
            }
        };
        ensure!(
            answers.insert(name.clone(), answer).is_none(),
            "duplicate decision answer for question `{name}`"
        );
    }
    ensure!(
        answers.len() == questions.len(),
        "decision response omitted requested questions"
    );
    Ok(Answers {
        model: response.model.or_else(|| Some(model.to_owned())),
        answers,
        usage: response.usage,
    })
}

#[async_trait::async_trait]
impl DecisionModel for NativeDecisionClient {
    fn id(&self) -> &str {
        &self.model
    }

    async fn ask(&self, state: Value, questions: BTreeMap<String, Question>) -> Result<Answers> {
        let body = match self.wire_api {
            DecisionWireApi::OpenAiDecisions => {
                crate::protocols::openai::decision::request(&self.model, state, &questions)?
            }
            DecisionWireApi::SystemOne => {
                json!({ "model": self.model, "state": state, "questions": questions })
            }
        };
        let mut request = self.http.post(&self.endpoint).json(&body);
        if let Some(api_key) = &self.api_key {
            request = request.bearer_auth(api_key);
        }
        let response = request
            .send()
            .await
            .context("decision model request failed")?;
        let status = response.status();
        if !status.is_success() {
            bail!("decision model returned HTTP {}", status.as_u16());
        }
        match self.wire_api {
            DecisionWireApi::OpenAiDecisions => {
                let response = response
                    .json::<OpenAiDecisionResponse>()
                    .await
                    .context("decision model response was not an OpenAI Decisions answer set")?;
                openai_answers(response, &questions, &self.model)
            }
            DecisionWireApi::SystemOne => response
                .json::<Answers>()
                .await
                .context("decision model response was not a System One answer set"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn questions() -> BTreeMap<String, Question> {
        BTreeMap::from([
            (
                "go".to_owned(),
                Question::Noul {
                    instructions: "run?".to_owned(),
                },
            ),
            (
                "route".to_owned(),
                Question::Choice {
                    instructions: "where?".to_owned(),
                    criteria: BTreeMap::from([
                        ("fast".to_owned(), "low latency".to_owned()),
                        ("safe".to_owned(), "high confidence".to_owned()),
                    ]),
                },
            ),
        ])
    }

    fn native_response() -> Value {
        json!({
            "model": "served-decision-model",
            "answers": [
                {"type": "choice", "name": "route", "choice": "safe", "confidence": 0.9, "probabilities": [{"value": "fast", "probability": 0.1}, {"value": "safe", "probability": 0.9}]},
                {"type": "predicate", "name": "go", "probability": 0.8}
            ],
            "usage": {"input_tokens": 12}
        })
    }

    #[test]
    fn native_answers_are_matched_by_name_and_preserve_usage() {
        let response = serde_json::from_value(native_response()).unwrap();
        let result = openai_answers(response, &questions(), "requested-model").unwrap();
        assert_eq!(result.model.as_deref(), Some("served-decision-model"));
        assert_eq!(result.answers["go"], Answer::Noul { noul: 0.8 });
        assert!(
            matches!(&result.answers["route"], Answer::Choice { choice, probabilities, confidence } if choice == "safe" && probabilities["safe"] == 0.9 && *confidence == 0.9)
        );
        assert_eq!(result.usage.unwrap().input_tokens, 12);
    }

    #[test]
    fn invalid_or_refused_native_answers_fail_closed() {
        for raw in [
            json!({"answers": [{"type": "refusal", "name": "go"}]}),
            json!({"answers": []}),
            json!({"answers": [{"type": "predicate", "name": "go", "probability": 2.0}]}),
            json!({"answers": [{"type": "predicate", "name": "route", "probability": 0.5}]}),
            json!({"answers": [{"type": "predicate", "name": "go", "probability": 0.5}, {"type": "predicate", "name": "go", "probability": 0.5}]}),
            json!({"answers": [{"type": "choice", "name": "route", "choice": "unsafe", "confidence": 1.0, "probabilities": []}]}),
        ] {
            let response = serde_json::from_value(raw).unwrap();
            assert!(openai_answers(response, &questions(), "test").is_err());
        }
    }

    #[test]
    fn endpoint_derivation_preserves_versioned_gateway_prefixes() {
        assert_eq!(
            endpoint_for(
                "https://example.test/inference/v1/",
                DecisionWireApi::OpenAiDecisions
            )
            .unwrap(),
            "https://example.test/inference/v1/decisions"
        );
        assert_eq!(
            endpoint_for("https://example.test/api/v1", DecisionWireApi::SystemOne).unwrap(),
            "https://example.test/api/v1/systemone"
        );
        assert_eq!(
            endpoint_for("https://example.test", DecisionWireApi::OpenAiDecisions).unwrap(),
            "https://example.test/v1/decisions"
        );
        assert_eq!(
            endpoint_for(
                "https://example.test/v1/systemone",
                DecisionWireApi::SystemOne
            )
            .unwrap(),
            "https://example.test/v1/systemone"
        );
    }

    #[tokio::test]
    async fn native_protocols_use_decision_endpoints_and_share_transport_settings() {
        use wiremock::matchers::{header, method, path};
        use wiremock::{Mock, MockServer, ResponseTemplate};

        let server = MockServer::start().await;
        for kind in [
            "openai",
            "vllm",
            "custom",
            "openai-compatible",
            "fireworks",
            "lmstudio",
            "litellm",
            "system-one",
            "ollama",
            "openrouter",
            "llamacpp",
            "sglang",
        ] {
            let uri = if kind == "openrouter" {
                String::new()
            } else {
                format!("uri = \"{}/{}\"", server.uri(), kind)
            };
            let registry: zeroclaw_config::provider_registry::ProviderRegistry =
                toml::from_str(&format!(
                    r#"
[connections.gateway]
kind = "{kind}"
{uri}
auth = {{ mode = "api_key", api_key = "test-key" }}
[models."decision.sop"]
capability = "decision"
provider = "gateway"
model = "native-decision-model"
"#
                ))
                .unwrap();
            let resolved = registry.resolve_decision(&"decision.sop".into()).unwrap();
            let mut config = DecisionProviderConfig::from_resolved(&resolved).unwrap();
            let (suffix, response) = match resolved.wire_api {
                DecisionWireApi::OpenAiDecisions => ("decisions", native_response()),
                DecisionWireApi::SystemOne => (
                    "systemone",
                    json!({"model": "served-decision-model", "answers": {"go": {"type": "noul", "noul": 0.8}, "route": {"type": "choice", "choice": "safe", "probabilities": {"fast": 0.1, "safe": 0.9}, "confidence": 0.9}}, "usage": {"input_tokens": 12}}),
                ),
            };
            Mock::given(method("POST"))
                .and(path(format!("/{kind}/v1/{suffix}")))
                .and(header("Authorization", "Bearer test-key"))
                .and(header("X-Decision-Test", "native"))
                .respond_with(ResponseTemplate::new(200).set_body_json(response))
                .expect(1)
                .mount(&server)
                .await;
            // Substitute the mock host for fixed public endpoints too.
            config.transport.base_url = format!("{}/{kind}/v1", server.uri());
            config
                .transport
                .extra_headers
                .insert("X-Decision-Test".to_owned(), "native".to_owned());
            let client = build_decision_provider(config).unwrap();
            let result = client
                .ask(json!({"event": "test"}), questions())
                .await
                .unwrap();
            assert_eq!(result.answers["go"], Answer::Noul { noul: 0.8 });
            assert_eq!(result.usage.unwrap().input_tokens, 12);
        }
        for request in server.received_requests().await.unwrap() {
            let body: Value = serde_json::from_slice(&request.body).unwrap();
            assert_eq!(body["model"], "native-decision-model");
            if request.url.path().ends_with("/decisions") {
                assert_eq!(
                    body["questions"][0],
                    json!({"type": "predicate", "name": "go", "instructions": "run?"})
                );
                assert_eq!(
                    body["questions"][1]["choices"][0],
                    json!({"value": "fast", "description": "low latency"})
                );
                assert_eq!(
                    serde_json::from_str::<Value>(body["input"].as_str().unwrap()).unwrap(),
                    json!({"event": "test"})
                );
            } else {
                assert_eq!(body["state"], json!({"event": "test"}));
                assert_eq!(body["questions"]["go"]["type"], "noul");
            }
            assert!(body.get("response_format").is_none());
            assert!(body.get("messages").is_none());
            assert!(body.get("text").is_none());
        }
    }
}
