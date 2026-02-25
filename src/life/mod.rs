use crate::channels::traits::Channel;
use crate::config::LifeConfig;
use crate::cosmic::{DriftDetector, EmotionalModulator, IntegrationMeter};
use crate::memory::traits::Memory;
use crate::observability::{Observer, ObserverEvent};
use crate::providers::traits::Provider;
use anyhow::Result;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::time::Duration;

mod dream;
mod emotional;
mod initiative;

pub use dream::DreamEngine;
pub use emotional::EmotionalState;
pub use initiative::{InitiativeEngine, InitiativeTrigger};

pub struct LifeLoop {
    pub emotional_state: Arc<Mutex<EmotionalState>>,
    memory: Arc<dyn Memory>,
    provider: Arc<dyn Provider>,
    channels: Vec<Arc<dyn Channel>>,
    observer: Arc<dyn Observer>,
    initiative: InitiativeEngine,
    dream: DreamEngine,
    integration: Arc<Mutex<IntegrationMeter>>,
    drift: Option<Arc<Mutex<DriftDetector>>>,
    modulator: Option<Arc<Mutex<EmotionalModulator>>>,
    config: LifeConfig,
}

impl LifeLoop {
    pub fn new(
        memory: Arc<dyn Memory>,
        provider: Arc<dyn Provider>,
        channels: Vec<Arc<dyn Channel>>,
        observer: Arc<dyn Observer>,
        integration: Arc<Mutex<IntegrationMeter>>,
        drift: Option<Arc<Mutex<DriftDetector>>>,
        modulator: Option<Arc<Mutex<EmotionalModulator>>>,
        config: LifeConfig,
    ) -> Self {
        let emotional_state = Arc::new(Mutex::new(EmotionalState::load_or_default(
            &config.emotional_persistence_path,
        )));

        let initiative = InitiativeEngine::new(
            Duration::from_secs(u64::from(config.initiative_cooldown_minutes) * 60),
            vec![
                InitiativeTrigger::CuriosityThreshold(config.curiosity_initiative_threshold),
                InitiativeTrigger::SilenceThreshold(Duration::from_secs(
                    u64::from(config.silence_initiative_hours) * 3600,
                )),
                InitiativeTrigger::EmotionalPeak(0.9),
            ],
        );

        let dream = DreamEngine::new(Duration::from_secs(
            u64::from(config.dream_idle_hours) * 3600,
        ));

        {
            let mut meter = integration.blocking_lock();
            meter.register_subsystem("emotional", vec!["memory".into(), "initiative".into()]);
            meter.register_subsystem("memory", vec!["emotional".into(), "dream".into()]);
            meter.register_subsystem("initiative", vec!["emotional".into()]);
            meter.register_subsystem("dream", vec!["memory".into()]);
        }

        Self {
            emotional_state,
            memory,
            provider,
            channels,
            observer,
            initiative,
            dream,
            integration,
            drift,
            modulator,
            config,
        }
    }

    pub async fn run(&mut self) -> Result<()> {
        tracing::info!(
            "Life loop started — tick interval: {}s",
            self.config.tick_interval_secs
        );

        let tick = Duration::from_secs(u64::from(self.config.tick_interval_secs));
        let mut interval = tokio::time::interval(tick);

        loop {
            interval.tick().await;

            let now = chrono::Utc::now();

            {
                let mut state = self.emotional_state.lock().await;
                let elapsed = now
                    .signed_duration_since(state.last_tick)
                    .to_std()
                    .unwrap_or(Duration::from_secs(0));
                state.tick(elapsed);
                state.last_tick = now;
            }

            {
                let state = self.emotional_state.lock().await;
                let mut meter = self.integration.lock().await;
                meter.update_state("emotional", f64::from(state.valence).clamp(0.0, 1.0));
                meter.update_state(
                    "memory",
                    if self.memory.health_check().await {
                        0.9
                    } else {
                        0.3
                    },
                );
                meter.update_state("initiative", f64::from(state.curiosity).clamp(0.0, 1.0));
                let snap = meter.snapshot();
                tracing::debug!(phi = snap.phi, hub_ratio = snap.hub_ratio, "Life loop integration tick");
            }

            if let Some(ref drift) = self.drift {
                let mut d = drift.lock().await;
                let state = self.emotional_state.lock().await;
                d.record_sample("emotional_valence", f64::from(state.valence).clamp(0.0, 1.0));
                d.record_sample("emotional_arousal", f64::from(state.arousal).clamp(0.0, 1.0));
            }

            if let Some(ref modulator) = self.modulator {
                let state = self.emotional_state.lock().await;
                let mut m = modulator.lock().await;
                m.apply_emotional_input(state.valence, state.arousal, state.trust);
            }

            if let Err(e) = self.maybe_initiate().await {
                tracing::warn!("Life initiative error: {e}");
            }

            if let Err(e) = self.maybe_dream().await {
                tracing::warn!("Life dream error: {e}");
            }

            {
                let state = self.emotional_state.lock().await;
                self.observer.record_event(&ObserverEvent::LifeTick {
                    valence: state.valence,
                    arousal: state.arousal,
                    curiosity: state.curiosity,
                });
                state.save(&self.config.emotional_persistence_path);
            }
        }
    }

    async fn maybe_initiate(&mut self) -> Result<()> {
        let state = self.emotional_state.lock().await;
        let message = self
            .initiative
            .evaluate(
                &state,
                &self.channels,
                self.provider.as_ref(),
                self.memory.as_ref(),
                &self.config,
            )
            .await?;

        if let Some(msg) = message {
            tracing::info!("Life loop initiated contact: {}", &msg[..msg.len().min(80)]);
            self.observer.record_event(&ObserverEvent::LifeInitiative {
                trigger: "auto".into(),
                message_preview: msg[..msg.len().min(50)].to_string(),
            });
        }
        Ok(())
    }

    async fn maybe_dream(&mut self) -> Result<()> {
        let mut state = self.emotional_state.lock().await;
        let insight = self
            .dream
            .maybe_dream(self.memory.as_ref(), self.provider.as_ref(), &mut state)
            .await?;

        if let Some(dream) = insight {
            tracing::info!(
                "Life loop dream insight: {}",
                &dream.synthesis[..dream.synthesis.len().min(80)]
            );
            self.observer
                .record_event(&ObserverEvent::LifeDreamComplete {
                    insight_preview: dream.synthesis[..dream.synthesis.len().min(50)].to_string(),
                });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn life_config_defaults_are_sane() {
        let config = LifeConfig::default();
        assert!(!config.enabled);
        assert!(config.tick_interval_secs >= 10);
        assert!(config.curiosity_initiative_threshold > 0.0);
        assert!(config.curiosity_initiative_threshold <= 1.0);
    }
}
