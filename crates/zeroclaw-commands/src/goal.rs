//! Transport-neutral parsing for the fixed Goal Mode V1 command grammar.

/// Maximum accepted length of the declared success criterion.
pub const MAX_GOAL_OBJECTIVE_CHARS: usize = 4096;

/// Return whether a finite token budget is positive and can be represented by
/// the canonical SQLite task plane.
///
/// This predicate is shared by command parsing and direct runtime admission so
/// a typed command cannot accept a finite token value the grammar rejects.
pub fn is_valid_finite_goal_token_limit(value: u64) -> bool {
    value > 0 && value <= i64::MAX as u64
}

/// Return whether a finite cost budget is usable for Goal admission.
///
/// This predicate is shared by command parsing and direct runtime admission so
/// a typed command cannot accept a finite cost value the grammar rejects.
pub fn is_valid_finite_goal_cost_limit(value: f64) -> bool {
    value.is_finite() && value > 0.0
}

/// A finite limit selected by a command. `None` means unlimited in that
/// dimension; it is deliberately distinct from omitted command flags.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GoalBudgetLimits {
    pub token_limit: Option<u64>,
    pub cost_limit_usd: Option<f64>,
}

/// Budget selection supplied by `start` or `budget set`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum GoalBudgetSelection {
    /// Use the configured defaults (valid only for `start`).
    Defaults,
    /// Replace both dimensions; omitted dimensions are unlimited.
    Limits(GoalBudgetLimits),
    /// Explicitly select unlimited operation.
    Unlimited,
}

/// Parsed command text. It contains no authority, route, task, or lifecycle
/// facts; adapters supply those only to the later runtime admission boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum GoalCommand {
    Start {
        budget: GoalBudgetSelection,
        objective: String,
    },
    Status,
    Budget,
    SetBudget(GoalBudgetSelection),
    Pause {
        immediate: bool,
    },
    /// An optional, verbatim response to a previous Goal-owned request.
    ///
    /// The parser deliberately preserves embedded newlines so transports can
    /// append the response to the same durable session before resuming work.
    Resume {
        response: Option<String>,
    },
    Cancel,
    Help,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalCommandParseError {
    NotGoalCommand,
    MissingSubcommand,
    UnknownSubcommand(String),
    MissingObjective,
    MissingFlagValue(&'static str),
    InvalidTokenLimit(String),
    InvalidCostLimit(String),
    DuplicateFlag(&'static str),
    ConflictingBudgetFlags,
    UnknownFlag(String),
    UnexpectedArguments(&'static str),
    TextTooLong { max: usize },
}

/// Parse exactly one fixed V1 `/goal` command. The caller is responsible for
/// determining whether the command is permitted on its transport.
pub fn parse_goal_command(content: &str) -> Result<GoalCommand, GoalCommandParseError> {
    let (token, rest) = split_token(content.trim_start());
    if !token.eq_ignore_ascii_case("/goal") {
        return Err(GoalCommandParseError::NotGoalCommand);
    }

    let (subcommand, arguments) = split_token(rest.trim_start());
    if subcommand.is_empty() {
        return Err(GoalCommandParseError::MissingSubcommand);
    }

    let normalized_subcommand = subcommand.to_ascii_lowercase();
    match normalized_subcommand.as_str() {
        "start" => parse_start(arguments),
        "status" => argument_free(arguments, "status", GoalCommand::Status),
        "budget" => parse_budget(arguments),
        "pause" => parse_pause(arguments),
        "resume" => parse_resume(arguments),
        "cancel" => argument_free(arguments, "cancel", GoalCommand::Cancel),
        "help" => argument_free(arguments, "help", GoalCommand::Help),
        _ => Err(GoalCommandParseError::UnknownSubcommand(
            normalized_subcommand,
        )),
    }
}

fn parse_pause(arguments: &str) -> Result<GoalCommand, GoalCommandParseError> {
    match arguments.trim() {
        "" => Ok(GoalCommand::Pause { immediate: false }),
        value if value.eq_ignore_ascii_case("now") => Ok(GoalCommand::Pause { immediate: true }),
        _ => Err(GoalCommandParseError::UnexpectedArguments("pause")),
    }
}

fn parse_resume(arguments: &str) -> Result<GoalCommand, GoalCommandParseError> {
    let response = (!arguments.trim().is_empty()).then(|| arguments.to_owned());
    Ok(GoalCommand::Resume { response })
}

fn split_token(input: &str) -> (&str, &str) {
    input
        .split_once(char::is_whitespace)
        .map_or((input, ""), |(token, rest)| (token, rest))
}

fn parse_start(arguments: &str) -> Result<GoalCommand, GoalCommandParseError> {
    let mut remaining = arguments;
    let mut flags = String::new();

    loop {
        let trimmed = remaining.trim_start();
        let (word, rest) = split_token(trimmed);
        if word.is_empty() {
            validate_goal_objective(remaining)?;
            return Err(GoalCommandParseError::MissingObjective);
        }

        match word {
            "--unlimited" => {
                flags.push_str(word);
                flags.push(' ');
                remaining = rest;
            }
            "--tokens" | "--cost-usd" => {
                let flag: &'static str = if word == "--tokens" {
                    "--tokens"
                } else {
                    "--cost-usd"
                };
                let (value, after_value) = split_token(rest.trim_start());
                if value.is_empty() {
                    return Err(GoalCommandParseError::MissingFlagValue(flag));
                }
                flags.push_str(word);
                flags.push(' ');
                flags.push_str(value);
                flags.push(' ');
                remaining = after_value;
            }
            _ if word.starts_with("--") => {
                return Err(GoalCommandParseError::UnknownFlag(word.to_string()));
            }
            _ => break,
        }
    }

    let objective = remaining;
    validate_goal_objective(objective)?;

    Ok(GoalCommand::Start {
        budget: parse_budget_selection(&flags, true)?,
        objective: objective.to_string(),
    })
}

/// Validate an objective supplied through either parsed or typed Goal input.
///
/// The runtime also calls this for directly constructed [`GoalCommand::Start`]
/// values so command parsing and durable admission cannot drift.
pub fn validate_goal_objective(objective: &str) -> Result<(), GoalCommandParseError> {
    let mut has_non_whitespace = false;
    for (index, character) in objective.chars().enumerate() {
        if index == MAX_GOAL_OBJECTIVE_CHARS {
            return Err(GoalCommandParseError::TextTooLong {
                max: MAX_GOAL_OBJECTIVE_CHARS,
            });
        }
        has_non_whitespace |= !character.is_whitespace();
    }

    has_non_whitespace
        .then_some(())
        .ok_or(GoalCommandParseError::MissingObjective)
}

fn parse_budget(arguments: &str) -> Result<GoalCommand, GoalCommandParseError> {
    let (subcommand, flags) = split_token(arguments.trim());
    if subcommand.is_empty() {
        return Ok(GoalCommand::Budget);
    }
    if !subcommand.eq_ignore_ascii_case("set") {
        return Err(GoalCommandParseError::UnexpectedArguments("budget"));
    }

    Ok(GoalCommand::SetBudget(parse_budget_selection(
        flags, false,
    )?))
}

fn argument_free(
    arguments: &str,
    name: &'static str,
    command: GoalCommand,
) -> Result<GoalCommand, GoalCommandParseError> {
    if arguments.trim().is_empty() {
        Ok(command)
    } else {
        Err(GoalCommandParseError::UnexpectedArguments(name))
    }
}

fn parse_budget_selection(
    flags: &str,
    allow_defaults: bool,
) -> Result<GoalBudgetSelection, GoalCommandParseError> {
    let mut token_limit = None;
    let mut cost_limit_usd = None;
    let mut unlimited = false;
    let mut words = flags.split_whitespace();

    while let Some(flag) = words.next() {
        match flag {
            "--unlimited" => {
                if unlimited {
                    return Err(GoalCommandParseError::DuplicateFlag("--unlimited"));
                }
                unlimited = true;
            }
            "--tokens" => {
                if token_limit.is_some() {
                    return Err(GoalCommandParseError::DuplicateFlag("--tokens"));
                }
                let value = words
                    .next()
                    .ok_or(GoalCommandParseError::MissingFlagValue("--tokens"))?;
                let value = value
                    .parse::<u64>()
                    .map_err(|_| GoalCommandParseError::InvalidTokenLimit(value.to_string()))?;
                if !is_valid_finite_goal_token_limit(value) {
                    return Err(GoalCommandParseError::InvalidTokenLimit(value.to_string()));
                }
                token_limit = Some(value);
            }
            "--cost-usd" => {
                if cost_limit_usd.is_some() {
                    return Err(GoalCommandParseError::DuplicateFlag("--cost-usd"));
                }
                let value = words
                    .next()
                    .ok_or(GoalCommandParseError::MissingFlagValue("--cost-usd"))?;
                let value = value
                    .parse::<f64>()
                    .map_err(|_| GoalCommandParseError::InvalidCostLimit(value.to_string()))?;
                if !is_valid_finite_goal_cost_limit(value) {
                    return Err(GoalCommandParseError::InvalidCostLimit(value.to_string()));
                }
                cost_limit_usd = Some(value);
            }
            _ => return Err(GoalCommandParseError::UnknownFlag(flag.to_string())),
        }
    }

    if unlimited && (token_limit.is_some() || cost_limit_usd.is_some()) {
        return Err(GoalCommandParseError::ConflictingBudgetFlags);
    }
    if unlimited {
        return Ok(GoalBudgetSelection::Unlimited);
    }
    if token_limit.is_some() || cost_limit_usd.is_some() {
        return Ok(GoalBudgetSelection::Limits(GoalBudgetLimits {
            token_limit,
            cost_limit_usd,
        }));
    }
    if allow_defaults {
        Ok(GoalBudgetSelection::Defaults)
    } else {
        Err(GoalCommandParseError::UnexpectedArguments("budget set"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_the_complete_v1_grammar() {
        assert_eq!(
            parse_goal_command("/goal start finish the task"),
            Ok(GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            })
        );
        assert_eq!(
            parse_goal_command("/goal start --tokens 12 --cost-usd 1.5 complete"),
            Ok(GoalCommand::Start {
                budget: GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: Some(12),
                    cost_limit_usd: Some(1.5),
                }),
                objective: "complete".into(),
            })
        );
        assert_eq!(
            parse_goal_command("/goal start --unlimited complete"),
            Ok(GoalCommand::Start {
                budget: GoalBudgetSelection::Unlimited,
                objective: "complete".into(),
            })
        );
        assert_eq!(
            parse_goal_command("/goal budget set --tokens 12"),
            Ok(GoalCommand::SetBudget(GoalBudgetSelection::Limits(
                GoalBudgetLimits {
                    token_limit: Some(12),
                    cost_limit_usd: None,
                }
            )))
        );
        assert_eq!(
            parse_goal_command("/goal budget set --unlimited"),
            Ok(GoalCommand::SetBudget(GoalBudgetSelection::Unlimited))
        );
        for command in [
            "/goal status",
            "/goal budget",
            "/goal pause",
            "/goal pause now",
            "/goal resume",
            "/goal cancel",
            "/goal help",
        ] {
            assert!(parse_goal_command(command).is_ok(), "{command}");
        }
    }

    #[test]
    fn start_accepts_an_objective_without_a_delimiter() {
        assert_eq!(
            parse_goal_command("/goal start finish the task"),
            Ok(GoalCommand::Start {
                budget: GoalBudgetSelection::Defaults,
                objective: "finish the task".into(),
            })
        );
        assert_eq!(
            parse_goal_command("/goal start --tokens 12 --cost-usd 1.5 complete"),
            Ok(GoalCommand::Start {
                budget: GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: Some(12),
                    cost_limit_usd: Some(1.5),
                }),
                objective: "complete".into(),
            })
        );
        assert_eq!(
            parse_goal_command("/goal start --unlimited complete"),
            Ok(GoalCommand::Start {
                budget: GoalBudgetSelection::Unlimited,
                objective: "complete".into(),
            })
        );
    }

    #[test]
    fn start_rejects_the_retired_objective_delimiter() {
        assert!(parse_goal_command("/goal start -- finish the task").is_err());
        assert!(parse_goal_command("/goal start --unlimited -- complete").is_err());
    }

    #[test]
    fn rejects_retired_ambiguous_and_non_positive_forms() {
        for command in [
            "/goal start -- objective",
            "/goal objective amend",
            "/goal start --tokens=1 objective",
            "/goal start --tokens 0 objective",
            "/goal start --cost-usd 0 objective",
            "/goal start --unlimited --tokens 1 objective",
            "/goal budget set",
            "/goal budget set --cost-usd NaN",
            "/goal budget set --tokens 1 extra",
        ] {
            assert!(parse_goal_command(command).is_err(), "{command}");
        }
    }

    #[test]
    fn resume_preserves_a_multiline_response_verbatim() {
        assert_eq!(
            parse_goal_command("/goal resume first line\n\n- second line"),
            Ok(GoalCommand::Resume {
                response: Some("first line\n\n- second line".into()),
            })
        );
        assert_eq!(
            parse_goal_command("/goal resume"),
            Ok(GoalCommand::Resume { response: None })
        );
    }

    #[test]
    fn pause_only_accepts_the_immediate_now_modifier() {
        assert_eq!(
            parse_goal_command("/goal pause now"),
            Ok(GoalCommand::Pause { immediate: true })
        );
        assert!(parse_goal_command("/goal pause later").is_err());
    }

    #[test]
    fn preserves_the_objective_after_budget_flags() {
        assert_eq!(
            parse_goal_command("/goal start --tokens 1  \u{2003}complete -- exactly\u{2002}"),
            Ok(GoalCommand::Start {
                budget: GoalBudgetSelection::Limits(GoalBudgetLimits {
                    token_limit: Some(1),
                    cost_limit_usd: None,
                }),
                objective: " \u{2003}complete -- exactly\u{2002}".into(),
            })
        );
    }

    #[test]
    fn normalizes_unknown_subcommands_once_for_the_error_payload() {
        assert_eq!(
            parse_goal_command("/goal MiXeD"),
            Err(GoalCommandParseError::UnknownSubcommand("mixed".into()))
        );
    }

    #[test]
    fn bounds_objective_validation_before_scanning_unbounded_whitespace() {
        let overlong_whitespace = " ".repeat(MAX_GOAL_OBJECTIVE_CHARS + 2);
        assert_eq!(
            parse_goal_command(&format!("/goal start {overlong_whitespace}")),
            Err(GoalCommandParseError::TextTooLong {
                max: MAX_GOAL_OBJECTIVE_CHARS,
            })
        );
    }

    #[test]
    fn objective_length_limit_counts_unicode_scalar_values() {
        let within_limit = "🦀".repeat(MAX_GOAL_OBJECTIVE_CHARS);
        let over_limit = "🦀".repeat(MAX_GOAL_OBJECTIVE_CHARS + 1);
        assert!(parse_goal_command(&format!("/goal start {within_limit}")).is_ok());
        assert_eq!(
            parse_goal_command(&format!("/goal start {over_limit}")),
            Err(GoalCommandParseError::TextTooLong {
                max: MAX_GOAL_OBJECTIVE_CHARS,
            })
        );
    }
}
