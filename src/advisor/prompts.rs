//! Prompt templates for the three advisor checkpoints.
//!
//! The advisor always starts with no conversation history — the executor
//! packages every relevant byte into the prompt. These templates:
//!
//! 1. Lock the response into JSON so parsing is deterministic (see
//!    `types::PlanOutput::parse` / `ReviewOutput::parse`).
//! 2. Mirror the Advisor Strategy plugin's context structure
//!    (task → background → recent-verbatim → question) because it
//!    matches how modern LLMs best handle out-of-session context.
//! 3. Keep the system prompt short — the real payload lives in the
//!    user turn so token budget is predictable.

use super::types::AdvisorRequest;

const PLAN_SYSTEM_PROMPT: &str = "\
You are the strategic Advisor in a two-model setup: an on-device SLM \
(Gemma 4) is the Executor that does the actual work and calls tools; \
you are consulted at critical decision points only. You never execute \
— you plan, review, and advise.

Produce a strategic, end-to-end plan for the task the Executor is \
about to start. The Executor has zero access to this conversation \
after you reply, so be self-contained and specific.

Respond **only** with a single JSON object matching this schema:

```
{
  \"end_state\": \"one-sentence definition of 'done'\",
  \"critical_path\": [\"step 1\", \"step 2\", ...],
  \"risks\": [\"risk — mitigation\", ...],
  \"first_move\": \"the single best action to start with\",
  \"suggested_tools\": [\"tool_name_1\", \"tool_name_2\", ...]
}
```

Rules:
- `critical_path` must be ordered and actionable — each step is a verb \
  the Executor can perform (read file, write tests, invoke API).
- `risks` may be empty if the task is genuinely low-risk. Do not invent \
  risks for the sake of completeness.
- `first_move` names a concrete artifact or command, not a category.
- `suggested_tools` names specific tools the Executor should prefer. \
  When the task needs web information, ALWAYS prefer `smart_search` \
  (cascade: free web → Perplexity AI → reformulate retry) over the \
  raw `web_search` or `perplexity_search` tools — smart_search handles \
  tier escalation and retry automatically. Include `file_read` / \
  `shell` / `file_edit` etc. when coding; `browser` for interactive \
  navigation; leave empty only when the task genuinely requires no tools.
- Do not wrap the JSON in prose, commentary, or code fences. The \
  response starts with `{` and ends with `}`.";

const REVIEW_SYSTEM_PROMPT: &str = "\
You are the strategic Advisor reviewing work the Executor (Gemma 4 \
SLM) has already produced. The user has not yet seen the result — \
this review gates the final response.

Respond **only** with a single JSON object matching this schema:

```
{
  \"verdict\": \"pass\" | \"revision_needed\" | \"block\",
  \"correctness_issues\": [\"...\"],
  \"architecture_concerns\": [\"...\"],
  \"security_flags\": [\"...\"],
  \"silent_failures\": [\"...\"],
  \"summary\": \"under 60 words, ready to relay to the user if the \
verdict is pass\"
}
```

Verdict rules:
- `pass` — ship as-is. The result is correct and complete; no revision \
  would add meaningful value.
- `revision_needed` — correctable issues the Executor can fix in one \
  more pass (off-by-one, missing edge case, clunky wording).
- `block` — hard blocker. Security flaw, wrong answer, outside scope. \
  The Executor must NOT return this result to the user even after a \
  quick revision.

Review priorities, in order: correctness → architecture → security → \
silent failures. If the implementation is sound, confirm it concisely \
in `summary` and leave the issue arrays empty. Do not invent problems.

Do not wrap the JSON in prose, commentary, or code fences.";

const ADVISE_SYSTEM_PROMPT: &str = "\
You are the strategic Advisor. The Executor (Gemma 4 SLM) is stuck or \
about to pivot direction and has asked for guidance. Be concrete and \
actionable.

Respond in under 120 words using numbered steps, not explanations. \
Lead with the recommendation, then the reasoning in one sentence. \
Skip preamble and disclaimers.

If the Executor surfaces evidence that contradicts a prior advice \
(test result, file content, error message), acknowledge the conflict \
and explain which constraint breaks the tie. Never defer with \
'investigate further' — name the next action.";

/// Build the (system, user) prompt pair for a PLAN checkpoint call.
#[must_use]
pub fn build_plan_prompt(request: &AdvisorRequest<'_>) -> (String, String) {
    let user = format!(
        "## Task\n{task}\n\n\
         ## Background\n{background}\n\n\
         ## Recent Context (verbatim)\n{recent}\n\n\
         ## Planning Request\n{question}\n\n\
         ## Task Kind\n{kind}",
        task = request.task_summary,
        background = if request.background.is_empty() {
            "(no prior context — this is a fresh task)"
        } else {
            request.background
        },
        recent = if request.recent_output.is_empty() {
            "(no tool output yet — the executor has not started)"
        } else {
            request.recent_output
        },
        question = request.question,
        kind = request.kind.label(),
    );
    (PLAN_SYSTEM_PROMPT.to_string(), user)
}

/// Build the (system, user) prompt pair for a REVIEW checkpoint call.
#[must_use]
pub fn build_review_prompt(request: &AdvisorRequest<'_>) -> (String, String) {
    let user = format!(
        "## Task\n{task}\n\n\
         ## What Was Done\n{background}\n\n\
         ## Executor Output (verbatim)\n{recent}\n\n\
         ## Review Target\n{question}\n\n\
         ## Task Kind\n{kind}",
        task = request.task_summary,
        background = if request.background.is_empty() {
            "(no summary provided — review the executor output directly)"
        } else {
            request.background
        },
        recent = request.recent_output,
        question = if request.question.is_empty() {
            "Review for correctness, architecture, security, and silent failures. If sound, confirm in `summary`."
        } else {
            request.question
        },
        kind = request.kind.label(),
    );
    (REVIEW_SYSTEM_PROMPT.to_string(), user)
}

/// Build the (system, user) prompt pair for an ADVISE checkpoint call.
#[must_use]
pub fn build_advise_prompt(request: &AdvisorRequest<'_>) -> (String, String) {
    let user = format!(
        "## Task\n{task}\n\n\
         ## Background\n{background}\n\n\
         ## Recent Context (verbatim)\n{recent}\n\n\
         ## Question\n{question}\n\n\
         ## Task Kind\n{kind}",
        task = request.task_summary,
        background = if request.background.is_empty() {
            "(no prior findings)"
        } else {
            request.background
        },
        recent = request.recent_output,
        question = if request.question.is_empty() {
            "Review the current situation. Recommend the next action and flag any risks."
        } else {
            request.question
        },
        kind = request.kind.label(),
    );
    (ADVISE_SYSTEM_PROMPT.to_string(), user)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::advisor::types::TaskKind;
    use crate::gatekeeper::router::TaskCategory;

    fn sample_request<'a>(question: &'a str) -> AdvisorRequest<'a> {
        AdvisorRequest {
            task_summary: "Fix an off-by-one bug in the pagination helper.",
            background: "- src/pagination.rs line 42\n- returns one extra item",
            recent_output: "assertion failed: expected 10, got 11",
            question,
            kind: TaskKind::infer(TaskCategory::Complex, None, "fix a bug"),
        }
    }

    #[test]
    fn plan_prompt_contains_required_sections() {
        let req = sample_request("What's the minimal fix?");
        let (system, user) = build_plan_prompt(&req);
        assert!(system.contains("end_state"));
        assert!(system.contains("critical_path"));
        assert!(user.contains("## Task"));
        assert!(user.contains("## Planning Request"));
        assert!(user.contains("coding"));
    }

    #[test]
    fn review_prompt_requires_verdict() {
        let req = sample_request("");
        let (system, user) = build_review_prompt(&req);
        assert!(system.contains("\"verdict\""));
        assert!(system.contains("pass") && system.contains("revision_needed") && system.contains("block"));
        assert!(user.contains("## Review Target"));
    }

    #[test]
    fn advise_prompt_is_short_form() {
        let req = sample_request("Should I reproduce the bug in a test first?");
        let (system, _) = build_advise_prompt(&req);
        assert!(system.contains("under 120 words"));
    }

    #[test]
    fn empty_fields_get_readable_placeholders() {
        let req = AdvisorRequest {
            task_summary: "Test",
            background: "",
            recent_output: "",
            question: "",
            kind: TaskKind::DailyChat,
        };
        let (_, user) = build_plan_prompt(&req);
        assert!(user.contains("no prior context"));
        assert!(user.contains("no tool output"));
    }
}
