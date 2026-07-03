//! Freeform LLM-guided onboarding: instead of walking the spec node by node,
//! the whole spec is injected into the guide's briefing and the guide holds an
//! open conversation, inferring as many field values as it can from operator
//! intent. When the guide believes the section is complete it submits a full
//! value map; the deterministic layer validates every value against the spec,
//! previews the final shape to the operator (secrets never shown or collected
//! yet), and only writes after explicit operator approval. Secrets are
//! collected off-LLM after approval, exactly like the per-field walk.

use std::collections::BTreeMap;

use zeroclaw_runtime::flow::{
    Outcome, PlannedAction, PrefilledError, PrefilledPlan, Prompt, Spec, TransportResult,
};
use zeroclaw_runtime::response_type::{ResponseType, ResponseValue};

use crate::agent_responder::{AgentTurn, OperatorIo};
use crate::llm_transport::{SecretReader, parse_raw};

/// Marker the guide uses to hand a complete field→value map back to the
/// deterministic layer. Everything before the marker line is conversation for
/// the operator; the JSON object after it is machine input and never shown.
const SUBMIT_MARKER: &str = "SUBMIT:";

/// Upper bound on guide/operator exchanges for the whole session. Generous —
/// a session is one conversation, not one field — but bounded so a
/// non-converging guide fails fast instead of looping forever.
const MAX_SESSION_TURNS: usize = 48;

/// Upper bound on consecutive rejected submissions. Each rejection carries a
/// structured error back to the guide; a guide that cannot produce a valid
/// map within this budget aborts the session.
const MAX_INVALID_SUBMISSIONS: usize = 6;

#[derive(Debug, thiserror::Error)]
pub enum FreeformError {
    #[error("guide session did not complete within {MAX_SESSION_TURNS} turns")]
    TurnBudgetExhausted,
    #[error("guide produced {MAX_INVALID_SUBMISSIONS} invalid submissions in a row")]
    SubmissionBudgetExhausted,
    #[error(transparent)]
    Transport(#[from] zeroclaw_runtime::flow::TransportError),
    #[error(transparent)]
    Walk(#[from] zeroclaw_runtime::flow::WalkError),
}

/// Briefing prefix for the freeform session. The spec brief (every field with
/// its type contract) is appended at build time, so the guide knows the whole
/// shape up front and can infer values from natural conversation instead of
/// interrogating the operator field by field.
const FREEFORM_BRIEFING: &str = "You are guiding a person through ZeroClaw setup in one natural conversation. \
They do not understand configuration fields. Below is the complete list of fields you must fill. \
Do NOT walk them through fields one at a time and do NOT ask about technical tuning values \
(timeouts, intervals, stream modes) unless they bring them up; infer sensible values from what \
they tell you about their situation, and prefer defaults for anything they will not care about. \
Ask only the questions a human setup assistant would genuinely need answered. \
You can ONLY set the fields listed below; if the person asks you to change anything else \
(other agents, autonomy, unrelated settings), say plainly that it is outside this setup. \
If the person asks for safety, simplicity, or says they do not understand computers, prefer \
restrictive values (for example exclude the `shell` tool) and say so in one short sentence. \
If the person does not care about something, defers to you, or gives 'whatever' answers, choose \
safe sensible defaults yourself and note them in the preview; only insist on an answer for \
values you truly cannot invent (ids, addresses, names). \
Fields marked (secret) are NEVER collected by you: do not ask for the value; it is gathered \
securely after the person approves. Fields marked (optional) may be omitted from your submission. \
When you are confident you know every required value, reply with a line containing only \
`SUBMIT:` followed by a JSON object mapping each field id to its value as a string \
(yes/no fields: \"yes\" or \"no\"; choice fields: exactly one listed token). \
Anything you say without the SUBMIT line is shown to the person as conversation. \
If the person pastes what looks like a secret or token into chat, tell them you cannot \
accept it there, that it will be collected privately after they approve, and that they \
should reset it if it is real. Never place it in your submission. \
If your submission is rejected you will receive the machine error; fix it and resubmit \
without bothering the person unless you need information only they have.\n";

/// Render the spec into the guide-facing field brief: one line per node with
/// its id, prompt text, and machine contract. The brief is derived from the
/// live spec so it can never drift from what the validator accepts.
#[must_use]
pub fn spec_brief(spec: &Spec) -> String {
    let mut brief = String::from("\nFields:\n");
    for (id, node) in &spec.nodes {
        let prompt = &node.prompt;
        let text = crate::i18n::resolve_prompt_text(prompt);
        brief.push_str(&format!("- `{}`: {}", id.0, text.trim()));
        brief.push_str(&contract_suffix(prompt));
        brief.push('\n');
    }
    brief
}

fn contract_suffix(prompt: &Prompt) -> String {
    let mut suffix = match &prompt.response_type {
        ResponseType::Secret => " (secret: never collect this; gathered after approval)".into(),
        ResponseType::YesNo => " [yes|no]".to_string(),
        ResponseType::Number => " [number]".to_string(),
        ResponseType::FreeformText => String::new(),
        ResponseType::Choice { options } => {
            let tokens: Vec<String> = options
                .iter()
                .map(|option| format!("`{}` = {}", option.value, option.label))
                .collect();
            format!(" [one of: {}]", tokens.join("; "))
        }
    };
    if prompt.optional {
        suffix.push_str(" (optional)");
    }
    suffix
}

/// Extract the submission JSON from a guide reply carrying the marker.
/// Conversation may precede the marker; the JSON may follow on the same line
/// or on subsequent lines (optionally fenced).
fn extract_submission(reply: &str) -> Option<String> {
    let index = reply.find(SUBMIT_MARKER)?;
    let after = &reply[index + SUBMIT_MARKER.len()..];
    let cleaned = after
        .replace("```json", "")
        .replace("```", "")
        .trim()
        .to_string();
    if cleaned.is_empty() {
        None
    } else {
        Some(cleaned)
    }
}

/// Result of vetting one submission against the spec: either a resolvable
/// plan or the structured feedback the guide needs to repair its map.
enum Vetted {
    Plan(
        PrefilledPlan,
        BTreeMap<zeroclaw_runtime::flow::NodeId, ResponseValue>,
    ),
    Rejected(String),
}

fn vet_submission(spec: &Spec, submission_json: &str) -> Vetted {
    // Take the first JSON value and ignore anything after it: guides
    // routinely append commentary after the object, and that prose must not
    // invalidate an otherwise correct submission.
    let parsed: serde_json::Value = match serde_json::Deserializer::from_str(submission_json)
        .into_iter::<serde_json::Value>()
        .next()
    {
        Some(Ok(value)) => value,
        Some(Err(error)) => {
            return Vetted::Rejected(format!(
                "Submission rejected: the text after `{SUBMIT_MARKER}` is not valid JSON ({error}). \
                 Resubmit a single JSON object mapping field ids to string values."
            ));
        }
        None => {
            return Vetted::Rejected(format!(
                "Submission rejected: nothing parseable after `{SUBMIT_MARKER}`. \
                 Resubmit a single JSON object mapping field ids to string values."
            ));
        }
    };
    let Some(object) = parsed.as_object() else {
        return Vetted::Rejected(format!(
            "Submission rejected: expected a JSON object after `{SUBMIT_MARKER}`."
        ));
    };

    let mut values = BTreeMap::new();
    let mut problems = Vec::new();
    for (key, raw_value) in object {
        let node_id = zeroclaw_runtime::flow::NodeId::new(key.clone());
        let Some(node) = spec.nodes.get(&node_id) else {
            problems.push(format!("- `{key}` is not a field in this section"));
            continue;
        };
        if node.prompt.routes_secret() {
            problems.push(format!(
                "- `{key}` is a secret and must NOT be in the submission; it is collected separately"
            ));
            continue;
        }
        if raw_value.is_null() {
            continue;
        }
        let raw = match flatten_value(raw_value) {
            Some(text) => text,
            None => {
                problems.push(format!(
                    "- `{key}`: value must be a string (or a flat array of strings), not nested JSON"
                ));
                continue;
            }
        };
        if raw.chars().any(char::is_control) {
            // A human at the CLI walk can never type a control character;
            // the guide must not be able to write one either, and embedded
            // newlines could forge extra lines in the operator preview.
            problems.push(format!(
                "- `{key}`: value must not contain control characters"
            ));
            continue;
        }
        if is_decline(&raw) {
            if node.prompt.optional {
                continue;
            }
            problems.push(format!(
                "- `{key}` is required; `{raw}` is not a value. Ask the person if you cannot infer it"
            ));
            continue;
        }
        match parse_raw(&node.prompt, &raw) {
            Some(value) => {
                values.insert(node_id, value);
            }
            None => {
                problems.push(format!(
                    "- `{key}`: value `{raw}` does not satisfy{}",
                    contract_suffix(&node.prompt)
                ));
            }
        }
    }
    if !problems.is_empty() {
        return Vetted::Rejected(format!(
            "Submission rejected:\n{}\nFix these and resubmit the full map.",
            problems.join("\n")
        ));
    }

    match spec.resolve_prefilled(&values) {
        Ok(plan) => Vetted::Plan(plan, values),
        Err(PrefilledError::MissingValue(node)) => Vetted::Rejected(format!(
            "Submission rejected: required field `{}` has no value. \
             Ask the person if you cannot infer it, then resubmit.",
            node.0
        )),
        Err(PrefilledError::InvalidValue(node)) => Vetted::Rejected(format!(
            "Submission rejected: the value for `{}` failed validation. Resubmit.",
            node.0
        )),
        Err(error) => Vetted::Rejected(format!("Submission rejected: {error}. Resubmit.")),
    }
}

fn is_decline(raw: &str) -> bool {
    let trimmed = raw.trim();
    trimmed.is_empty()
        || trimmed.eq_ignore_ascii_case("none")
        || trimmed.eq_ignore_ascii_case("skip")
}

/// Flatten a submitted JSON value into the raw string the prompt parser
/// expects. Guides sometimes send native JSON types (numbers, booleans,
/// arrays of ids) instead of strings; scalars stringify losslessly and flat
/// scalar arrays join into the comma-separated shape list fields accept.
/// Nested objects/arrays have no defensible flattening and are rejected.
fn flatten_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(flag) => Some(if *flag { "yes" } else { "no" }.to_string()),
        serde_json::Value::Array(items) => {
            let mut parts = Vec::with_capacity(items.len());
            for item in items {
                match item {
                    serde_json::Value::String(text) => parts.push(text.clone()),
                    serde_json::Value::Number(number) => parts.push(number.to_string()),
                    _ => return None,
                }
            }
            Some(parts.join(", "))
        }
        serde_json::Value::Null | serde_json::Value::Object(_) => None,
    }
}

/// Render the operator-facing preview of a resolved plan. Secret values are
/// not in the plan (they have not been collected), so the preview marks them
/// as gathered-after-approval; nothing sensitive can appear here.
#[must_use]
pub fn render_preview(spec: &Spec, plan: &PrefilledPlan) -> String {
    let mut preview = String::from("Here is everything about to be configured:\n");
    for step in &plan.steps {
        let label = spec
            .nodes
            .get(&step.node)
            .map(|node| node.prop.clone())
            .filter(|prop| !prop.is_empty())
            .unwrap_or_else(|| step.node.0.clone());
        match &step.action {
            PlannedAction::Write(value) => {
                preview.push_str(&format!("  {label} = {}\n", display_value(value)));
            }
            PlannedAction::Skip => {
                preview.push_str(&format!("  {label} (left unset)\n"));
            }
            PlannedAction::CollectSecret => {
                preview.push_str(&format!(
                    "  {label} = <secret: asked privately after you approve; \
                     answer 'later' to leave it unset>\n"
                ));
            }
        }
    }
    preview.push_str("Apply this? (yes to apply, or tell me what to change)");
    preview
}

fn display_value(value: &ResponseValue) -> String {
    match value {
        ResponseValue::Secret(_) => "<secret>".to_string(),
        ResponseValue::FreeformText(text) => text.clone(),
        ResponseValue::Number(number) => number.clone(),
        ResponseValue::Choice(choice) => choice.clone(),
        ResponseValue::YesNo(true) => "yes".to_string(),
        ResponseValue::YesNo(false) => "no".to_string(),
    }
}

fn is_approval(reply: &str) -> bool {
    if let Some(flag) = crate::llm_transport::parse_yes_no(reply.trim()) {
        return flag;
    }
    // Real operators approve in sentences ("all good, apply it", "yes please
    // go ahead"). Approve only when an affirmative word is present AND no
    // hedge/change word is: "yes but disable shell" must NOT apply.
    let mut affirmative = false;
    for word in reply
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
    {
        if APPROVAL_BLOCKERS.contains(&word.as_str()) {
            return false;
        }
        if APPROVAL_WORDS.contains(&word.as_str()) {
            affirmative = true;
        }
    }
    affirmative
}

const APPROVAL_WORDS: &[&str] = &[
    "yes",
    "yep",
    "yeah",
    "yup",
    "ok",
    "okay",
    "sure",
    "apply",
    "approve",
    "approved",
    "confirm",
    "confirmed",
    "good",
    "perfect",
    "lgtm",
    "proceed",
    "go",
];

const APPROVAL_BLOCKERS: &[&str] = &[
    "no", "not", "nope", "dont", "don", "but", "except", "change", "changes", "instead", "wait",
    "hold", "stop", "cancel", "remove", "add", "actually", "wrong", "fix", "edit", "adjust",
    "swap", "rather", "unless", "however",
];

/// A hard operator abort. Checked deterministically on every operator reply
/// so a person can always leave the session without convincing the guide.
fn is_cancel(reply: &str) -> bool {
    let word = reply
        .trim()
        .trim_end_matches(['.', '!'])
        .to_ascii_lowercase();
    matches!(
        word.as_str(),
        "cancel" | "quit" | "abort" | "exit" | "/quit"
    )
}

/// Collect every planned secret off-LLM through the masked secret channel,
/// re-prompting on empty input like the per-field walk does.
async fn collect_secrets<S: SecretReader>(
    spec: &Spec,
    plan: &PrefilledPlan,
    secrets: &mut S,
) -> TransportResult<BTreeMap<zeroclaw_runtime::flow::NodeId, ResponseValue>> {
    let mut collected = BTreeMap::new();
    for step in &plan.steps {
        if !matches!(step.action, PlannedAction::CollectSecret) {
            continue;
        }
        let Some(node) = spec.nodes.get(&step.node) else {
            continue;
        };
        let prompt_text = crate::i18n::resolve_prompt_text(&node.prompt);
        loop {
            let raw = secrets.read_secret(&prompt_text).await?;
            if crate::cli_transport::is_secret_deferral(&raw) {
                // The person is not ready to hand this over; the field stays
                // unset and they can rerun this section when they have it.
                break;
            }
            if !raw.is_empty() {
                collected.insert(
                    step.node.clone(),
                    ResponseValue::Secret(zeroclaw_runtime::response_type::SecretValue::new(raw)),
                );
                break;
            }
        }
    }
    Ok(collected)
}

/// Run the freeform session to completion: conversation, submission vetting,
/// operator preview/approval, off-LLM secret collection, and the prefilled
/// apply. The guide never sees a secret and never writes config; every write
/// goes through the same typed plan the per-field walk uses.
pub async fn run_freeform<T, O, S>(
    spec: &Spec,
    config: &mut zeroclaw_config::schema::Config,
    turn: &mut T,
    io: &mut O,
    secrets: &mut S,
) -> Result<Outcome, FreeformError>
where
    T: AgentTurn,
    O: OperatorIo,
    S: SecretReader,
{
    let mut message = format!("{FREEFORM_BRIEFING}{}", spec_brief(spec));
    let mut invalid_submissions = 0usize;

    for session_turn in 0..MAX_SESSION_TURNS {
        let reply = turn.run_single(&message).await?;

        let Some(submission) = extract_submission(&reply) else {
            let spoken = reply.trim();
            if spoken.is_empty() {
                // A blank guide turn shown to the operator reads as a hang.
                // Bounce it back to the guide instead of the person.
                message = format!(
                    "Your reply was empty. Continue the conversation or submit with `{SUBMIT_MARKER}`."
                );
                continue;
            }
            io.say(spoken).await?;
            let operator = io.hear().await?;
            if is_cancel(&operator) {
                return Ok(Outcome::Cancelled);
            }
            message = if session_turn + 4 >= MAX_SESSION_TURNS {
                format!(
                    "{operator}\n\nThe session is nearly out of turns. Resolve now: submit with \
                     `{SUBMIT_MARKER}` using sensible defaults for anything still unknown, or \
                     state plainly what single piece of information you still need."
                )
            } else {
                operator
            };
            continue;
        };

        match vet_submission(spec, &submission) {
            Vetted::Rejected(feedback) => {
                invalid_submissions += 1;
                if invalid_submissions >= MAX_INVALID_SUBMISSIONS {
                    return Err(FreeformError::SubmissionBudgetExhausted);
                }
                message = feedback;
            }
            Vetted::Plan(plan, _values) => {
                invalid_submissions = 0;
                io.say(&render_preview(spec, &plan)).await?;
                let verdict = io.hear().await?;
                if is_cancel(&verdict) {
                    return Ok(Outcome::Cancelled);
                }
                if is_approval(&verdict) {
                    let collected = collect_secrets(spec, &plan, secrets).await?;
                    let outcome = spec.apply_prefilled(&plan, &collected, config)?;
                    return Ok(outcome);
                }
                message = format!(
                    "The person reviewed the preview and did not approve. They said: \
                     \"{verdict}\". Adjust the values accordingly and resubmit, or ask \
                     them a clarifying question."
                );
            }
        }
    }
    Err(FreeformError::TurnBudgetExhausted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use zeroclaw_config::schema::{Config, MatrixConfig};
    use zeroclaw_runtime::flow::TransportError;

    struct ScriptedTurn {
        replies: VecDeque<String>,
        seen: Vec<String>,
    }

    impl ScriptedTurn {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: replies.into_iter().map(String::from).collect(),
                seen: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl AgentTurn for ScriptedTurn {
        async fn run_single(&mut self, message: &str) -> TransportResult<String> {
            self.seen.push(message.to_string());
            self.replies.pop_front().ok_or(TransportError::Closed)
        }
    }

    struct ScriptedOperator {
        replies: VecDeque<String>,
        heard: Vec<String>,
    }

    impl ScriptedOperator {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: replies.into_iter().map(String::from).collect(),
                heard: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl OperatorIo for ScriptedOperator {
        async fn say(&mut self, text: &str) -> TransportResult<()> {
            self.heard.push(text.to_string());
            Ok(())
        }

        async fn hear(&mut self) -> TransportResult<String> {
            self.replies.pop_front().ok_or(TransportError::Closed)
        }
    }

    struct ScriptedSecrets {
        replies: VecDeque<String>,
        prompts: Vec<String>,
    }

    impl ScriptedSecrets {
        fn new(replies: Vec<&str>) -> Self {
            Self {
                replies: replies.into_iter().map(String::from).collect(),
                prompts: Vec::new(),
            }
        }
    }

    #[async_trait]
    impl SecretReader for ScriptedSecrets {
        async fn read_secret(&mut self, prompt_text: &str) -> TransportResult<String> {
            self.prompts.push(prompt_text.to_string());
            self.replies.pop_front().ok_or(TransportError::Closed)
        }
    }

    const SECTION: &str = "channels.matrix.home";

    fn matrix_config() -> Config {
        let mut config = Config::default();
        config
            .channels
            .matrix
            .insert("home".to_string(), MatrixConfig::default());
        config
    }

    fn discord_config() -> Config {
        let mut config = Config::default();
        config
            .channels
            .discord
            .insert("main".to_string(), Default::default());
        config
    }

    fn spec_for(config: &Config, section: &str, instance: &str) -> Spec {
        crate::spec_builder::build_spec(
            config.prop_fields(),
            section,
            "channel",
            instance,
            Outcome::Completed {
                configured: vec![zeroclaw_runtime::flow::ConfiguredItem {
                    layer: "channel".into(),
                    instance: instance.into(),
                }],
            },
        )
        .expect("section yields a spec")
    }

    fn matrix_spec(config: &Config) -> Spec {
        spec_for(config, SECTION, "home")
    }

    fn full_submission(spec: &Spec) -> String {
        let mut map = serde_json::Map::new();
        for (id, node) in &spec.nodes {
            if node.prompt.routes_secret() || node.prompt.optional {
                continue;
            }
            let value = match &node.prompt.response_type {
                ResponseType::YesNo => "yes".to_string(),
                ResponseType::Number => "42".to_string(),
                ResponseType::Choice { options } => options[0].value.clone(),
                _ => "https://matrix.example.com".to_string(),
            };
            map.insert(id.0.clone(), serde_json::Value::String(value));
        }
        serde_json::Value::Object(map).to_string()
    }

    #[tokio::test]
    async fn brief_marks_secrets_and_lists_choice_tokens() {
        let config = matrix_config();
        let spec = matrix_spec(&config);
        let brief = spec_brief(&spec);
        assert!(brief.contains("(secret: never collect this"));
        assert!(brief.contains("[yes|no]"));
    }

    #[tokio::test]
    async fn conversation_then_valid_submission_previews_and_applies_on_approval() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let submission = format!("All set!\nSUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec!["Hi! What server do you use?", &submission]);
        let mut io = ScriptedOperator::new(vec!["matrix.example.com please", "yes"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Completed { .. }));
        let preview = io
            .heard
            .iter()
            .find(|text| text.contains("about to be configured"))
            .expect("operator saw a preview");
        assert!(
            preview.contains("left unset"),
            "optional fields shown as unset: {preview}"
        );
        assert!(
            secrets.prompts.is_empty(),
            "matrix secrets are all optional, none collected"
        );
        let matrix = config.channels.matrix.get("home").unwrap();
        assert_eq!(matrix.homeserver, "https://matrix.example.com");
        assert!(
            matrix.access_token.is_none(),
            "optional secret stays unset without operator intent"
        );
    }

    #[tokio::test]
    async fn required_secret_masked_in_preview_and_collected_off_llm() {
        let mut config = discord_config();
        let spec = spec_for(&config, "channels.discord.main", "main");
        let submission = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![&submission]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec!["dsc-live-token"]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Completed { .. }));
        let preview = &io.heard[0];
        assert!(
            preview.contains("<secret: asked privately after you approve"),
            "required secret masked in preview: {preview}"
        );
        assert!(
            !preview.contains("dsc-live-token"),
            "no secret value in preview"
        );
        assert_eq!(
            secrets.prompts.len(),
            1,
            "discord bot token collected off-LLM exactly once"
        );
        let discord = config.channels.discord.get("main").unwrap();
        assert!(
            !discord.bot_token.is_empty(),
            "token written through the secret path"
        );
    }

    #[tokio::test]
    async fn invalid_submission_bounces_to_guide_not_operator() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let good = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![
            r#"SUBMIT: {"channels.matrix.home.enabled": "absolutely"}"#,
            &good,
        ]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec!["tok"]);

        run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        let feedback = &turn.seen[1];
        assert!(
            feedback.contains("Submission rejected"),
            "guide got the rejection: {feedback}"
        );
        assert!(
            feedback.contains("does not satisfy"),
            "rejection names the bad value: {feedback}"
        );
        assert_eq!(
            io.heard.len(),
            1,
            "operator saw only the preview, never the machine error"
        );
    }

    #[tokio::test]
    async fn submission_carrying_a_secret_is_rejected() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let secret_id = spec
            .nodes
            .iter()
            .find(|(_, node)| node.prompt.routes_secret())
            .map(|(id, _)| id.0.clone())
            .expect("matrix has a secret field");
        let bad = format!(r#"SUBMIT: {{"{secret_id}": "sk-leaked"}}"#);
        let good = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![&bad, &good]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec!["tok"]);

        run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        let feedback = &turn.seen[1];
        assert!(
            feedback.contains("must NOT be in the submission"),
            "secret-bearing submission rejected: {feedback}"
        );
        assert!(
            !feedback.contains("sk-leaked"),
            "rejection never echoes the secret value"
        );
    }

    #[tokio::test]
    async fn required_secret_deferred_leaves_field_unset() {
        let mut config = discord_config();
        let spec = spec_for(&config, "channels.discord.main", "main");
        let submission = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![&submission]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec!["later"]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(
            matches!(outcome, Outcome::Completed { .. }),
            "deferring the token must not brick the flow"
        );
        let discord = config.channels.discord.get("main").unwrap();
        assert!(
            discord.bot_token.is_empty(),
            "deferred secret stays unset, no phantom value"
        );
    }

    #[tokio::test]
    async fn operator_rejection_routes_feedback_to_guide_and_resubmits() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let submission = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![&submission, &submission]);
        let mut io = ScriptedOperator::new(vec!["no, change the server", "yes"]);
        let mut secrets = ScriptedSecrets::new(vec!["tok"]);

        run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(
            turn.seen[1].contains("did not approve"),
            "operator objection went back to the guide: {}",
            turn.seen[1]
        );
        assert_eq!(io.heard.len(), 2, "operator saw both previews");
    }

    #[tokio::test]
    async fn hedged_approval_does_not_apply() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let submission = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![&submission, &submission]);
        let mut io = ScriptedOperator::new(vec!["yes but change the server to matrix.org", "yes"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(
            turn.seen[1].contains("did not approve"),
            "hedged yes routed back to the guide, not applied: {}",
            turn.seen[1]
        );
    }

    #[tokio::test]
    async fn sentence_approval_applies() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let submission = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![&submission]);
        let mut io = ScriptedOperator::new(vec!["all good, apply it"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Completed { .. }));
    }

    #[tokio::test]
    async fn operator_cancel_ends_session_without_writes() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let submission = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![&submission]);
        let mut io = ScriptedOperator::new(vec!["cancel"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Cancelled));
        let matrix = config.channels.matrix.get("home").unwrap();
        assert!(matrix.homeserver.is_empty(), "nothing was written");
    }

    #[tokio::test]
    async fn cancel_mid_conversation_ends_session() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let mut turn = ScriptedTurn::new(vec!["What server do you folks use?"]);
        let mut io = ScriptedOperator::new(vec!["quit"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Cancelled));
    }

    #[tokio::test]
    async fn required_field_declined_with_skip_is_rejected() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let required_text_id = spec
            .nodes
            .iter()
            .find(|(_, node)| {
                !node.prompt.optional
                    && !node.prompt.routes_secret()
                    && matches!(node.prompt.response_type, ResponseType::FreeformText)
            })
            .map(|(id, _)| id.0.clone())
            .expect("matrix has a required text field");
        let bad = format!(r#"SUBMIT: {{"{required_text_id}": "skip"}}"#);
        let good = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![&bad, &good]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(
            turn.seen[1].contains("is required"),
            "skip on a required field rejected: {}",
            turn.seen[1]
        );
    }

    #[tokio::test]
    async fn native_json_types_are_flattened() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let mut map = serde_json::Map::new();
        for (id, node) in &spec.nodes {
            if node.prompt.routes_secret() || node.prompt.optional {
                continue;
            }
            let value = match &node.prompt.response_type {
                ResponseType::YesNo => serde_json::Value::Bool(true),
                ResponseType::Number => serde_json::json!(42),
                ResponseType::Choice { options } => {
                    serde_json::Value::String(options[0].value.clone())
                }
                _ => serde_json::Value::String("https://matrix.example.com".into()),
            };
            map.insert(id.0.clone(), value);
        }
        let submission = format!("SUBMIT: {}", serde_json::Value::Object(map));
        let mut turn = ScriptedTurn::new(vec![&submission]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();
        assert!(
            matches!(outcome, Outcome::Completed { .. }),
            "booleans and numbers flatten to the string contract"
        );
        assert!(config.channels.matrix.get("home").unwrap().enabled);
    }

    #[tokio::test]
    async fn trailing_prose_after_json_is_tolerated() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let submission = format!(
            "SUBMIT: {}\nLet me know if you want anything changed!",
            full_submission(&spec)
        );
        let mut turn = ScriptedTurn::new(vec![&submission]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();
        assert!(matches!(outcome, Outcome::Completed { .. }));
    }

    #[tokio::test]
    async fn control_characters_in_values_are_rejected() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let bad =
            "SUBMIT: {\"channels.matrix.home.homeserver\": \"https://x.com\\nfake_line = true\"}";
        let good = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec![bad, &good]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(
            turn.seen[1].contains("control characters"),
            "newline smuggling rejected: {}",
            turn.seen[1]
        );
    }

    #[tokio::test]
    async fn empty_guide_reply_bounces_back_without_operator_turn() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let submission = format!("SUBMIT: {}", full_submission(&spec));
        let mut turn = ScriptedTurn::new(vec!["   ", &submission]);
        let mut io = ScriptedOperator::new(vec!["yes"]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        let outcome = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap();

        assert!(matches!(outcome, Outcome::Completed { .. }));
        assert!(
            turn.seen[1].contains("Your reply was empty"),
            "blank turn bounced to guide: {}",
            turn.seen[1]
        );
        assert_eq!(io.heard.len(), 1, "operator only ever saw the preview");
    }

    #[tokio::test]
    async fn submission_budget_aborts_after_max_consecutive_rejections() {
        let mut config = matrix_config();
        let spec = matrix_spec(&config);
        let bad = r#"SUBMIT: {"nonsense.field": "x"}"#;
        let mut turn = ScriptedTurn::new(vec![bad; MAX_INVALID_SUBMISSIONS]);
        let mut io = ScriptedOperator::new(vec![]);
        let mut secrets = ScriptedSecrets::new(vec![]);

        let error = run_freeform(&spec, &mut config, &mut turn, &mut io, &mut secrets)
            .await
            .unwrap_err();
        assert!(matches!(error, FreeformError::SubmissionBudgetExhausted));
        assert!(io.heard.is_empty(), "operator never saw machine errors");
    }

    #[test]
    fn approval_parser_matrix() {
        for approve in [
            "yes",
            "y",
            "Yes please",
            "yep looks good",
            "ok apply it",
            "all good, apply it",
            "sure go ahead",
            "LGTM",
        ] {
            assert!(is_approval(approve), "should approve: {approve}");
        }
        for reject in [
            "no",
            "yes but change the port",
            "ok wait",
            "looks wrong",
            "apply it except the shell thing",
            "hmm",
            "add archiving first",
            "dont",
            "",
        ] {
            assert!(!is_approval(reject), "should NOT approve: {reject}");
        }
    }

    #[test]
    fn cancel_parser_matrix() {
        for cancel in [
            "cancel", "Cancel", "quit", "abort", "exit", "/quit", "cancel.",
        ] {
            assert!(is_cancel(cancel), "should cancel: {cancel}");
        }
        for keep in ["cancel the archive bit", "no", "quit asking me stuff"] {
            assert!(!is_cancel(keep), "should NOT cancel: {keep}");
        }
    }
}
