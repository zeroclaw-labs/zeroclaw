//! Compatibility bridge to the extracted approval owner. Fluent rendering
//! remains runtime-owned; policy, decision state and tests live in kernel.
use zeroclaw_config::schema::RiskProfileConfig;
pub use zeroclaw_kernel::approval::{
    ApprovalLogEntry, ApprovalRequest, ApprovalRequirement, ApprovalResponse, MAX_REPLACEMENT_LEN,
    looks_like_secret_key, sanitize_tool_replacement, summarize_args,
};

pub struct ApprovalManager(zeroclaw_kernel::approval::ApprovalManager);
impl std::ops::Deref for ApprovalManager {
    type Target = zeroclaw_kernel::approval::ApprovalManager;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}
impl ApprovalManager {
    pub(crate) fn propagates_runtime_command_approval(&self) -> bool {
        self.0.propagates_runtime_command_approval()
    }
    pub fn from_risk_profile(profile: &RiskProfileConfig) -> Self {
        Self(zeroclaw_kernel::approval::ApprovalManager::from_risk_profile(profile))
    }
    pub fn for_non_interactive(profile: &RiskProfileConfig) -> Self {
        Self(zeroclaw_kernel::approval::ApprovalManager::for_non_interactive(profile))
    }
    pub fn for_non_interactive_backchannel(profile: &RiskProfileConfig) -> Self {
        Self(zeroclaw_kernel::approval::ApprovalManager::for_non_interactive_backchannel(profile))
    }
    pub(crate) fn for_bounded_non_interactive(profile: &RiskProfileConfig) -> Self {
        Self(zeroclaw_kernel::approval::ApprovalManager::for_bounded_non_interactive(profile))
    }
    pub fn derive_for_risk_profile(&self, profile: &RiskProfileConfig) -> Self {
        Self(self.0.derive_for_risk_profile(profile))
    }
    pub fn for_new_turn(&self) -> Self {
        Self(self.0.for_new_turn())
    }
    pub fn prompt_cli(&self, request: &ApprovalRequest) -> std::io::Result<ApprovalResponse> {
        let args = [("tool", request.tool_name.as_str())];
        let title = crate::i18n::get_required_cli_string_with_args("cli-approval-request", &args);
        let prompt = crate::i18n::get_required_cli_string_with_args("cli-approval-prompt", &args);
        self.0.prompt_cli(request, &title, &prompt)
    }
}
