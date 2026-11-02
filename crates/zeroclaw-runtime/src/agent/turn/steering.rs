//! Mid-turn steering: non-blocking drain of caller-pushed messages between
//! loop iterations (and between wrapper rounds).

use zeroclaw_api::ingress::IngressContext;

/// Whether a steering message may still influence the turn when the agent
/// consumes it, and under which tool posture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SteeringAdmission {
    /// The sender may no longer steer this turn; the message is dropped
    /// without reaching the model, memory, or any tool.
    Refused(String),
    /// The sender may steer. Its current posture is applied to the agent
    /// before the steered round runs.
    Admitted(SteeringPosture),
}

/// The sender's current tool posture, applied to the agent before a steered
/// round exactly as a new prompt applies its prompter's. Narrowing only ever
/// removes tools, so a steered round can never run with more than the sender
/// holds now.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SteeringPosture {
    /// `None` leaves the agent's tools as they are; `Some(list)` keeps only
    /// the named tools (an empty list leaves none).
    pub tool_ceiling: Option<Vec<String>>,
    /// Remove nested entry points that do not yet carry a principal's
    /// selectors (see `Agent::disable_principal_unaware_nested_tools`).
    pub disable_principal_unaware_nested_tools: bool,
}

/// Re-resolves a steering sender's authority. Called when the message is
/// consumed, not when it was queued, so a grant revoked or narrowed in
/// between takes effect.
pub type SteeringAdmit = Box<dyn Fn() -> SteeringAdmission + Send + Sync>;

/// One mid-turn steering message.
pub struct SteeringInput {
    text: String,
    admit: Option<SteeringAdmit>,
    ingress: Option<IngressContext>,
}

impl SteeringInput {
    /// A message whose sender's authority cannot change during the turn (the
    /// gateway chat socket authorizes its connection once). Always admitted,
    /// with no narrowing.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            admit: None,
            ingress: None,
        }
    }

    /// A message whose sender must still be authorized when it is consumed.
    pub fn with_admission(text: impl Into<String>, admit: SteeringAdmit) -> Self {
        Self {
            text: text.into(),
            admit: Some(admit),
            ingress: None,
        }
    }

    /// Attach producer-stamped ingress facts independently of live
    /// authorization. Absent facts never inherit the enclosing turn.
    #[must_use]
    pub fn with_ingress(mut self, ingress: IngressContext) -> Self {
        self.ingress = Some(ingress);
        self
    }

    pub fn ingress(&self) -> Option<&IngressContext> {
        self.ingress.as_ref()
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    /// Evaluate the sender's authority now.
    pub fn admit(&self) -> SteeringAdmission {
        match &self.admit {
            Some(admit) => admit(),
            None => SteeringAdmission::Admitted(SteeringPosture::default()),
        }
    }

    pub fn into_text(self) -> String {
        self.text
    }
}

impl std::fmt::Debug for SteeringInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SteeringInput")
            .field("len", &self.text.len())
            .field("checked", &self.admit.is_some())
            .finish()
    }
}

impl From<String> for SteeringInput {
    fn from(text: String) -> Self {
        Self::new(text)
    }
}

impl From<&str> for SteeringInput {
    fn from(text: &str) -> Self {
        Self::new(text)
    }
}

/// Drain any steering messages the caller pushed since the last round. The
/// caller admits each one (`SteeringInput::admit`) before it uses it.
pub fn drain_steering_messages(
    steering_rx: &mut Option<&mut tokio::sync::mpsc::Receiver<SteeringInput>>,
) -> Vec<SteeringInput> {
    let Some(rx) = steering_rx.as_deref_mut() else {
        return Vec::new();
    };
    let mut messages = Vec::new();
    while let Ok(message) = rx.try_recv() {
        messages.push(message);
    }
    messages
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unchecked_message_is_admitted_without_narrowing() {
        assert_eq!(
            SteeringInput::new("hi").admit(),
            SteeringAdmission::Admitted(SteeringPosture::default())
        );
    }

    #[test]
    fn a_checked_message_is_judged_when_admitted_not_when_built() {
        let allowed = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let flag = std::sync::Arc::clone(&allowed);
        let input = SteeringInput::with_admission(
            "hi",
            Box::new(move || {
                if flag.load(std::sync::atomic::Ordering::SeqCst) {
                    SteeringAdmission::Admitted(SteeringPosture::default())
                } else {
                    SteeringAdmission::Refused("revoked".into())
                }
            }),
        );
        allowed.store(false, std::sync::atomic::Ordering::SeqCst);
        assert_eq!(input.admit(), SteeringAdmission::Refused("revoked".into()));
    }
}
