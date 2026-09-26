//! JSON Schema catalog for the wire types, behind `schema-export`.
//!
//! `cargo generate openrpc` asks this module for the schema of each type a
//! [`crate::Method::contract`] names. A type this crate does not define is
//! not here; the generator then records it as external, with its owning
//! crate from [`crate::method::EXTERNAL_TYPES`].

use crate::types::*;
use schemars::{Schema, SchemaGenerator};

/// One list of every schema-bearing wire type, by name. Adding a type to
/// [`crate::types`] and forgetting it here is caught by
/// `catalog_names_match_types` in the unit tests below.
macro_rules! catalog {
    ($($ty:ident),* $(,)?) => {
        /// Names of every type the catalog can produce a schema for.
        pub const NAMES: &[&str] = &[$(stringify!($ty)),*];

        /// Register `name` with `generator` and return the subschema to
        /// reference it by, or `None` when this crate does not define it.
        pub fn subschema_for_named(generator: &mut SchemaGenerator, name: &str) -> Option<Schema> {
            match name {
                $(stringify!($ty) => Some(generator.subschema_for::<$ty>()),)*
                _ => None,
            }
        }

        #[cfg(test)]
        fn assert_all_implement_json_schema() {
            fn probe<T: schemars::JsonSchema>() {}
            $(probe::<$ty>();)*
        }
    };
}

catalog! {
    // Core
    InitializeParams, CommandDescriptor, InitializeResult, StatusResult, DoctorSummary,
    // TUI
    TuiListEntry, TuiListResult,
    // Sessions
    SessionIdParams, ChatMode, SessionNewResult, SessionCloseResult, SessionKillParams,
    SessionKillResult, SessionPromptParams, SessionPromptResult, SessionConfigureParams,
    SessionConfigureResult, SessionCancelResult, SessionGitBranchResult, SessionListParams,
    SessionListResult, SessionEntry, SessionMessagesResult, SessionMessagesParams,
    MessageEntryKind, MessageEntry, SessionStateResult, SessionDeleteResult, SessionOverrides,
    // Memory
    MemoryListParams, MemoryListResult, MemorySearchParams, MemorySearchResult, MemoryGetParams,
    MemoryGetResult, MemoryStoreParams, MemoryStoreResult, MemoryDeleteParams, MemoryDeleteResult,
    // Cron
    CronIdParams, CronPatchParams, CronDeleteResult, CronRunsParams, CronTriggerResult,
    // Config
    ConfigGetParams, ConfigGetPropResult, ConfigSetParams, ConfigSetResult, ConfigValidateResult,
    ConfigReloadResult, ConfigListParams, ConfigListResult, ConfigDeleteParams, ConfigDeleteResult,
    ConfigMapKeysParams, ConfigMapKeysResult, ConfigResolveAliasSourceParams,
    ConfigResolveAliasSourceResult, ConfigMapKeyCreateParams, ConfigMapKeyCreateResult,
    ConfigMapKeyDeleteParams, ConfigMapKeyDeleteResult, ConfigMapKeyRenameParams,
    ConfigMapKeyRenameResult, ConfigTemplateEntry, ConfigTemplatesResult,
    // Agents and cost
    AgentEntry, AgentsListResult, AgentStatusEntry, AgentsStatusResult, CostQueryParams,
    // Skills
    SkillBundleEntry, SkillsBundlesResult, SkillsListParams, AgentSkillEntry, ShadowedSkillEntry,
    DroppedSkillEntry, AgentSkillsResult, SkillsReadParams, SkillsWriteResult, SkillsDeleteParams,
    SkillsDeleteResult,
    // Personality
    PersonalityListParams, PersonalityFileEntry, PersonalityListResult, PersonalityGetParams,
    PersonalityGetResult, PersonalityPutParams, PersonalityPutResult, PersonalityTemplatesParams,
    TemplateFileEntry, PersonalityTemplatesResult,
    // Config introspection
    CatalogModelProvider, CatalogResponse, CatalogModelsParams, CatalogModelsResult,
    ConfigSectionEntry, ConfigSectionsResult, ConfigStatusResult, PickerItem, PickerResponse,
    SectionSelectParams, SelectItemResponse,
    // Files
    FileSource, FileEntry, FileAttachParams, FileEntryResult, FileAttachResult,
    // Approval
    SessionApproveParams, SessionApproveResult,
    // Logs
    LogsSubscribeResult, LogsQueryParams, LogsQueryResult, LogsGetParams, LogsGetResult,
    // Notifications
    SessionUpdateEvent, TurnCompletionOutcome,
    // Quickstart (wire-stable subset)
    QuickstartStateResult, QuickstartTypeOption, QuickstartValidateParams, QuickstartApplyParams,
    QuickstartDismissResult,
    // SOP graph projection
    SopGraph, GraphLegend,
}

use crate::sop::{GraphLegend, SopGraph};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Method;
    use crate::method::{EXTERNAL_TYPES, Shape};
    use std::collections::BTreeSet;

    #[test]
    fn every_catalog_type_implements_json_schema() {
        assert_all_implement_json_schema();
        let names: BTreeSet<_> = NAMES.iter().copied().collect();
        assert_eq!(names.len(), NAMES.len(), "duplicate catalog entry");
    }

    #[test]
    fn every_typed_contract_resolves_to_a_schema_or_an_external_owner() {
        let external: BTreeSet<&str> = EXTERNAL_TYPES.iter().map(|(n, _)| *n).collect();
        let mut generator = SchemaGenerator::default();
        for (method, wire) in Method::ALL {
            let contract = method.contract();
            for shape in [contract.params, contract.result] {
                let Shape::Typed(name) = shape else { continue };
                let in_catalog = subschema_for_named(&mut generator, name).is_some();
                assert!(
                    in_catalog != external.contains(name),
                    "{wire}: {name} must be exactly one of catalog-defined or external \
                     (catalog={in_catalog}, external={})",
                    external.contains(name)
                );
            }
        }
    }

    #[test]
    fn notification_payloads_resolve() {
        let mut generator = SchemaGenerator::default();
        for (name, payload) in crate::notification::ALL {
            if let Some(payload) = payload {
                assert!(
                    subschema_for_named(&mut generator, payload).is_some(),
                    "{name}: payload type {payload} is not in the catalog"
                );
            }
        }
    }
}
