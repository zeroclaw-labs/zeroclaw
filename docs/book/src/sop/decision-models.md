# Decision models

A decision model answers typed questions about an event instead of generating
text. An SOP can ask one, after its trigger matches and before a run starts:

- **Gate:** should this event start this SOP at all?
- **Mode:** which of the execution modes the author listed fits this run?

This lets one trigger (a webhook path, a channel, an MQTT topic) feed several
SOPs, with the model deciding which of them the event is really for and how
much human supervision each run gets.

ZeroClaw speaks the "System One" typed-decision API (`POST <base_url>/v1/systemone`).
Two models implement it:

| Model | Where it runs | Notes |
|---|---|---|
| [TypeSafe Jev](https://docs.typesafe.ai/models) | Hosted API, API key | Event content is sent to TypeSafe. |
| [Laya](https://huggingface.co/convaiinnovations/laya) | Self-hosted (`laya-serve`) | Open weights; no key; event content stays local. |

## Configure models

Declare each model under an alias in `config.toml` and pick its `provider`:

| `provider` | Service | `base_url` default | `model` default | Key |
|---|---|---|---|---|
| `jev` (default) | TypeSafe Jev, hosted | `https://api.typesafe.ai` | `jev-latest` | `api_key` required |
| `laya` | Laya via `laya-serve` | `http://127.0.0.1:8000` | `laya` | none |
| `custom` | Any other System One-compatible endpoint | required | `jev-latest` | optional |

```toml
[decision_models.jev]
provider = "jev"
# api_key is a secret; set it with `zeroclaw config` or
# ZEROCLAW_decision_models__jev__api_key.

[decision_models.laya]
provider = "laya"

[decision_models.gpu_box]
provider = "custom"
base_url = "http://10.0.0.5:8000"
model = "laya"
```

`base_url` and `model` override the provider defaults. A `custom` entry without
a `base_url` is ignored, and SOPs selecting it fall back to their strictest mode.
The aliases can also be managed from the dashboard's Config page.

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
| `model` | required | Alias from `[decision_models]`. |
| `gate` | unset | Yes/no question. The run starts only when the model's "yes" probability reaches `gate_threshold`; otherwise the event is recorded as skipped for this SOP. |
| `gate_threshold` | `0.7` | Minimum "yes" probability to start. |
| `gate_on_error` | `run_strict` | When the model cannot answer: `run_strict` starts the run in its fail-closed mode, `skip` does not start it. |
| `modes` | empty | Modes the model may choose for this run. Only `auto`, `supervised`, and `step_by_step`. Empty keeps the authored `execution_mode`. |
| `mode_instructions` | generic | Guidance for the mode choice. |
| `min_confidence` | `0.7` | A mode choice below this confidence uses the fail-closed mode. |

A `[decision]` table needs a `gate`, `modes`, or both. `modes` cannot be used
with a deterministic SOP.

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

Each decision is logged with the SOP name, outcome, chosen mode, and input
tokens.
