# OpenRouter balance settings UI evidence

Captured from the actual Vite-served dashboard on this settings branch in
visible Chrome on 2026-10-18. These are full application captures, not isolated
component previews or generated illustrations.

- Desktop: 1440 x 1000, reporting disabled, saved management key masked.
- Mobile: 390 x 844, reporting enabled after save and reload. Scrolled to show
  the credential and toggle together.
- The backend responses for the browser exercise were local, stateful fixtures;
  no production configuration, real credentials, or OpenRouter requests were used.
  This verifies rendering and the browser's PATCH/reload interactions, not a live
  OpenRouter integration. Native gateway tests separately cover config metadata,
  secret omission, persistence/encryption, and the runtime reporting gate.
- Browser actions: enable and save; reload; Change the management key (empty
  password input); type and save a replacement fixture; reload to the fixed mask;
  disable and save; reload with the credential still populated.
- Observed PATCH paths: `credit_balance_enabled=true`, `management_api_key`,
  `credit_balance_enabled=false`, under `providers.models.openrouter.primary`.
- No page errors or horizontal document overflow were observed.

The eight asterisks are the interface's constant saved-secret indicator. They do
not encode a credential or its length. The screenshots contain only fixture data.

The `openrouter-account-desktop.png` and `openrouter-account-mobile.png` captures
exercise the full overview page after adding the selected provider alias to the
balance response/card. They use a local `primary` alias and fictional credit
totals (100 total, 10 used). They do not contain an actual account balance.
