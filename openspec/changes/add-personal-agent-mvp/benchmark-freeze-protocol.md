# Benchmark and freeze protocol (task 1.4)

Candidate selection and the measurement protocol for freezing appendix B1.
Everything here is a **proposal**: nothing is pilot-ready, and no value freezes
until the protocol below has run on the actual pilot hardware with recorded
artifacts (A16, A17). The pilot owner confirms the model choice before the
benchmark runs.

## Candidate local model

| | Primary candidate | Alternative |
|---|---|---|
| Model | `qwen3:8b` (Qwen3 8B, instruct) via the existing `ollama` provider | `llama3.1:8b` via Ollama, or any OpenAI-compatible server (llama.cpp / LM Studio) through the `compatible` provider |
| Why | CJK-strong (PRD §6.4 fields and A23 test long Chinese input), 8B class is the plausible fit for the candidate 4-core / 16 GiB / no-GPU host, quantized builds keep the 8192-token budget realistic | Same size class; weaker CJK token efficiency — measure before choosing |
| Explicit risks | CPU-only generation latency may miss the 60 s candidate deadline; that outcome adjusts the model or the published deadline (appendix B1), never isolation | — |

Selection criteria recorded now: context window ≥ 8192 reported as
`ModelContextWindowSource::Configured` (never the compatibility fallback),
deterministic JSON-envelope adherence on the offline semantic corpus (A10),
CJK token efficiency on the fixed corpus, and measured P95 within the
candidate deadline. A model failing any criterion is replaced or the deadline
is renegotiated — no benchmark tuning weakens isolation or the deterministic
checks (design Open Questions).

## Tokenizer plan (the "actual tokenizer" requirement)

The repo today estimates tokens approximately; those estimates are display-only
(`zeroclaw-api/src/agent.rs:238`). Appendix B1 and design §6 require the actual
model tokenizer to govern the 8192 budget. Two supported routes:

1. **Server-side**: Ollama's `/api/tokenize` for models that expose it — the
   provider already owns the connection; the personal adapter calls it per
   request. Preferred: zero new dependencies, exact counts.
2. **Embedded**: pin the model's `tokenizer.json` (hash recorded at freeze) and
   load it through the Hugging Face `tokenizers` crate as a bounded dependency
   of `zeroclaw-personal` only — never runtime.

Unsupported counting fails configuration validation at startup (design §6);
there is no silent estimate fallback. The freeze record states which route and
which tokenizer artifact hash is in force.

## TLS certificate delivery (candidate)

- Pilot: a private CA whose root is distributed through the organization's
  existing trust channel; the visitor listener presents a leaf for the LAN
  hostname. Delivery steps belong in the deployment doc (task 12.1).
- Development: `mkcert`-style local CA on the operator's machine only.
- Never a self-signed prompt-to-click-through and never disabled validation
  (design §3); activation fails closed on untrusted TLS.

## SMTP sandbox (candidate)

- Automated tests: **Mailpit** as a local SMTP sink — accepts, stores, and has
  a REST API the test harness can assert against; supports the failure modes
  the retry schedule needs (reject after DATA, accept-then-drop for the
  crash-after-send ambiguity, slow accept for lease tests).
- Pilot: the organization-approved SMTP endpoint (design Open Question);
  Mailpit never serves a real pilot.

## Benchmark protocol (to run before freeze)

1. **Pin the environment**: model + quantization + exact Ollama/compatible
   server version, host model (CPU cores, RAM, governor), ZeroClaw commit.
   Record all of it in the freeze artifact.
2. **Pin the corpus**: a fixed bilingual (CJK-heavy) question set drawn from
   the pilot knowledge scope, each with known eligible/ineligible sources;
   include the A23 long-Chinese probes and injection probes. Corpus is
   versioned alongside the test fixtures.
3. **Timing decomposition** (appendix B1 "性能分解" row): per request record
   queue wait, retrieval, provider first token, full generation, deterministic
   check, and user-visible total. First token ≠ first visible word (buffered
   publication).
4. **Five-person workload** (A17): scripted schedule — five persons submit
   concurrently, one floods (multi-device), one cancels mid-queue, one runs a
   long task while others wait; assert reservation and round-robin behavior,
   and that no person's second wait is admitted under the default five-slot
   configuration.
5. **Egress intercept on** for the whole run (A06): the benchmark must not
   require, and must not silently use, any external endpoint.
6. **Report**: P95 with sample count, deadline misses, queue rejection counts,
   cancel/timeout release times, and the unsupported-limits discovered
   (tokenizer, context). Misses are reported, never absorbed by loosening
   checks.
7. **Freeze decision**: recorded in this change's tasks (12.4) with the
   artifact paths; appendix B1 values update from candidates to frozen numbers,
   and the pilot-owner acknowledgment that five reserved waiting slots provide
   no second same-person wait is captured at the same time.

## Status

Not pilot-ready. No benchmark has run. The model row is a proposal pending the
pilot owner's selection; the protocol above is the agreement on how the freeze
numbers will be produced.
