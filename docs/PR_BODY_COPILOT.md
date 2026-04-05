Title: Add GitHub Copilot integration, dashboard /api/channels endpoint, and Copilot model dropdown

This PR bundles the work to make GitHub Copilot a first-class provider in ZeroClaw and to fix the Channels dashboard JSON parse error.

What changed

- Add `docs/COPILOT_INTEGRATION.md` describing the change and how to test.
- Expose `copilot` (alias `github-copilot`) in the provider dropdown and add a curated `COPILOT_MODELS` dropdown in `web/src/pages/config/sections/GeneralSection.tsx`.
- Implement `/api/channels` JSON handler in `src/gateway/api.rs` and wire it in `src/gateway/mod.rs` so the dashboard no longer receives HTML from the SPA fallback.
- Add GitHub Copilot integration entry in `src/integrations/registry.rs` and guidance in `src/integrations/mod.rs`.
- Normalize channel health/session lookup keys to lowercase/underscore form.
- Add regression tests covering the channels builder and Copilot integration presence.

Why

- The Channels dashboard was failing with `response.json()` parse errors when `/api/channels` was missing and the Axum SPA fallback returned HTML. This makes the API contract explicit and prevents that failure mode.
- Making Copilot selectable in onboarding and the config UI improves UX for Copilot subscribers and aligns the onboarding defaults with model selection.

How to test

1. Run the focused regression tests added:

```bash
cargo test build_channel_details_aggregates_sessions_and_maps_health
cargo test github_copilot_entry_is_present_and_active_when_selected
```

2. Run formatting and checks:

```bash
cargo fmt --all && cargo check
```

3. Start the gateway and open the web dashboard; verify the Channels page loads and the provider dropdown contains "GitHub Copilot" with the Copilot model dropdown.

Suggested reviewers (CODEOWNERS):

- @theonlyhennygod
- @JordanTheJet
- @SimianAstronaut7

Notes

- Localization follow-up is out of scope for this PR.
- If you prefer I push and open the PR for you, ensure you have push/PR rights on the remote and that `gh` is installed; otherwise I committed the branch locally and you can push/open with the commands below.

Commands I ran (locally):

```bash
# create branch and commit locally
# git checkout -b feat/copilot-integration
# git add -A
# git commit -m "feat: add GitHub Copilot integration and /api/channels endpoint + docs"
```

Commands to push and create PR (copy/paste):

```bash
git push -u origin feat/copilot-integration
# then (if you have gh):
# gh pr create --fill --title "Add GitHub Copilot integration, dashboard /api/channels endpoint, and Copilot model dropdown" --body-file docs/PR_BODY_COPILOT.md --reviewer theonlyhennygod --reviewer JordanTheJet --reviewer SimianAstronaut7
```
