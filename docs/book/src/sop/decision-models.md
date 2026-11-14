# Decision models

A decision model answers typed questions about an event instead of generating
text. An SOP can ask one, after its trigger matches and before a run starts:

- **Gate:** should this event start this SOP at all?
- **Mode:** which of the execution modes the author listed fits this run?

This lets one trigger (a webhook path, a channel, an MQTT topic) feed several
SOPs, with the model deciding which of them the event is really for and how
much human supervision each run gets.

Canonical provider models can use either native typed-decision protocol:
OpenAI's Decisions API (`POST <base_url>/v1/decisions`) or System One
(`POST <base_url>/v1/systemone`). Chat Completions and Responses structured
outputs are not substituted for either protocol.

Two services implement System One:

| Model | Where it runs | Notes |
|---|---|---|
| [TypeSafe Jev](https://docs.typesafe.ai/models) | Hosted API, API key | Event content is sent to TypeSafe. |
| [Laya](https://huggingface.co/convaiinnovations/laya) | Self-hosted (`laya-serve`) | Open weights; no key; event content stays local. |

## Configure models

Declare reusable connections and exact model keys under `providers`:

```toml
[providers.connections.typesafe]
kind = "system-one"
uri = "https://api.typesafe.ai"

[providers.connections.typesafe.auth]
mode = "api_key"
# Set api_key with `zeroclaw config` or
# ZEROCLAW_providers__connections__typesafe__auth__api_key.

[providers.connections.local]
kind = "system-one"
uri = "http://127.0.0.1:8000"

[providers.models.jev]
capability = "decision"
provider = "typesafe"
model = "jev-latest"

[providers.models.laya]
capability = "decision"
provider = "local"
model = "laya"
```

Connections with `kind = "system-one"` default to the System One protocol.
OpenAI, custom and vLLM connections use the native OpenAI Decisions protocol
by default; `api.decision.wire_api` selects `openai_decisions` or `system_one`.
Decision models reuse connection authentication, HTTP headers and timeouts.
The dashboard's model selector lists exact decision model keys only.

V3 `[decision_models.<alias>]` input is migrated to a canonical connection and
decision model during configuration loading. The alias remains the exact model
key, including dots. Runtime, CLI validation and the dashboard read only the
canonical registry; saved configuration no longer contains `decision_models`.

## Select a model in an SOP

In the web dashboard's SOP editor, tick **Decision model** under Execution
mode and pick a configured model from the dropdown; the gate, modes, and
thresholds are edited in the same panel. Saving rejects an invalid
`[decision]` table with the reason.

In a file-backed SOP, add a `[decision]` table to `SOP.toml` and name the alias:

```toml
[decision]
model = "jev"
gate = "Is the customer asking for money back?"
gate_threshold = 0.7
modes = ["auto", "supervised", "step_by_step"]
mode_instructions = "auto: under $50 and routine. step_by_step: over $500, legal threats, or anything unusual."
min_confidence = 0.7
```

| Field | Default | Effect |
|---|---:|---|
| `model` | required | Exact decision model key from `[providers.models]`. |
| `gate` | unset | Yes/no question. The run starts only when the model's "yes" probability reaches `gate_threshold`; otherwise the event is recorded as skipped for this SOP. |
| `gate_threshold` | `0.7` | Minimum "yes" probability to start. |
| `gate_on_error` | `run_strict` | When the model cannot answer: `run_strict` starts the run in its fail-closed mode, `skip` does not start it. |
| `modes` | empty | Modes the model may choose for this run. Only `auto`, `supervised`, and `step_by_step`. Empty keeps the authored `execution_mode`. |
| `mode_instructions` | generic | Guidance for the mode choice. |
| `min_confidence` | `0.7` | A mode choice below this confidence uses the fail-closed mode. |
| `part_threshold` | `0.5` | Minimum "yes" probability to run a step with a `decide` question. See [Conditional steps](#conditional-steps). |

A `[decision]` table needs a `gate`, `modes`, or a step with `decide`. `modes`
cannot be used with a deterministic SOP.

## Conditional steps

A step can carry its own yes/no question. The model answers it in the same
request as the gate and mode, so one call decides which steps a run includes.
This composes one SOP from parts instead of splitting it into several SOPs that
each ask the model whether to start.

```markdown
## Steps

1. **Hold for a maintainer** - Draft a comment that tags a maintainer to decide on scope.
   - decide: Does this PR add a new feature outside the roadmap and existing features?

2. **Security review** - Review the auth, secret, and sandbox changes.
   - decide: Does this PR touch authentication, secrets, or sandboxing?
   - unless_decided: 1

3. **Test review** - Check that the changed behavior is tested.
   - decide: Does this PR change runtime behavior?
   - unless_decided: 1

4. **Compose the review** - Combine the findings into one review comment.
   - unless_decided: 1
```

- `- decide: <question>` runs the step only when the answer's "yes"
  probability reaches `part_threshold`.
- `- unless_decided: N` skips the step when step N's question was answered
  yes. Step N must have a `decide` question.
- A skipped step is recorded with status `skipped` and the reason, and the run
  continues with the next step in order. The next step receives the input the
  skipped step would have received.
- When the model cannot answer, or answers a step's question with a malformed
  value, that step runs. Skipping is never the fail-safe.
- Routing guards can read the answers: `$.decisions.2` is step 2's "yes"
  probability in a `when:` condition.
- Another step cannot `depends_on` a conditional step, because a skipped step
  produces no output. Read `$.steps.N` in a `when:` condition instead.
- A step is skipped before its approval gate, so a skipped step never asks for
  approval.

## Safety

- The model only chooses among what the author wrote. It cannot add triggers,
  pick an unlisted mode, or select `priority_based` or `deterministic`.
- A chosen mode applies to that run only and is stored with the run, so it
  survives a restart. Step-level `requires_confirmation` and checkpoints still
  gate regardless of the chosen mode.
- **Fail-closed:** an unconfigured alias, a network or HTTP error, a malformed
  answer, or a low-confidence answer never reduces supervision. The run uses the
  strictest of its listed modes, its authored `execution_mode`, and
  `supervised`.
- Event payloads are sent to the model framed as untrusted content, capped at
  8,000 characters. Choose a self-hosted model when event content must not leave
  the host.

Each decision is logged with the SOP name, outcome, chosen mode, each
conditional step's answer, and input tokens.
