//! Per-request OpenAI effort mapping after the concrete model is selected.
//!
//! Public API capabilities follow each model's official documentation:
//! <https://developers.openai.com/api/docs/models>.
//! Subscription capabilities follow the separate client catalog and wire normalization:
//! <https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/models-manager/models.json>
//! <https://github.com/openai/codex/blob/126a0ee92ed2c5e73cd8a8f7e37563e8527552d7/codex-rs/protocol/src/openai_models/reasoning_effort.rs>.

use zeroclaw_api::model_provider::ThinkingLevel;

/// Set only by the OpenAI-family factory, including its endpoint overrides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReasoningSurface {
    PublicApi,
    Subscription,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReasoningApi {
    Responses,
    ChatCompletions,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ReasoningCall {
    Turn,
    OneShot,
}

/// Wire values in increasing effort order; client delegation modes are excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum NativeEffort {
    None,
    Minimal,
    Low,
    Medium,
    High,
    XHigh,
    Max,
}

impl NativeEffort {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::XHigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Preserve existing sampling behavior unless this request newly selects
/// reasoning. Omitting an optional sampling parameter keeps the mapped request
/// valid for reasoning models that reject it.
/// <https://developers.openai.com/api/docs/guides/latest-model>
pub(crate) fn profile_temperature(
    temperature: Option<f64>,
    effort: Option<NativeEffort>,
) -> Option<f64> {
    match effort {
        Some(effort) if effort != NativeEffort::None => None,
        _ => temperature,
    }
}

struct ModelEfforts {
    models: &'static [&'static str],
    efforts: &'static [NativeEffort],
    chat_supported: bool,
    // Some models accept Chat Completions tools only without reasoning.
    // None leaves unverified or unsupported combinations on existing behavior.
    chat_tools: Option<&'static [NativeEffort]>,
}

const PUBLIC_MODELS: &[ModelEfforts] = &[
    // https://developers.openai.com/api/docs/models/gpt-5.1
    ModelEfforts {
        models: &["gpt-5.1", "gpt-5.1-2025-11-13"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5.2-codex
    ModelEfforts {
        models: &["gpt-5.2-codex"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    // https://developers.openai.com/api/docs/models/gpt-5.2
    ModelEfforts {
        models: &["gpt-5.2", "gpt-5.2-2025-12-11"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5.3-codex
    ModelEfforts {
        models: &["gpt-5.3-codex"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    // https://developers.openai.com/api/docs/models/gpt-5.4-mini
    ModelEfforts {
        models: &["gpt-5.4-mini", "gpt-5.4-mini-2026-03-17"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5.4-nano
    ModelEfforts {
        models: &["gpt-5.4-nano", "gpt-5.4-nano-2026-03-17"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5.4-pro
    ModelEfforts {
        models: &["gpt-5.4-pro", "gpt-5.4-pro-2026-03-05"],
        efforts: &[
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    // https://developers.openai.com/api/docs/models/gpt-5.4
    ModelEfforts {
        models: &["gpt-5.4", "gpt-5.4-2026-03-05"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5.5-pro
    ModelEfforts {
        models: &["gpt-5.5-pro", "gpt-5.5-pro-2026-04-23"],
        efforts: &[
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    // https://developers.openai.com/api/docs/models/gpt-5.5
    ModelEfforts {
        models: &["gpt-5.5", "gpt-5.5-2026-04-23"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5.6-luna
    ModelEfforts {
        models: &["gpt-5.6-luna"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5.6-sol
    ModelEfforts {
        models: &["gpt-5.6-sol", "gpt-5.6"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5.6-terra
    ModelEfforts {
        models: &["gpt-5.6-terra"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-5
    ModelEfforts {
        models: &["gpt-5", "gpt-5-2025-08-07"],
        efforts: &[
            NativeEffort::Minimal,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
        ],
        chat_supported: true,
        chat_tools: Some(&[
            NativeEffort::Minimal,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
        ]),
    },
    // https://developers.openai.com/api/docs/models/gpt-6-astra
    ModelEfforts {
        models: &["gpt-6-astra"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: true,
        chat_tools: None,
    },
    // https://developers.openai.com/api/docs/models/gpt-6-luna
    ModelEfforts {
        models: &["gpt-6-luna"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: true,
        chat_tools: Some(&[NativeEffort::None]),
    },
    // https://developers.openai.com/api/docs/models/gpt-6-sol
    ModelEfforts {
        models: &["gpt-6-sol"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: true,
        chat_tools: Some(&[NativeEffort::None]),
    },
    // https://developers.openai.com/api/docs/models/gpt-6.1-sol
    ModelEfforts {
        models: &["gpt-6.1-sol"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: true,
        chat_tools: None,
    },
    // https://developers.openai.com/api/docs/models/gpt-6.1-terra
    ModelEfforts {
        models: &["gpt-6.1-terra"],
        efforts: &[
            NativeEffort::None,
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: true,
        chat_tools: None,
    },
];

const SUBSCRIPTION_MODELS: &[ModelEfforts] = &[
    ModelEfforts {
        models: &["gpt-6-astra"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    ModelEfforts {
        models: &["gpt-6.1-sol"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    ModelEfforts {
        models: &["gpt-6.1-terra"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    ModelEfforts {
        models: &["gpt-6-sol"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    ModelEfforts {
        models: &["gpt-6-luna"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    ModelEfforts {
        models: &["gpt-5.6-sol"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    ModelEfforts {
        models: &["gpt-5.6-terra"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    ModelEfforts {
        models: &["gpt-5.6-luna"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
            NativeEffort::Max,
        ],
        chat_supported: false,
        chat_tools: None,
    },
    ModelEfforts {
        models: &["gpt-5.5"],
        efforts: &[
            NativeEffort::Low,
            NativeEffort::Medium,
            NativeEffort::High,
            NativeEffort::XHigh,
        ],
        chat_supported: false,
        chat_tools: None,
    },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MappedEffort {
    effort: NativeEffort,
    substituted: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UnmappedReason {
    UnknownModel,
    UnsupportedApi,
    UnverifiedChatTools,
    EmptyCapabilitySet,
}

impl UnmappedReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::UnknownModel => "unknown_model",
            Self::UnsupportedApi => "unsupported_request_api",
            Self::UnverifiedChatTools => "chat_completions_tools_unverified",
            Self::EmptyCapabilitySet => "empty_capability_set",
        }
    }
}

fn requested_effort(level: ThinkingLevel) -> NativeEffort {
    match level {
        ThinkingLevel::Off => NativeEffort::None,
        ThinkingLevel::Minimal => NativeEffort::Minimal,
        ThinkingLevel::Low => NativeEffort::Low,
        ThinkingLevel::Medium => NativeEffort::Medium,
        ThinkingLevel::High => NativeEffort::High,
        ThinkingLevel::Max => NativeEffort::Max,
    }
}

fn map_level(level: ThinkingLevel, supported: &[NativeEffort]) -> Option<MappedEffort> {
    let requested = requested_effort(level);
    let effort = if level == ThinkingLevel::Max {
        supported.last().copied()
    } else {
        supported
            .iter()
            .copied()
            .find(|effort| *effort >= requested)
            .or_else(|| supported.last().copied())
    }?;
    Some(MappedEffort {
        effort,
        substituted: effort != requested,
    })
}

fn resolve(
    surface: ReasoningSurface,
    api: ReasoningApi,
    model: &str,
    level: ThinkingLevel,
    has_tools: bool,
) -> Result<MappedEffort, UnmappedReason> {
    let models = match surface {
        ReasoningSurface::PublicApi => PUBLIC_MODELS,
        ReasoningSurface::Subscription => SUBSCRIPTION_MODELS,
    };
    let capabilities = models
        .iter()
        .find(|entry| entry.models.contains(&model))
        .ok_or(UnmappedReason::UnknownModel)?;
    let supported = match api {
        ReasoningApi::Responses => capabilities.efforts,
        ReasoningApi::ChatCompletions if !capabilities.chat_supported => {
            return Err(UnmappedReason::UnsupportedApi);
        }
        ReasoningApi::ChatCompletions if has_tools => capabilities
            .chat_tools
            .ok_or(UnmappedReason::UnverifiedChatTools)?,
        ReasoningApi::ChatCompletions => capabilities.efforts,
    };
    map_level(level, supported).ok_or(UnmappedReason::EmptyCapabilitySet)
}

/// Immutable identity of one concrete leaf request, never a provider-wide policy.
#[derive(Clone, Copy)]
pub(crate) struct ReasoningContext<'a> {
    pub(crate) surface: Option<ReasoningSurface>,
    pub(crate) api: ReasoningApi,
    pub(crate) alias: &'a str,
    pub(crate) model: &'a str,
    pub(crate) has_tools: bool,
    pub(crate) custom_endpoint: bool,
    pub(crate) call: ReasoningCall,
}

impl ReasoningContext<'_> {
    /// Resolve only explicit levels on OpenAI-family leaves. The caller retains
    /// its existing fallback, including subscription defaults and legacy clamps.
    pub(crate) fn resolve_level(
        self,
        level: Option<ThinkingLevel>,
        configured_effort: Option<&str>,
        fallback_effort: Option<&str>,
    ) -> Option<NativeEffort> {
        let level = level?;
        let surface = self.surface?;
        let resolution = resolve(surface, self.api, self.model, level, self.has_tools);
        let mapped = resolution.as_ref().ok().map(|mapped| mapped.effort);
        let debug = ::zeroclaw_log::debug_enabled();
        if resolution.is_err() || debug {
            let outcome = match resolution {
                Ok(MappedEffort {
                    substituted: false, ..
                }) => "exact",
                Ok(_) => "substituted",
                Err(_) => "unmapped",
            };
            let attrs = ::serde_json::json!({
                "provider": match surface {
                    ReasoningSurface::PublicApi => "openai",
                    ReasoningSurface::Subscription => "openai-codex",
                },
                "alias": self.alias,
                "request_api": match self.api {
                    ReasoningApi::Responses => "responses",
                    ReasoningApi::ChatCompletions => "chat_completions",
                },
                "surface": match surface {
                    ReasoningSurface::PublicApi => "public_api",
                    ReasoningSurface::Subscription => "subscription",
                },
                "model": self.model,
                "requested_level": level.as_str(),
                "effective_effort": mapped.map(NativeEffort::as_str).or(fallback_effort),
                "outcome": outcome,
                "unmapped_reason": resolution.err().map(UnmappedReason::as_str),
                "global_effort_overridden": mapped.is_some() && configured_effort.is_some(),
                "custom_endpoint": self.custom_endpoint,
                "call": match self.call {
                    ReasoningCall::Turn => "turn",
                    ReasoningCall::OneShot => "one_shot",
                },
            });
            let event = ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(attrs);
            if resolution.is_err() {
                ::zeroclaw_log::record!(
                    INFO,
                    event,
                    "OpenAI native thinking level could not be mapped"
                );
            } else {
                ::zeroclaw_log::record!(DEBUG, event, "OpenAI native thinking effort resolved");
            }
        }
        mapped
    }
}

#[cfg(test)]
#[path = "openai_reasoning_tests.rs"]
mod tests;
