use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use super::builtins;
use super::types::{CapabilityContext, CapabilityResult, SopCapability};
use crate::sop::schema;
use crate::sop::types::{Sop, SopStep, SopStepKind};

#[derive(Clone, Default)]
pub struct SopCapabilityRegistry {
    capabilities: BTreeMap<String, Arc<dyn SopCapability>>,
}

impl SopCapabilityRegistry {
    pub fn with_builtins() -> Self {
        let mut registry = Self::default();
        builtins::register(&mut registry);
        registry
    }

    pub fn register<C>(&mut self, capability: C)
    where
        C: SopCapability + 'static,
    {
        self.capabilities
            .insert(capability.id().to_string(), Arc::new(capability));
    }

    pub fn contains(&self, id: &str) -> bool {
        self.capabilities.contains_key(id)
    }

    pub fn ids(&self) -> Vec<&str> {
        self.capabilities.keys().map(String::as_str).collect()
    }

    pub fn validate_sop(&self, sop: &Sop) -> Result<()> {
        for step in &sop.steps {
            if step.kind != SopStepKind::Capability {
                continue;
            }
            let id = step.capability_id().with_context(|| {
                format!(
                    "SOP '{}' step {} is kind=capability but has no capability id",
                    sop.name, step.number
                )
            })?;
            let capability = self.capabilities.get(id).with_context(|| {
                format!(
                    "SOP '{}' step {} references unknown capability '{}'",
                    sop.name, step.number, id
                )
            })?;
            if capability.requires_authored_input() {
                let authored = authored_capability_input(capability.as_ref(), step)?;
                if let Some(schema) = capability.describe().input_schema.as_ref() {
                    schema::validate_value(schema, &authored).with_context(|| {
                        format!(
                            "SOP '{}' step {} capability '{id}' authored input is invalid",
                            sop.name, step.number
                        )
                    })?;
                }
            }
        }
        Ok(())
    }

    pub fn execute_step(
        &self,
        ctx: CapabilityContext,
        step: &SopStep,
        piped_input: Value,
    ) -> Result<CapabilityResult> {
        let id = step
            .capability_id()
            .context("capability step missing capability id")?;
        let capability = self
            .capabilities
            .get(id)
            .with_context(|| format!("unknown SOP capability '{id}'"))?;
        let info = capability.describe();
        let input = if capability.requires_authored_input() {
            let mut authored = authored_capability_input(capability.as_ref(), step)?;
            // The authored object is the trusted configuration plane. The piped
            // value is always data, even when the author included an `input` key.
            authored
                .as_object_mut()
                .expect("authored capability input was validated as an object")
                .insert("input".to_string(), piped_input);
            authored
        } else {
            step.capability_call_input(piped_input)
        };

        if let Some(schema) = info.input_schema.as_ref() {
            schema::validate_value(schema, &input)
                .with_context(|| format!("capability '{id}' input schema validation failed"))?;
        }

        let result = capability.execute(ctx, input)?;
        if result.success
            && let Some(schema) = info.output_schema.as_ref()
        {
            schema::validate_value(schema, &result.output)
                .with_context(|| format!("capability '{id}' output schema validation failed"))?;
        }
        Ok(result)
    }
}

fn authored_capability_input(capability: &dyn SopCapability, step: &SopStep) -> Result<Value> {
    let configured = step.capability_input.clone().with_context(|| {
        format!(
            "capability '{}' requires authored `with` configuration",
            capability.id()
        )
    })?;
    if !configured.is_object() {
        bail!(
            "capability '{}' requires authored `with` configuration to be an object",
            capability.id()
        );
    }
    Ok(configured)
}

impl std::fmt::Debug for SopCapabilityRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SopCapabilityRegistry")
            .field("capabilities", &self.ids())
            .finish()
    }
}
