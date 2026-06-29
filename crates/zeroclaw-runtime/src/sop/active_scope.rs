use std::sync::{Arc, Mutex};

/// The active SOP step's additional tool exclusions for the agent turn loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActiveStepScope {
    pub run_id: String,
    pub step_number: u32,
    pub excluded: Vec<String>,
}

pub type ActiveScopeHandle = Arc<Mutex<Option<ActiveStepScope>>>;
