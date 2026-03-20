---
id: haiku-local-model-2db7
stage: done
deps: [tiered-model-routing-63c1]
links: []
created: 2026-03-19T15:52:15Z
type: feature
priority: 1
assignee: Dustin Reynolds
tags: [providers, cost, routing, haiku]
skipped: [verify]
version: 8
---
# Haiku/local model classifier for intelligent query routing

Replace static keyword-based query classification with a Haiku (or local model) call that scores message complexity and intent. Current keyword rules are brittle — 'what is the architectural impact' matches the 'fast' hint. A lightweight model call (~200ms, fractions of a cent) can judge whether a message needs Opus-level reasoning or Sonnet is sufficient. Also use Haiku/local models for: (1) quick status acknowledgments on long-running tmux tasks, (2) cron notification text, (3) tool output summarization. Consider qwen3:4b via ollama as a zero-cost local alternative for classification when ollama is available.

## Notes

**2026-03-19T16:18:26Z**

Implemented: qwen3:4b via ollama as classifier_provider in query_classification config. 3-tier routing: simple→haiku, moderate→sonnet, complex→opus. 5s timeout with static keyword fallback. classifier.rs gains classify_with_model() async fn. channels/mod.rs updated to call model-based classifier when configured.
