use zeroclaw_api::attribution::{Attributable, Role, ToolKind};
use zeroclaw_api::tool_attribution;

use crate::tools::ArcToolRef;
use crate::tools::cron_add::CronAddTool;
use crate::tools::cron_list::CronListTool;
use crate::tools::cron_remove::CronRemoveTool;
use crate::tools::cron_run::CronRunTool;
use crate::tools::cron_runs::CronRunsTool;
use crate::tools::cron_update::CronUpdateTool;
use crate::tools::delegate::DelegateTool;
use crate::tools::deliver_file::DeliverFileTool;
use crate::tools::file_read::FileReadTool;
use crate::tools::model_switch::ModelSwitchTool;
use crate::tools::read_skill::ReadSkillTool;
use crate::tools::schedule::ScheduleTool;
use crate::tools::security_ops::SecurityOpsTool;
use crate::tools::send_message_to_peer::SendMessageToPeerTool;
use crate::tools::shell::ShellTool;
use crate::tools::skill_http::SkillHttpTool;
use crate::tools::skill_manage::{SkillManageTool, SkillViewTool, SkillsListTool};
use crate::tools::skill_tool::{SkillBuiltinTool, SkillShellTool};
use crate::tools::sop_advance::SopAdvanceTool;
use crate::tools::sop_approve::SopApproveTool;
use crate::tools::sop_execute::SopExecuteTool;
use crate::tools::sop_list::SopListTool;
use crate::tools::sop_status::SopStatusTool;
use crate::tools::spawn_subagent::SpawnSubagentTool;
use crate::tools::verifiable_intent::VerifiableIntentTool;

tool_attribution!(CronAddTool, ToolKind::Plugin);
tool_attribution!(CronListTool, ToolKind::Plugin);
tool_attribution!(CronRemoveTool, ToolKind::Plugin);
tool_attribution!(CronRunTool, ToolKind::Plugin);
tool_attribution!(CronRunsTool, ToolKind::Plugin);
tool_attribution!(CronUpdateTool, ToolKind::Plugin);
tool_attribution!(DelegateTool, ToolKind::Plugin);
tool_attribution!(DeliverFileTool, ToolKind::Plugin);
tool_attribution!(FileReadTool, ToolKind::Plugin);
tool_attribution!(ModelSwitchTool, ToolKind::Plugin);
tool_attribution!(ReadSkillTool, ToolKind::Plugin);
tool_attribution!(ScheduleTool, ToolKind::Plugin);
tool_attribution!(SecurityOpsTool, ToolKind::Plugin);
tool_attribution!(SendMessageToPeerTool, ToolKind::Plugin);
tool_attribution!(ShellTool, ToolKind::Shell);
tool_attribution!(SkillHttpTool, ToolKind::Plugin);
tool_attribution!(SkillsListTool, ToolKind::Plugin);
tool_attribution!(SkillViewTool, ToolKind::Plugin);
tool_attribution!(SkillManageTool, ToolKind::Plugin);
tool_attribution!(SkillBuiltinTool, ToolKind::Plugin);
tool_attribution!(SkillShellTool, ToolKind::Plugin);
tool_attribution!(SopAdvanceTool, ToolKind::SopAdvance);
tool_attribution!(SopApproveTool, ToolKind::SopApprove);
tool_attribution!(SopExecuteTool, ToolKind::SopExecute);
tool_attribution!(SopListTool, ToolKind::SopList);
tool_attribution!(SopStatusTool, ToolKind::SopStatus);
tool_attribution!(SpawnSubagentTool, ToolKind::SpawnSubagent);
tool_attribution!(VerifiableIntentTool, ToolKind::Plugin);

impl Attributable for ArcToolRef {
    fn role(&self) -> Role {
        self.0.role()
    }
    fn alias(&self) -> &str {
        self.0.alias()
    }
}
