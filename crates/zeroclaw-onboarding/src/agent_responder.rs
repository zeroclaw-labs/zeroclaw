use async_trait::async_trait;
use zeroclaw_runtime::flow::TransportResult;
use zeroclaw_runtime::response_type::FollowOn;

use crate::llm_transport::LlmResponder;

#[async_trait]
pub trait AgentTurn: Send {
    async fn run_single(&mut self, message: &str) -> TransportResult<String>;
}

pub struct InProcessAgentTurn {
    agent: zeroclaw_runtime::agent::Agent,
}

impl InProcessAgentTurn {
    pub fn new(agent: zeroclaw_runtime::agent::Agent) -> Self {
        Self { agent }
    }
}

#[async_trait]
impl AgentTurn for InProcessAgentTurn {
    async fn run_single(&mut self, message: &str) -> TransportResult<String> {
        self.agent.run_single(message).await.map_err(|error| {
            zeroclaw_runtime::flow::TransportError::Agent {
                reason: error.to_string(),
            }
        })
    }
}

/// The operator side of the guided conversation. The guide (an LLM) talks to
/// the human through this seam: `say` shows the operator the guide's words,
/// `hear` waits for the operator's reply. The operator is assumed NOT to
/// understand the config schema; they describe intent in plain language and
/// the guide translates that into the typed field value.
#[async_trait]
pub trait OperatorIo: Send {
    async fn say(&mut self, text: &str) -> TransportResult<()>;
    async fn hear(&mut self) -> TransportResult<String>;
}

/// Terminal operator: guide speaks on stdout, operator replies on stdin.
pub struct TtyOperatorIo;

#[async_trait]
impl OperatorIo for TtyOperatorIo {
    async fn say(&mut self, text: &str) -> TransportResult<()> {
        println!("\n{text}");
        Ok(())
    }

    async fn hear(&mut self) -> TransportResult<String> {
        tokio::task::spawn_blocking(|| {
            {
                use std::io::Write;
                let mut out = std::io::stdout();
                let _ = write!(out, "> ");
                let _ = out.flush();
            }
            let mut line = String::new();
            match std::io::stdin().read_line(&mut line) {
                Ok(0) | Err(_) => Err(zeroclaw_runtime::flow::TransportError::Closed),
                Ok(_) => Ok(line.trim_end_matches(['\r', '\n']).to_string()),
            }
        })
        .await
        .map_err(|_| zeroclaw_runtime::flow::TransportError::Closed)?
    }
}

pub struct AgentResponder<T: AgentTurn, O: OperatorIo> {
    turn: T,
    io: O,
    last_follow_on: FollowOn,
    locale: String,
    briefed: bool,
}

impl<T: AgentTurn, O: OperatorIo> AgentResponder<T, O> {
    pub fn new(turn: T, io: O) -> Self {
        Self {
            turn,
            io,
            last_follow_on: FollowOn::BackToLlm,
            locale: default_locale(),
            briefed: false,
        }
    }

    #[must_use]
    pub fn with_locale(mut self, locale: impl Into<String>) -> Self {
        self.locale = locale.into();
        self
    }

    pub fn last_follow_on(&self) -> &FollowOn {
        &self.last_follow_on
    }

    fn classify(reply: &str) -> FollowOn {
        let trimmed = reply.trim();
        if trimmed.is_empty() {
            FollowOn::BackToLlm
        } else {
            FollowOn::Complete
        }
    }
}

const DEFAULT_LOCALE: &str = "en";

/// Marker the guide uses to hand the resolved value back to the walk. Anything
/// the guide says WITHOUT this marker is conversation for the operator, not an
/// answer, so the walk never mistakes an explanation for a field value.
const ANSWER_MARKER: &str = "ANSWER:";

/// Upper bound on operator/guide exchanges within a single field before the
/// guide is nudged to resolve. Keeps a wandering conversation from stalling
/// the walk forever while still giving a confused operator several turns.
const MAX_CONVERSATION_TURNS: usize = 8;

/// Briefing sent once at the start of the conversation. After this turn the
/// guide works from conversation history, so later prompts carry only the
/// field text. The guide's job is to talk a non-expert through each field:
/// explain in plain words, interpret vague or wrong replies, ask follow-ups,
/// and only emit the machine-readable value once the operator's intent is
/// clear. Secrets never route here; the walk collects them operator-side.
const GUIDE_BRIEFING: &str = "You are guiding a person through ZeroClaw setup. \
They likely do not understand configuration fields, so talk to them like a helpful human. \
I will send you one field at a time, then relay your words to the person and their reply back to you. \
For each field: explain briefly what it is in plain language, ask what they want, and interpret \
their answer even if it is vague or non-technical. Ask a follow-up question if you are unsure. \
When you are confident, reply with a line starting with `ANSWER:` followed by ONLY the value: \
`ANSWER: yes` or `ANSWER: no` for yes/no fields, `ANSWER: 42` for numbers, \
`ANSWER: <option>` (exactly one offered option) for choices, `ANSWER: <value>` for free text. \
If the field is optional and the person does not want it, reply `ANSWER: none`. \
Never invent a value the person did not agree to for a required field; ask instead. \
Anything you say without the `ANSWER:` line is shown to the person as conversation. \
Use their earlier choices to stay consistent. Here is the first field.\n\n";

pub(crate) fn default_locale() -> String {
    DEFAULT_LOCALE.to_string()
}

/// A one-line directive instructing the guide to answer in the operator's
/// chosen locale, resolved to the human label from the `locales.toml` registry.
/// Empty for the English default so existing English prompts are unchanged.
pub(crate) fn locale_directive(locale: &str) -> String {
    if locale == DEFAULT_LOCALE {
        return String::new();
    }
    let label = zeroclaw_runtime::i18n::available_locales()
        .iter()
        .find(|option| option.code == locale)
        .map(|option| option.label.clone())
        .unwrap_or_else(|| locale.to_string());
    format!("Respond in {label}.\n")
}

/// Extract the value from a guide reply carrying the `ANSWER:` marker line.
/// Conversation may precede the marker; only the marker line is the value.
fn extract_answer(reply: &str) -> Option<String> {
    reply
        .lines()
        .find_map(|line| line.trim_start().strip_prefix(ANSWER_MARKER))
        .map(|value| value.trim().to_string())
}

#[async_trait]
impl<T: AgentTurn, O: OperatorIo> LlmResponder for AgentResponder<T, O> {
    async fn respond(&mut self, prompt_text: &str) -> TransportResult<String> {
        let briefing = if self.briefed { "" } else { GUIDE_BRIEFING };
        self.briefed = true;
        let directive = locale_directive(&self.locale);
        let mut message = format!("{briefing}{directive}Next field: {prompt_text}");
        for turn in 0..MAX_CONVERSATION_TURNS {
            let reply = self.turn.run_single(&message).await?;
            if let Some(value) = extract_answer(&reply) {
                self.last_follow_on = Self::classify(&value);
                return Ok(value);
            }
            self.io.say(reply.trim()).await?;
            let operator = self.io.hear().await?;
            message = if turn + 2 == MAX_CONVERSATION_TURNS {
                format!(
                    "{operator}\n\nResolve this field now with an `{ANSWER_MARKER}` line \
                     based on everything above."
                )
            } else {
                operator
            };
        }
        Err(zeroclaw_runtime::flow::TransportError::Agent {
            reason: format!(
                "guide never resolved the field within {MAX_CONVERSATION_TURNS} conversation turns"
            ),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
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

    /// A scripted human: replies in order, records what the guide said to them.
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

    /// An operator that must never be spoken to: for paths where the guide
    /// resolves immediately and no conversation should occur.
    struct SilentOperator;

    #[async_trait]
    impl OperatorIo for SilentOperator {
        async fn say(&mut self, _text: &str) -> TransportResult<()> {
            panic!("the guide must not open a conversation on this path");
        }

        async fn hear(&mut self) -> TransportResult<String> {
            panic!("the operator must not be read on this path");
        }
    }

    #[tokio::test]
    async fn an_immediate_answer_marker_resolves_without_conversation() {
        let mut responder =
            AgentResponder::new(ScriptedTurn::new(vec!["ANSWER: yes"]), SilentOperator);
        let reply = responder.respond("Enable telemetry?").await.unwrap();
        assert_eq!(reply, "yes");
    }

    #[tokio::test]
    async fn guide_talk_is_relayed_to_the_operator_and_their_reply_reaches_the_guide() {
        let mut responder = AgentResponder::new(
            ScriptedTurn::new(vec![
                "This controls whether the bot reacts to messages. Want that?",
                "ANSWER: yes",
            ]),
            ScriptedOperator::new(vec!["uh sure, sounds good"]),
        );
        let reply = responder.respond("ack_reactions field").await.unwrap();
        assert_eq!(reply, "yes");
        assert_eq!(
            responder.io.heard,
            vec!["This controls whether the bot reacts to messages. Want that?"],
            "the guide's question must be shown to the operator verbatim"
        );
        assert_eq!(
            responder.turn.seen[1], "uh sure, sounds good",
            "the operator's plain-language reply must reach the guide unmodified"
        );
    }

    #[tokio::test]
    async fn conversation_may_precede_the_marker_in_the_same_reply() {
        let mut responder = AgentResponder::new(
            ScriptedTurn::new(vec!["Great, that settles it.\nANSWER: multi_message"]),
            SilentOperator,
        );
        let reply = responder.respond("stream mode?").await.unwrap();
        assert_eq!(
            reply, "multi_message",
            "only the marker line is the value; leading talk is ignored"
        );
    }

    #[tokio::test]
    async fn a_never_resolving_guide_fails_bounded_not_forever() {
        let chatter: Vec<&str> = std::iter::repeat_n("So, what do you think?", 12).collect();
        let answers: Vec<&str> = std::iter::repeat_n("i dunno", 12).collect();
        let mut responder =
            AgentResponder::new(ScriptedTurn::new(chatter), ScriptedOperator::new(answers));
        let error = responder.respond("Enable telemetry?").await.unwrap_err();
        assert!(
            matches!(error, TransportError::Agent { .. }),
            "an unresolvable conversation must surface a bounded Agent error, got {error:?}"
        );
    }

    #[tokio::test]
    async fn the_final_turn_carries_a_resolve_nudge() {
        let chatter: Vec<&str> =
            std::iter::repeat_n("hmm tell me more", MAX_CONVERSATION_TURNS).collect();
        let answers: Vec<&str> = std::iter::repeat_n("stuff", MAX_CONVERSATION_TURNS).collect();
        let mut responder =
            AgentResponder::new(ScriptedTurn::new(chatter), ScriptedOperator::new(answers));
        let _ = responder.respond("Enable telemetry?").await;
        let last = responder.turn.seen.last().expect("turns were taken");
        assert!(
            last.contains("Resolve this field now"),
            "the guide must be nudged to resolve before the bound trips, got: {last}"
        );
    }

    #[tokio::test]
    async fn responder_prepends_locale_directive_for_a_non_default_locale() {
        let mut responder =
            AgentResponder::new(ScriptedTurn::new(vec!["ANSWER: oui"]), SilentOperator)
                .with_locale("fr");
        responder.respond("Enable telemetry?").await.unwrap();
        let label = zeroclaw_runtime::i18n::available_locales()
            .iter()
            .find(|o| o.code == "fr")
            .map(|o| o.label.clone())
            .expect("fr is registered");
        let seen = responder.turn.seen.last().expect("a prompt was sent");
        assert!(
            seen.contains(&label) && seen.contains("Enable telemetry?"),
            "the guide must be told to answer in the chosen locale, got: {seen}"
        );
    }

    #[tokio::test]
    async fn responder_briefs_once_then_sends_only_the_field() {
        let mut responder = AgentResponder::new(
            ScriptedTurn::new(vec!["ANSWER: yes", "ANSWER: no"]),
            SilentOperator,
        );
        responder.respond("First field?").await.unwrap();
        responder.respond("Second field?").await.unwrap();
        let seen = &responder.turn.seen;
        assert!(
            seen[0].contains("ZeroClaw setup") && seen[0].ends_with("First field?"),
            "the first turn carries the briefing, got: {}",
            seen[0]
        );
        assert_eq!(
            seen[1], "Next field: Second field?",
            "later turns carry only the field; continuity comes from conversation history"
        );
    }

    #[tokio::test]
    async fn a_non_empty_answer_marks_the_turn_complete() {
        let mut responder = AgentResponder::new(
            ScriptedTurn::new(vec!["ANSWER: multi_message"]),
            SilentOperator,
        );
        responder.respond("stream mode?").await.unwrap();
        assert_eq!(responder.last_follow_on(), &FollowOn::Complete);
    }

    #[tokio::test]
    async fn an_empty_answer_routes_back_to_the_llm() {
        let mut responder =
            AgentResponder::new(ScriptedTurn::new(vec!["ANSWER:   "]), SilentOperator);
        responder.respond("stream mode?").await.unwrap();
        assert_eq!(responder.last_follow_on(), &FollowOn::BackToLlm);
    }
}
