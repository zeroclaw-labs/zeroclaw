# GitHub Copilot Integration

This document summarizes the recent changes that add GitHub Copilot as a first-class provider in ZeroClaw's onboarding, configuration UI, and integrations catalog, and also exposes a new dashboard API used by the Channels page.

Summary of changes

- Added GitHub Copilot to the integrations catalog and CLI help text.
- Exposed `copilot` (and alias `github-copilot`) as a selectable provider in the web dashboard config UI (`GeneralSection.tsx`).
- Replaced the free-text model entry for Copilot with a dropdown catalog (`COPILOT_MODELS`) and set the onboarding default model to `gpt-5.4-mini`.
- Implemented a JSON `/api/channels` endpoint in the gateway so the Channels dashboard no longer receives the SPA HTML fallback and fails with `response.json()` parse errors.
- Normalized channel health/session lookup keys to lowercase/underscore form to avoid mismatches between UI labels and runtime keys.
- Added regression tests that validate the channels builder and the Copilot integrations entry presence.

Why this change

- The dashboard Channels page was throwing a `JSON.parse` error because a missing `/api/channels` route caused the Axum SPA fallback to return HTML. Adding a dedicated JSON endpoint eliminates this class of failures.
- GitHub Copilot is widely used; making it available in onboarding and model selection improves out-of-the-box ergonomics for Copilot subscribers.

How to test locally

1. Build and run focused tests:

```bash
cargo test build_channel_details_aggregates_sessions_and_maps_health
cargo test github_copilot_entry_is_present_and_active_when_selected
```

2. Full checks and formatting:

```bash
cargo fmt --all
cargo check
```

3. Run the gateway and open the dashboard to verify the Channels tab loads without errors and the provider dropdown contains "GitHub Copilot" with the Copilot model dropdown working.

Notes and follow-ups

- Optional: update localized strings to mention GitHub Copilot in additional locales.
- Optional: add more granular per-channel health signals in the runtime (currently we normalize and present health from the global snapshot).

If anything looks off in the PR that I will open next, tell me which additional changes you'd like and I will update the branch accordingly.
