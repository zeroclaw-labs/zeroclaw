//! Call-scoped sampling policy. Config owns defaults; callers own overrides.
use std::{future::Future, sync::Arc};
use zeroclaw_config::schema::Config;

/// Invocation input, kept separate from any configuration-derived default.
#[derive(Clone, Copy, Debug, Default)]
pub struct Temperature {
    pub override_value: Option<f64>,
    pub adjustment: f64,
}

impl From<Option<f64>> for Temperature {
    fn from(override_value: Option<f64>) -> Self {
        Self {
            override_value,
            adjustment: 0.0,
        }
    }
}

impl Temperature {
    pub fn adjusted(override_value: Option<f64>, adjustment: f64) -> Self {
        Self {
            override_value,
            adjustment,
        }
    }

    pub fn resolve(self, config: Option<&Config>, selection_ref: &str) -> Option<f64> {
        let configured = config
            .and_then(|config| config.resolve_model_selection(selection_ref))
            .and_then(|selection| selection.temperature());
        self.override_value.or(configured).map(|value| {
            if self.adjustment == 0.0 {
                value
            } else {
                (value + self.adjustment).clamp(0.0, 2.0)
            }
        })
    }
}

struct CallTemperature {
    // One admitted generation, retained only for this call/stream's lifetime.
    // Providers never retain a copy of live sampling policy.
    config: Option<Arc<Config>>,
    input: Temperature,
}

tokio::task_local! {
    static CALL_TEMPERATURE: CallTemperature;
}

/// Bind a generation and invocation input while polling a logical provider
/// call, including retries and streams. Nested execution creates its own scope.
pub fn with_temperature<'a, F>(
    config: Option<&Config>,
    input: Temperature,
    future: F,
) -> futures_util::future::BoxFuture<'a, F::Output>
where
    F: Future + Send + 'a,
    F::Output: 'a,
{
    // Seal the task-local future at the providers boundary so downstream
    // gateway/channel spawn sites need not expand the complete turn future
    // to prove Send. The Config generation is owned only by this call.
    Box::pin(CALL_TEMPERATURE.scope(
        CallTemperature {
            config: config.map(|config| Arc::new(config.clone())),
            input,
        },
        future,
    ))
}

/// Resolve the actual configured candidate just before dispatch, including
/// direct vision and one-shot calls.
/// Unscoped provider users retain the trait's explicit scalar semantics.
pub fn temperature_for_dispatch(selection_ref: &str, fallback: Option<f64>) -> Option<f64> {
    CALL_TEMPERATURE
        .try_with(|call| {
            if call
                .config
                .as_ref()
                .and_then(|config| config.resolve_model_selection(selection_ref))
                .is_some()
            {
                call.input.resolve(call.config.as_deref(), selection_ref)
            } else {
                fallback
            }
        })
        .unwrap_or(fallback)
}

#[cfg(test)]
mod tests {
    use super::*;
    use zeroclaw_config::schema::ModelEntryConfig;

    #[tokio::test]
    async fn sampling_scope_resolves_target_then_adjusts_and_does_not_leak() {
        let mut config = Config::default();
        let profile = config.providers.models.ensure("custom", "gw").unwrap();
        profile.temperature = Some(0.9);
        profile.models.insert(
            "chosen".into(),
            ModelEntryConfig {
                id: Some("shared-id".into()),
                temperature: Some(0.2),
                ..Default::default()
            },
        );
        config.providers.models.ensure("custom", "unset").unwrap();
        for (input, expected) in [
            (Temperature::adjusted(None, 0.1), 0.3),
            (Temperature::adjusted(Some(0.2), 0.1), 0.3),
            (Temperature::adjusted(Some(1.9), 0.5), 2.0),
        ] {
            with_temperature(Some(&config), input, async {
                assert!(
                    (temperature_for_dispatch("custom.gw.chosen", Some(0.9)).unwrap() - expected)
                        .abs()
                        < 1e-9
                );
                assert_eq!(
                    temperature_for_dispatch("custom.unset", Some(0.9)),
                    input.resolve(Some(&config), "custom.unset")
                );
                with_temperature(None, None.into(), async {
                    assert_eq!(
                        temperature_for_dispatch("custom.gw.chosen", Some(0.7)),
                        Some(0.7)
                    );
                })
                .await;
                assert!(
                    (temperature_for_dispatch("custom.gw.chosen", Some(0.9)).unwrap() - expected)
                        .abs()
                        < 1e-9
                );
            })
            .await;
            assert_eq!(
                temperature_for_dispatch("custom.gw.chosen", Some(0.7)),
                Some(0.7)
            );
        }
    }
}
