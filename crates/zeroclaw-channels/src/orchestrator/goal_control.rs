//! Goal control admission shares ordinary ingress checks and native delivery.

use super::*;
use zeroclaw_goal::GoalDiagnosticTextGuard as _;
use zeroclaw_runtime::goal_accounting::RuntimeGoalDiagnosticTextGuard;

type MatrixGoalCommandSnapshot = (
    String,
    ChannelMessage,
    std::result::Result<zeroclaw_commands::goal::GoalCommand, GoalCommandParseError>,
);

pub(super) fn matrix_goal_command_snapshot(
    context: &ChannelRuntimeContext,
    message: &ChannelMessage,
) -> Option<MatrixGoalCommandSnapshot> {
    if message.passive_context || !is_matrix_channel_name(&message.channel) {
        return None;
    }
    let parsed = parse_goal_command(&message.content);
    if matches!(parsed, Err(GoalCommandParseError::NotGoalCommand)) {
        return None;
    }
    Some((
        runtime_conversation_history_key(context, message),
        matrix_goal_message_snapshot(message),
        parsed,
    ))
}

pub(super) enum PreparedMatrixGoalCommand {
    Admitted(Box<goal_execution::MatrixGoalCommandSubmission>),
    Reply(String),
}

pub(super) async fn prepare_matrix_goal_command(
    context: Arc<ChannelRuntimeContext>,
    history_key: String,
    original: ChannelMessage,
    parsed: std::result::Result<zeroclaw_commands::goal::GoalCommand, GoalCommandParseError>,
) -> PreparedMatrixGoalCommand {
    match parsed {
        Ok(command) => match submit_matrix_goal(context, history_key, original, command).await {
            Ok(submission) => PreparedMatrixGoalCommand::Admitted(Box::new(submission)),
            Err(error) => {
                log_control_error("Goal command submission failed", &error);
                PreparedMatrixGoalCommand::Reply(channel_runtime_cli_string(
                    "goal-mode-command-failed",
                ))
            }
        },
        Err(_) => PreparedMatrixGoalCommand::Reply(channel_runtime_cli_string(
            "goal-mode-command-invalid",
        )),
    }
}

pub(super) async fn deliver_prepared_matrix_goal_command(
    context: Arc<ChannelRuntimeContext>,
    original: ChannelMessage,
    prepared: PreparedMatrixGoalCommand,
) {
    let channel = find_channel_for_message(&context.channels_by_name, &original).cloned();
    match prepared {
        PreparedMatrixGoalCommand::Admitted(submission) => {
            deliver_matrix_goal_command(*submission, channel, original).await;
        }
        PreparedMatrixGoalCommand::Reply(response) => {
            if let Some(channel) = channel
                && let Err(error) = channel
                    .send(&SendMessage::reply_to(&original, response))
                    .await
            {
                log_control_error("Goal command reply could not be delivered", &error);
            }
        }
    }
}

pub(super) fn should_drop_channel_message(
    channel: Option<&Arc<dyn Channel>>,
    message: &ChannelMessage,
) -> bool {
    let Some(channel) = channel else {
        return false;
    };
    if channel.drop_self_messages(message) {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(serde_json::json!({"sender": message.sender})),
            "dropping self-authored inbound message (self-loop guard, sdk layer)"
        );
        return true;
    }
    if zeroclaw_runtime::peers::should_drop_self_loop(
        &message.sender,
        channel.self_handle().as_deref(),
    ) {
        ::zeroclaw_log::record!(
            DEBUG,
            ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                .with_attrs(serde_json::json!({"sender": message.sender})),
            "dropping self-authored inbound message (self-loop guard, agent-loop fallback)"
        );
        return true;
    }
    false
}

pub(super) fn log_control_error(context: &str, error: &anyhow::Error) {
    ::zeroclaw_log::record!(
        WARN,
        ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Fail)
            .with_outcome(::zeroclaw_log::EventOutcome::Failure)
            .with_attrs(serde_json::json!({
                "error": zeroclaw_providers::sanitize_api_error(&RuntimeGoalDiagnosticTextGuard.guard_diagnostic_text(&format!("{error:#}"))),
            })),
        context
    );
}
