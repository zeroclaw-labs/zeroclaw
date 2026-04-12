# RCA: Vikunja Deployment Timeout (2026-03-15 6:36pm PDT)

## Symptom
Sam updated her Signal message with "⚠️ Request timed out while waiting for the model" after receiving a request to deploy a todolist app.

## Root Cause
Sam's agent loop timeout (1200s) was shorter than Walter's actual execution time for the deployment task (~39 minutes). Sam was blocking on `acp-client wait` inside a shell tool call when the overall agent loop timed out.

## Contributing Factor
Walter's LLM backend (litellm-local-default-vision via llama-swap) was slow, causing Walter to take 39 minutes for 651 history entries of tool iterations. The llama-swap logs expired so we can't confirm whether this was model loading latency, inference speed, or both.

## Timeline
- 01:16 UTC: Sam receives deployment request
- 01:17:15: Sam sends Walter work order #1 (recon, 634 chars)
- 01:20:11: Walter completes recon (3 min, 23 history entries) — SUCCESS
- 01:21:22: Sam sends Walter work order #2 (deployment, 1708 chars)
- 01:36 UTC: Sam's agent loop times out (1200s elapsed) — FAILURE
- 01:59:59: Walter completes deployment (39 min, 651 history entries) — SUCCESS but Sam already dead

## Deployment State
Walter successfully deployed Vikunja with: namespace, deployment (2 replicas), service, PVC, certificate, gateway, virtualservice. All running. Committed to Gitea (not pushed). Chose SQLite over Postgres (diverging from Sam's earlier plan).

## Recommendations
1. Increase Sam's `message_timeout_secs` from 600 to 900+ for long ACP tasks
2. Consider using `bg_run` for acp-client wait to avoid blocking the agent loop
3. Walter's LLM backend speed needs investigation
