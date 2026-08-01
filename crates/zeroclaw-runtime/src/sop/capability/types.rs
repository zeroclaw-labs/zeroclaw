use std::path::PathBuf;

use anyhow::Result;
use serde_json::Value;

#[derive(Debug, Clone)]
pub struct CapabilityInfo {
    pub id: &'static str,
    pub description: &'static str,
    pub deterministic: bool,
    pub idempotent: bool,
    pub reversible: bool,
    pub supports_retry: bool,
    pub required_permissions: Vec<&'static str>,
    pub input_schema: Option<Value>,
    pub output_schema: Option<Value>,
}

#[derive(Debug, Clone)]
pub struct CapabilityContext {
    pub run_id: String,
    pub sop_name: String,
    pub step_number: u32,
    pub sop_location: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CapabilityResult {
    pub success: bool,
    pub output: Value,
    pub error: Option<String>,
}

impl CapabilityResult {
    pub fn success(output: Value) -> Self {
        Self {
            success: true,
            output,
            error: None,
        }
    }

    pub fn failure(error: impl Into<String>) -> Self {
        Self {
            success: false,
            output: Value::Null,
            error: Some(error.into()),
        }
    }
}

pub trait SopCapability: Send + Sync {
    fn id(&self) -> &'static str;

    fn describe(&self) -> CapabilityInfo;

    /// Whether this capability may read only authored `with` configuration,
    /// never a complete piped event payload as its configuration plane.
    fn requires_authored_input(&self) -> bool {
        false
    }

    fn execute(&self, ctx: CapabilityContext, input: Value) -> Result<CapabilityResult>;
}
