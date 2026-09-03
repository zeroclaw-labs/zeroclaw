## Summary

Base branch: `master`.

Anthropic's current Claude generations changed the Messages API in ways this
adapter predates. They think adaptively and reject the fixed thinking budget,
they reject every sampling parameter, they take reasoning depth in
`output_config.effort`, they return reasoning blocks whose text is withheld by
default, they bind a reasoning block to the conversation prefix that produced
it, and their safety classifiers decline a request with a successful response
rather than an error.

Pointing an alias at one of these models therefore failed in several ways at
once: native thinking returned a request error on every model newer than the
single one the old gate excluded, streamed tool loops dropped the reasoning the
model had signed, a declined request was retried on the same model and reported
as an invalid completion, and the thinking levels mapped to a budget the API no
longer accepts.

What this changes:

- Read the model generation from the alias `model` and pick the request shape
  from it, so a new release needs no code change. Earlier models keep the fixed
  budget and their temperature.
- Map the thinking level to reasoning depth on the current generations. A
  configured temperature is dropped with a warning naming the value, and a
  warning fires when the output cap is still at the baseline, because reasoning
  now counts against it.
- Capture streamed reasoning blocks, which the streaming parser had never read,
  and replay them only within the tool round that produced them.
- Add `thinking_display` on the `anthropic` slot so an operator can read the
  reasoning as a summary or as the progress notes written between tool calls.
- Treat a declined request as its own outcome: no retry on that model, failover
  to the configured fallbacks, and a localized message naming the refusal
  category.

Not in scope: global default changes for output caps and timeouts, reasoning
visibility on Bedrock, doctor checks for these aliases, quickstart filling in
`context_window`, capture of redacted reasoning blocks, and a thinking level for
RPC sessions, which still send none.

## Testing

Reviewer testing requested: yes. Interfaces: CLI and zerocode.

Setup: an `[providers.models.anthropic.<alias>]` entry with
`model = "claude-fable-5-1"`, `max_tokens = 32000`, `timeout_secs = 900`,
`context_window = 1000000`, `thinking_display = "summarized"` and
`fallback_models = ["claude-opus-5"]`; a second alias with `temperature` set and
`max_tokens = 2048` as a negative control; a third on `claude-sonnet-4-5` with
`native_thinking = true` as a regression control; a runtime profile with
`default_level = "high"`.

Steps and expectations:

1. Run one CLI turn that uses a tool. The prepared-request debug line reports
   adaptive thinking, the summarized display and high effort, with no budget and
   no temperature, and the second tool round succeeds.
2. Send `/think:max` and `/think:low` messages and confirm the logged depth
   changes; `/think:medium` sends none.
3. The negative-control alias logs both warnings. The regression-control alias
   still sends the budget shape with temperature 1.0.
4. In zerocode, run a tool task with the thought panel open: the reasoning reads
   as prose, not JSON. Switch to another model from the title-bar picker and
   continue; the next request succeeds.

## How I tested

`cargo fmt --all -- --check`, `./scripts/ci/rust_quality_gate.sh --strict`
(includes the provider dispatch gate), `scripts/ci/comment_hygiene_gate.sh`, and
both docs gates all pass. `cargo nextest run --locked --workspace --exclude
zeroclaw-desktop --no-fail-fast` reports 15174 of 15175 passing; the single
failure is `grok_cli::tests::unix_fake_child::timeout_kills_leader_and_descendant`,
a pre-existing timing flake on macOS in a file this branch does not touch, which
passes on every isolated rerun.

Not covered by these runs: no request was made against the live API, so the
adaptive request shape, the streamed reasoning capture and the refusal path are
verified by unit tests and fixtures rather than by a real response. Whether
Bedrock forwards the depth setting through its additional request fields is
likewise unverified.

## Security and privacy

No new permissions, no new endpoints, no secrets handling, and no new personal
data. One beta header is added, and only when an operator opts into the progress
notes display.

The refusal path handles provider-controlled text carefully: the category is
mapped onto a closed set on arrival, the provider's free-text explanation is
never read, and an unrecognised category reaches only a debug log. Every word of
the delivered message comes from the locale catalogues.

## Compatibility

Backward compatible. The one config addition is optional and additive, so no
migration is needed. Earlier Claude models keep their existing request shape.

Two behaviour changes worth naming: models in the 4.6 generation move from the
fixed budget to adaptive depth, so a configured budget no longer applies to
them; and setting a non-default thinking level now turns reasoning on for the
generations that default it off, which costs tokens it did not before.

## Rollback

`git revert` the range. Without a revert, unsetting `thinking_display` and
removing `temperature` from these aliases returns the request to its previous
shape on the affected models.
