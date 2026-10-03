//! JSON Schema catalog for the runtime-owned wire types, behind
//! `schema-export`.
//!
//! These are the names `zeroclaw_rpc_proto::method::EXTERNAL_TYPES` assigns
//! to `zeroclaw-runtime`: their fields are runtime types, so the proto crate
//! cannot define them. `cargo generate openrpc` resolves a contract type in
//! the proto catalog first and then here.

use schemars::{Schema, SchemaGenerator};

use super::types::{
    CronAddParams, CronJob, CronListResult, CronRunsResult, DoctorRunResult, QuickstartApplyResult,
    QuickstartDismissParams, QuickstartFieldsParams, QuickstartFieldsResult,
    QuickstartValidateResult, SessionNewParams, SkillsListResult, SkillsReadResult,
    SkillsWriteParams, SopWireDraftParams,
};
use crate::sop::approval::ApprovalDecision;
use crate::sop::graph::RunOverlay;
use crate::sop::trigger_registry::TriggerSourceRegistry;
use crate::sop::types::Sop;

macro_rules! catalog {
    ($($ty:ident),* $(,)?) => {
        /// Names of every runtime-owned type this catalog can produce a
        /// schema for.
        pub const NAMES: &[&str] = &[$(stringify!($ty)),*];

        /// Register `name` with `generator` and return the subschema to
        /// reference it by, or `None` when the runtime does not own it.
        pub fn subschema_for_named(generator: &mut SchemaGenerator, name: &str) -> Option<Schema> {
            match name {
                $(stringify!($ty) => Some(generator.subschema_for::<$ty>()),)*
                _ => None,
            }
        }
    };
}

catalog! {
    DoctorRunResult, SessionNewParams, CronListResult, CronJob, CronAddParams, CronRunsResult,
    SkillsListResult, SkillsReadResult, SkillsWriteParams, QuickstartFieldsParams,
    QuickstartFieldsResult, QuickstartValidateResult, QuickstartApplyResult,
    QuickstartDismissParams, Sop, RunOverlay, TriggerSourceRegistry, SopWireDraftParams,
    ApprovalDecision,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use zeroclaw_rpc_proto::method::{EXTERNAL_TYPES, RUNTIME_DOCUMENT_TYPES};

    #[test]
    fn catalog_matches_the_runtime_owned_external_types() {
        let owned: BTreeSet<&str> = EXTERNAL_TYPES
            .iter()
            .filter(|(_, owner)| *owner == "zeroclaw-runtime")
            .map(|(name, _)| *name)
            .chain(RUNTIME_DOCUMENT_TYPES.iter().copied())
            .collect();
        let catalog: BTreeSet<&str> = NAMES.iter().copied().collect();
        assert_eq!(catalog.len(), NAMES.len(), "duplicate catalog entry");
        assert_eq!(
            catalog, owned,
            "the runtime catalog must cover exactly the runtime-owned external types"
        );

        let mut generator = SchemaGenerator::default();
        for name in NAMES {
            assert!(
                subschema_for_named(&mut generator, name).is_some(),
                "{name} has no schema"
            );
        }
    }
}
