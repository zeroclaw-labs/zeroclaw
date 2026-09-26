//! The closed set of RPC methods and the single wire-name table.
//!
//! Every variant maps to exactly one wire string. `from_wire` is a table
//! scan; no hand-written string matching exists anywhere. The
//! authorization classification for each method (`Method::authz`) lives in
//! the runtime, because it names runtime grants; the shape of each method's
//! params and result lives here in [`Method::contract`], because clients
//! need it and the runtime does not.

// ── Method registry ──────────────────────────────────────────────
//
// Single source of truth. Every variant maps to exactly one wire
// string. `from_wire` is a table scan — no hand-written string
// matching anywhere in this file.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    // Core
    Initialize,
    Status,
    Health,
    DoctorRun,

    // Sessions (agent chat lives here — session/prompt + session/update
    // notifications is the RPC equivalent of the gateway's ws/chat)
    SessionNew,
    SessionClose,
    SessionPrompt,
    SessionConfigure,
    SessionCancel,
    SessionGitBranch,
    SessionList,
    SessionListAcp,
    SessionMessages,
    SessionState,
    SessionDelete,
    SessionApprove,
    SessionKill,

    // Memory
    MemoryList,
    MemorySearch,
    MemoryGet,
    MemoryStore,
    MemoryDelete,

    // Cron
    CronList,
    CronGet,
    CronAdd,
    CronPatch,
    CronDelete,
    CronRuns,
    CronTrigger,
    CronSettings,

    // Config
    ConfigGet,
    ConfigSet,
    ConfigValidate,
    ConfigReload,
    ConfigList,
    ConfigDelete,
    ConfigMapKeys,
    ConfigResolveAliasSource,
    ConfigMapKeyCreate,
    ConfigMapKeyDelete,
    ConfigMapKeyRename,
    ConfigTemplates,

    // Agents
    AgentsList,
    AgentsStatus,

    // Cost
    CostQuery,
    CostOrg,

    // Skills
    SkillsBundles,
    SkillsList,
    SkillsRead,
    SkillsWrite,
    SkillsDelete,

    // Personality
    PersonalityList,
    PersonalityGet,
    PersonalityPut,
    PersonalityTemplates,

    // Config introspection (sections, catalog, status)
    ConfigSections,
    ConfigStatus,
    ConfigCatalog,
    ConfigCatalogModels,

    // Logs / Events
    LogsSubscribe,
    LogsQuery,
    LogsGet,

    // TUI
    TuiList,

    // Files
    FileAttach,
    FsListDir,

    // Locales
    LocalesList,
    LocalesFetch,

    // Quickstart (TUI mirror of `/api/quickstart/*` HTTP routes)
    QuickstartState,
    QuickstartFields,
    QuickstartValidate,
    QuickstartApply,
    QuickstartDismiss,

    // Certificates (mTLS client-cert lifecycle)
    CertRenew,

    SopsList,
    SopsGet,
    SopsGraph,
    SopsRun,
    SopsRuns,
    SopsRunDetail,
    SopsRunOverlay,
    SopsValidate,
    SopsSave,
    SopsCreate,
    SopsDelete,
    SopsRename,
    SopsDecide,
    SopsWireDraft,
    SopsGraphDraft,
    SopsTriggerSources,
    ToolsParamOptions,
}

impl Method {
    /// The single table. Wire name ↔ variant, defined once.
    pub const ALL: &[(Method, &str)] = &[
        (Method::Initialize, "initialize"),
        (Method::Status, "status"),
        (Method::Health, "health"),
        (Method::DoctorRun, "doctor/run"),
        // Sessions
        (Method::SessionNew, "session/new"),
        (Method::SessionClose, "session/close"),
        (Method::SessionPrompt, "session/prompt"),
        (Method::SessionConfigure, "session/configure"),
        (Method::SessionCancel, "session/cancel"),
        (Method::SessionGitBranch, "session/git_branch"),
        (Method::SessionList, "session/list"),
        (Method::SessionListAcp, "session/list-acp"),
        (Method::SessionMessages, "session/messages"),
        (Method::SessionState, "session/state"),
        (Method::SessionDelete, "session/delete"),
        (Method::SessionApprove, "session/approve"),
        (Method::SessionKill, "session/kill"),
        // Memory
        (Method::MemoryList, "memory/list"),
        (Method::MemorySearch, "memory/search"),
        (Method::MemoryGet, "memory/get"),
        (Method::MemoryStore, "memory/store"),
        (Method::MemoryDelete, "memory/delete"),
        // Cron
        (Method::CronList, "cron/list"),
        (Method::CronGet, "cron/get"),
        (Method::CronAdd, "cron/add"),
        (Method::CronPatch, "cron/patch"),
        (Method::CronDelete, "cron/delete"),
        (Method::CronRuns, "cron/runs"),
        (Method::CronTrigger, "cron/trigger"),
        (Method::CronSettings, "cron/settings"),
        // Config
        (Method::ConfigGet, "config/get"),
        (Method::ConfigSet, "config/set"),
        (Method::ConfigValidate, "config/validate"),
        (Method::ConfigReload, "config/reload"),
        (Method::ConfigList, "config/list"),
        (Method::ConfigDelete, "config/delete"),
        (Method::ConfigMapKeys, "config/map-keys"),
        (
            Method::ConfigResolveAliasSource,
            "config/resolve-alias-source",
        ),
        (Method::ConfigMapKeyCreate, "config/map-key-create"),
        (Method::ConfigMapKeyDelete, "config/map-key-delete"),
        (Method::ConfigMapKeyRename, "config/map-key-rename"),
        (Method::ConfigTemplates, "config/templates"),
        // Agents
        (Method::AgentsList, "agents/list"),
        (Method::AgentsStatus, "agents/status"),
        // Cost
        (Method::CostQuery, "cost/query"),
        (Method::CostOrg, "cost/org"),
        // Skills
        (Method::SkillsBundles, "skills/bundles"),
        (Method::SkillsList, "skills/list"),
        (Method::SkillsRead, "skills/read"),
        (Method::SkillsWrite, "skills/write"),
        (Method::SkillsDelete, "skills/delete"),
        // Personality
        (Method::PersonalityList, "personality/list"),
        (Method::PersonalityGet, "personality/get"),
        (Method::PersonalityPut, "personality/put"),
        (Method::PersonalityTemplates, "personality/templates"),
        // Config introspection
        (Method::ConfigSections, "config/sections"),
        (Method::ConfigStatus, "config/status"),
        (Method::ConfigCatalog, "config/catalog"),
        (Method::ConfigCatalogModels, "config/catalog-models"),
        // Logs
        (Method::LogsSubscribe, "logs/subscribe"),
        (Method::LogsQuery, "logs/query"),
        (Method::LogsGet, "logs/get"),
        // TUI
        (Method::TuiList, "tui/list"),
        // Files
        (Method::FileAttach, "file/attach"),
        (Method::FsListDir, "fs/list_dir"),
        // Locales
        (Method::LocalesList, "locales/list"),
        (Method::LocalesFetch, "locales/fetch"),
        // Quickstart
        (Method::QuickstartState, "quickstart/state"),
        (Method::QuickstartFields, "quickstart/fields"),
        (Method::QuickstartValidate, "quickstart/validate"),
        (Method::QuickstartApply, "quickstart/apply"),
        (Method::QuickstartDismiss, "quickstart/dismiss"),
        (Method::CertRenew, "cert/renew"),
        (Method::SopsList, "sops/list"),
        (Method::SopsGet, "sops/get"),
        (Method::SopsGraph, "sops/graph"),
        (Method::SopsRun, "sops/run"),
        (Method::SopsRuns, "sops/runs"),
        (Method::SopsRunDetail, "sops/run-detail"),
        (Method::SopsRunOverlay, "sops/run-overlay"),
        (Method::SopsValidate, "sops/validate"),
        (Method::SopsSave, "sops/save"),
        (Method::SopsCreate, "sops/create"),
        (Method::SopsDelete, "sops/delete"),
        (Method::SopsRename, "sops/rename"),
        (Method::SopsDecide, "sops/decide"),
        (Method::SopsWireDraft, "sops/wire-draft"),
        (Method::SopsGraphDraft, "sops/graph-draft"),
        (Method::SopsTriggerSources, "sops/trigger-sources"),
        (Method::ToolsParamOptions, "tools/param-options"),
    ];

    /// Resolve a wire method name to a variant. Table scan, no hand-written
    /// string matching.
    pub fn from_wire(s: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .find(|(_, wire)| *wire == s)
            .map(|(m, _)| *m)
    }

    /// Wire name for this variant.
    pub fn wire_name(self) -> &'static str {
        Self::ALL
            .iter()
            .find(|(m, _)| *m == self)
            .map(|(_, wire)| *wire)
            .expect("every variant is in ALL")
    }

    /// The params and result shape of this method.
    ///
    /// The match is arm-complete over the closed enum, so a new method
    /// cannot be added without declaring what it accepts and returns. Type
    /// names refer to [`crate::types`] when the type is defined there,
    /// otherwise to the crate named in [`Shape::Typed`]'s documentation.
    pub const fn contract(self) -> MethodContract {
        use Method as M;
        use Shape::{None, Typed, Untyped};
        let (params, result) = match self {
            // Core
            M::Initialize => (Typed("InitializeParams"), Typed("InitializeResult")),
            M::Status => (None, Typed("StatusResult")),
            M::Health => (None, Untyped),
            M::DoctorRun => (None, Typed("DoctorRunResult")),

            // Sessions
            M::SessionNew => (Typed("SessionNewParams"), Typed("SessionNewResult")),
            M::SessionClose => (Typed("SessionIdParams"), Typed("SessionCloseResult")),
            M::SessionPrompt => (Typed("SessionPromptParams"), Typed("SessionPromptResult")),
            M::SessionConfigure => (
                Typed("SessionConfigureParams"),
                Typed("SessionConfigureResult"),
            ),
            M::SessionCancel => (Typed("SessionIdParams"), Typed("SessionCancelResult")),
            M::SessionGitBranch => (Typed("SessionIdParams"), Typed("SessionGitBranchResult")),
            M::SessionList => (Typed("SessionListParams"), Typed("SessionListResult")),
            M::SessionListAcp => (None, Typed("SessionListResult")),
            M::SessionMessages => (
                Typed("SessionMessagesParams"),
                Typed("SessionMessagesResult"),
            ),
            M::SessionState => (Typed("SessionIdParams"), Typed("SessionStateResult")),
            M::SessionDelete => (Typed("SessionIdParams"), Typed("SessionDeleteResult")),
            M::SessionApprove => (Typed("SessionApproveParams"), Typed("SessionApproveResult")),
            M::SessionKill => (Typed("SessionKillParams"), Typed("SessionKillResult")),

            // Memory
            M::MemoryList => (Typed("MemoryListParams"), Typed("MemoryListResult")),
            M::MemorySearch => (Typed("MemorySearchParams"), Typed("MemorySearchResult")),
            M::MemoryGet => (Typed("MemoryGetParams"), Typed("MemoryGetResult")),
            M::MemoryStore => (Typed("MemoryStoreParams"), Typed("MemoryStoreResult")),
            M::MemoryDelete => (Typed("MemoryDeleteParams"), Typed("MemoryDeleteResult")),

            // Cron
            M::CronList => (None, Typed("CronListResult")),
            M::CronGet => (Typed("CronIdParams"), Typed("CronJob")),
            M::CronAdd => (Typed("CronAddParams"), Typed("CronJob")),
            M::CronPatch => (Typed("CronPatchParams"), Typed("CronJob")),
            M::CronDelete => (Typed("CronIdParams"), Typed("CronDeleteResult")),
            M::CronRuns => (Typed("CronRunsParams"), Typed("CronRunsResult")),
            M::CronTrigger => (Typed("CronIdParams"), Typed("CronTriggerResult")),
            M::CronSettings => (Untyped, Untyped),

            // Config
            M::ConfigGet => (Typed("ConfigGetParams"), Untyped),
            M::ConfigSet => (Typed("ConfigSetParams"), Typed("ConfigSetResult")),
            M::ConfigValidate => (None, Typed("ConfigValidateResult")),
            M::ConfigReload => (None, Typed("ConfigReloadResult")),
            M::ConfigList => (Typed("ConfigListParams"), Typed("ConfigListResult")),
            M::ConfigDelete => (Typed("ConfigDeleteParams"), Typed("ConfigDeleteResult")),
            M::ConfigMapKeys => (Typed("ConfigMapKeysParams"), Typed("ConfigMapKeysResult")),
            M::ConfigResolveAliasSource => (
                Typed("ConfigResolveAliasSourceParams"),
                Typed("ConfigResolveAliasSourceResult"),
            ),
            M::ConfigMapKeyCreate => (
                Typed("ConfigMapKeyCreateParams"),
                Typed("ConfigMapKeyCreateResult"),
            ),
            M::ConfigMapKeyDelete => (
                Typed("ConfigMapKeyDeleteParams"),
                Typed("ConfigMapKeyDeleteResult"),
            ),
            M::ConfigMapKeyRename => (
                Typed("ConfigMapKeyRenameParams"),
                Typed("ConfigMapKeyRenameResult"),
            ),
            M::ConfigTemplates => (None, Typed("ConfigTemplatesResult")),
            M::ConfigSections => (None, Typed("ConfigSectionsResult")),
            M::ConfigStatus => (None, Typed("ConfigStatusResult")),
            M::ConfigCatalog => (None, Typed("CatalogResponse")),
            M::ConfigCatalogModels => (Typed("CatalogModelsParams"), Typed("CatalogModelsResult")),

            // Agents and cost
            M::AgentsList => (None, Typed("AgentsListResult")),
            M::AgentsStatus => (None, Typed("AgentsStatusResult")),
            M::CostQuery => (Typed("CostQueryParams"), Typed("CostSummary")),
            M::CostOrg => (None, Untyped),

            // Skills and personality
            M::SkillsBundles => (None, Typed("SkillsBundlesResult")),
            M::SkillsList => (Typed("SkillsListParams"), Typed("SkillsListResult")),
            M::SkillsRead => (Typed("SkillsReadParams"), Typed("SkillsReadResult")),
            M::SkillsWrite => (Typed("SkillsWriteParams"), Typed("SkillsWriteResult")),
            M::SkillsDelete => (Typed("SkillsDeleteParams"), Typed("SkillsDeleteResult")),
            M::PersonalityList => (
                Typed("PersonalityListParams"),
                Typed("PersonalityListResult"),
            ),
            M::PersonalityGet => (Typed("PersonalityGetParams"), Typed("PersonalityGetResult")),
            M::PersonalityPut => (Typed("PersonalityPutParams"), Typed("PersonalityPutResult")),
            M::PersonalityTemplates => (
                Typed("PersonalityTemplatesParams"),
                Typed("PersonalityTemplatesResult"),
            ),

            // Logs, TUI, files, locales
            M::LogsSubscribe => (None, Typed("LogsSubscribeResult")),
            M::LogsQuery => (Typed("LogsQueryParams"), Typed("LogsQueryResult")),
            M::LogsGet => (Typed("LogsGetParams"), Typed("LogsGetResult")),
            M::TuiList => (None, Typed("TuiListResult")),
            M::FileAttach => (Typed("FileAttachParams"), Typed("FileAttachResult")),
            M::FsListDir => (Typed("FsListDirRequest"), Typed("FsListDirResponse")),
            M::LocalesList => (None, Untyped),
            M::LocalesFetch => (Untyped, Untyped),

            // Quickstart
            M::QuickstartState => (None, Typed("QuickstartStateResult")),
            M::QuickstartFields => (
                Typed("QuickstartFieldsParams"),
                Typed("QuickstartFieldsResult"),
            ),
            M::QuickstartValidate => (
                Typed("QuickstartValidateParams"),
                Typed("QuickstartValidateResult"),
            ),
            M::QuickstartApply => (
                Typed("QuickstartApplyParams"),
                Typed("QuickstartApplyResult"),
            ),
            M::QuickstartDismiss => (
                Typed("QuickstartDismissParams"),
                Typed("QuickstartDismissResult"),
            ),

            // Transport-authenticated certificate renewal
            M::CertRenew => (Untyped, Untyped),

            // SOPs
            M::SopsList => (None, Untyped),
            M::SopsGet => (Typed("SopSelectRequest"), Typed("Sop")),
            M::SopsGraph => (Typed("SopSelectRequest"), Typed("SopGraph")),
            M::SopsRun => (Typed("SopRunRequest"), Typed("SopRunResponse")),
            M::SopsRuns => (Typed("SopRunsRequest"), Untyped),
            M::SopsRunDetail => (Typed("SopRunDetailRequest"), Untyped),
            M::SopsRunOverlay => (Typed("SopRunOverlayRequest"), Typed("RunOverlay")),
            M::SopsValidate => (Untyped, Untyped),
            M::SopsSave => (Typed("SopSaveRequest"), Untyped),
            M::SopsCreate => (Typed("SopSaveRequest"), Untyped),
            M::SopsDelete => (Typed("SopSelectRequest"), Untyped),
            M::SopsRename => (Typed("SopRenameRequest"), Untyped),
            M::SopsDecide => (Typed("SopDecideRequest"), Typed("RunOverlay")),
            M::SopsWireDraft => (Untyped, Untyped),
            M::SopsGraphDraft => (Untyped, Typed("SopGraph")),
            M::SopsTriggerSources => (None, Typed("TriggerSourceRegistry")),
            M::ToolsParamOptions => (Untyped, Untyped),
        };
        MethodContract { params, result }
    }
}

/// The shape of one side (params or result) of a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shape {
    /// The method takes no params, or returns no structured result.
    None,
    /// A named wire type. Defined in [`crate::types`] when the schema
    /// catalog knows it; otherwise owned by another crate and listed in
    /// [`EXTERNAL_TYPES`].
    Typed(&'static str),
    /// A free-form JSON value the daemon shapes at runtime.
    Untyped,
}

/// The declared params and result shape of one method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MethodContract {
    pub params: Shape,
    pub result: Shape,
}

/// Wire types named by a [`MethodContract`] that are owned by another crate
/// and therefore carry no schema in this crate's catalog. Each entry names
/// the owning crate so the contract document can say where to look.
pub const EXTERNAL_TYPES: &[(&str, &str)] = &[
    // zeroclaw-runtime: fields are runtime-owned types (diagnostics, cron
    // jobs, skill frontmatter, quickstart descriptors, SOP definitions).
    ("DoctorRunResult", "zeroclaw-runtime"),
    ("SessionNewParams", "zeroclaw-runtime"),
    ("CronListResult", "zeroclaw-runtime"),
    ("CronJob", "zeroclaw-runtime"),
    ("CronAddParams", "zeroclaw-runtime"),
    ("CronRunsResult", "zeroclaw-runtime"),
    ("SkillsListResult", "zeroclaw-runtime"),
    ("SkillsReadResult", "zeroclaw-runtime"),
    ("SkillsWriteParams", "zeroclaw-runtime"),
    ("QuickstartFieldsParams", "zeroclaw-runtime"),
    ("QuickstartFieldsResult", "zeroclaw-runtime"),
    ("QuickstartValidateResult", "zeroclaw-runtime"),
    ("QuickstartApplyResult", "zeroclaw-runtime"),
    ("QuickstartDismissParams", "zeroclaw-runtime"),
    ("Sop", "zeroclaw-runtime"),
    ("RunOverlay", "zeroclaw-runtime"),
    ("TriggerSourceRegistry", "zeroclaw-runtime"),
    // zeroclaw-api: JSON-RPC envelope-adjacent request types without schema
    // derives.
    ("FsListDirRequest", "zeroclaw-api"),
    ("FsListDirResponse", "zeroclaw-api"),
    ("SopSelectRequest", "zeroclaw-api"),
    ("SopRunRequest", "zeroclaw-api"),
    ("SopRunResponse", "zeroclaw-api"),
    ("SopRunsRequest", "zeroclaw-api"),
    ("SopRunDetailRequest", "zeroclaw-api"),
    ("SopRunOverlayRequest", "zeroclaw-api"),
    ("SopSaveRequest", "zeroclaw-api"),
    ("SopRenameRequest", "zeroclaw-api"),
    ("SopDecideRequest", "zeroclaw-api"),
    // zeroclaw-config: no schema derive on the cost summary.
    ("CostSummary", "zeroclaw-config"),
];

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn method_from_wire_roundtrip() {
        for (method, wire) in Method::ALL {
            assert_eq!(Method::from_wire(wire), Some(*method), "{wire}");
            assert_eq!(method.wire_name(), *wire);
        }
    }

    #[test]
    fn method_from_wire_unknown() {
        assert_eq!(Method::from_wire("no/such-method"), None);
    }

    #[test]
    fn wire_names_and_variants_are_unique() {
        let wires: BTreeSet<_> = Method::ALL.iter().map(|(_, w)| *w).collect();
        assert_eq!(wires.len(), Method::ALL.len(), "duplicate wire name");
        let variants: BTreeSet<_> = Method::ALL.iter().map(|(m, _)| format!("{m:?}")).collect();
        assert_eq!(
            variants.len(),
            Method::ALL.len(),
            "duplicate variant in ALL"
        );
    }

    #[test]
    fn external_types_are_unique_and_referenced() {
        let names: BTreeSet<_> = EXTERNAL_TYPES.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), EXTERNAL_TYPES.len(), "duplicate external type");
        let referenced: BTreeSet<&str> = Method::ALL
            .iter()
            .flat_map(|(m, _)| {
                let c = m.contract();
                [c.params, c.result]
            })
            .filter_map(|s| match s {
                Shape::Typed(name) => Some(name),
                _ => None,
            })
            .collect();
        for name in &names {
            assert!(
                referenced.contains(name),
                "{name} is listed as external but no method contract names it"
            );
        }
    }
}
