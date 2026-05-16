//! Per-event toggles controlling which [`ObserverEvent`] variants get
//! translated into status updates. Value type intentionally — no
//! dependency on `zeroclaw-config`.

/// Plain bag of bools, one per supported event class.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProgressEventToggles {
    pub agent_start: bool,
    pub agent_end: bool,
    pub tool_call_start: bool,
    pub tool_call: bool,
    pub llm_thinking: bool,
    pub error: bool,
}

impl ProgressEventToggles {
    /// `true` iff at least one sub-toggle is enabled. Used by the observer
    /// to decide whether to emit the startup-diagnostic info log.
    pub fn any_enabled(&self) -> bool {
        self.agent_start
            || self.agent_end
            || self.tool_call_start
            || self.tool_call
            || self.llm_thinking
            || self.error
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_has_all_subs_off() {
        let t = ProgressEventToggles::default();
        assert!(!t.any_enabled());
    }

    #[test]
    fn any_enabled_true_when_one_set() {
        let t = ProgressEventToggles { tool_call_start: true, ..Default::default() };
        assert!(t.any_enabled());
    }

    #[test]
    fn any_enabled_true_when_all_set() {
        let t = ProgressEventToggles {
            agent_start: true, agent_end: true,
            tool_call_start: true, tool_call: true,
            llm_thinking: true, error: true,
        };
        assert!(t.any_enabled());
    }
}
