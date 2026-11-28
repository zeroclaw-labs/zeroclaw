//! Tracked Matrix control delivery; waiting never owns command serialization.

use super::*;

pub(in crate::orchestrator) async fn deliver_matrix_goal_command(
    mut command: MatrixGoalCommandSubmission,
    channel: Option<Arc<dyn Channel>>,
    message: ChannelMessage,
) {
    if let Err(error) = send_response(channel.as_ref(), &message, command.response()).await {
        log_delivery_error(
            "Goal command acknowledgement could not be delivered",
            &error,
        );
        if let Err(error) = command.response_delivery_failed().await {
            log_delivery_error("Goal acknowledgement failure could not be settled", &error);
        }
        return;
    }

    // An idle/immediate command already committed. A queued acknowledgement
    // must be visible before waiting, and that wait must retain no bind lease.
    let admission = command.submission;
    command.submission = match admission {
        GoalControlAdmission::Direct(submission) => GoalControlAdmission::Direct(submission),
        GoalControlAdmission::Queued { deferred, .. } => {
            let ready = match deferred.wait().await {
                Ok(ready) => ready,
                Err(closed) => {
                    // The closed wait's durable response must be delivered,
                    // not interpreted as another lifecycle transition.
                    if let Err(error) =
                        send_response(channel.as_ref(), &message, closed.response()).await
                    {
                        log_delivery_error(
                            "Closed Goal control response could not be delivered",
                            &error,
                        );
                    }
                    log_delivery_error(
                        "Goal deferred control closed before admission",
                        &closed.into(),
                    );
                    return;
                }
            };
            let config = command.context.live_config.snapshot();
            let control_plane =
                match matrix_goal_control_plane(&command.context, &command.history_key) {
                    Some(control_plane) => control_plane,
                    None => {
                        drop(ready);
                        log_delivery_error(
                            "Goal deferred settings could not be resolved",
                            &anyhow::Error::msg("Goal control plane is unavailable"),
                        );
                        send_command_failed(channel.as_ref(), &message).await;
                        return;
                    }
                };
            let settings = match GoalHostSettings::from_config(
                &config,
                std::process::id(),
                control_plane.boot_id.clone(),
            ) {
                Ok(settings) => settings,
                Err(error) => {
                    drop(ready);
                    log_delivery_error("Goal deferred settings could not be resolved", &error);
                    send_command_failed(channel.as_ref(), &message).await;
                    return;
                }
            };
            let commit = match ready.commit(&settings).await {
                Ok(commit) => commit,
                Err(error) => {
                    log_delivery_error("Goal deferred command could not commit", &error);
                    if !error.is::<zeroclaw_goal::GoalLaunchFailureReported>() {
                        send_command_failed(channel.as_ref(), &message).await;
                    }
                    return;
                }
            };
            let before_release = matches!(
                commit.response(),
                GoalResponse::Started(_) | GoalResponse::Resumed(_)
            );
            if before_release {
                let delivery = send_response(channel.as_ref(), &message, commit.response()).await;
                command.submission = GoalControlAdmission::Direct(commit.release());
                if let Err(error) = delivery {
                    log_delivery_error(
                        "Goal deferred launch response could not be delivered",
                        &error,
                    );
                    if let Err(error) = command.response_delivery_failed().await {
                        log_delivery_error(
                            "Goal deferred launch failure could not be settled",
                            &error,
                        );
                    }
                    return;
                }
                finish_after_response(command, channel.as_ref(), &message, false).await;
                return;
            }
            // Release checkpoint and command ownership before draining a
            // predecessor. Budget and refused/stale outcomes publish afterwards.
            command.submission = GoalControlAdmission::Direct(commit.release());
            finish_after_response(command, channel.as_ref(), &message, true).await;
            return;
        }
    };
    finish_after_response(command, channel.as_ref(), &message, false).await;
}

async fn finish_after_response(
    mut command: MatrixGoalCommandSubmission,
    channel: Option<&Arc<dyn Channel>>,
    message: &ChannelMessage,
    publish_initial: bool,
) {
    match command.after_response().await {
        Ok(late) => {
            let response = late
                .as_ref()
                .or_else(|| publish_initial.then(|| command.response()));
            if let Some(response) = response
                && let Err(error) = send_response(channel, message, response).await
            {
                log_delivery_error("Final Goal command response could not be delivered", &error);
                return;
            }
            command.final_response_delivered(late.as_ref()).await;
        }
        Err(error) => {
            log_delivery_error("Goal command launch or drain could not finish", &error);
            if !error.is::<zeroclaw_goal::GoalLaunchFailureReported>() {
                send_command_failed(channel, message).await;
            }
        }
    }
}

async fn send_response(
    channel: Option<&Arc<dyn Channel>>,
    message: &ChannelMessage,
    response: &GoalResponse,
) -> Result<()> {
    let channel = channel.context("Matrix channel is unavailable for Goal response")?;
    channel
        .send(&SendMessage::reply_to(
            message,
            super::super::render_goal_response(response),
        ))
        .await
}

pub(super) async fn send_command_failed(
    channel: Option<&Arc<dyn Channel>>,
    message: &ChannelMessage,
) {
    if let Some(channel) = channel
        && let Err(error) = channel
            .send(&SendMessage::reply_to(
                message,
                super::super::channel_runtime_cli_string("goal-mode-command-failed"),
            ))
            .await
    {
        log_delivery_error(
            "Goal command failure response could not be delivered",
            &error,
        );
    }
}

fn log_delivery_error(context: &str, error: &anyhow::Error) {
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
