# Provider registry dashboard evidence

The [canonical UI follow-up](canonical-ui-20261110/observations.json) records
the production Web bundle rendered in Chrome with synthetic API data. It
checks selecting the exact model key `chat.fast.us`, clearing and selecting
the single-part key `main`, and opening the registry's Models tab through an
exact-key link. The captures use `en-US`, desktop 1280 × 900 and mobile
390 × 844; they do not establish real-backend or vendor behavior.

- [Quickstart desktop](canonical-ui-20261110/quickstart-desktop.png).
- [Quickstart selected model](canonical-ui-20261110/quickstart-selected.png).
- [Azure required resource](canonical-ui-20261110/quickstart-azure-required.png):
  staging remains disabled until the connection's resource is supplied.
- [Quickstart mobile](canonical-ui-20261110/quickstart-mobile.png).
- [Registry desktop](canonical-ui-20261110/registry-desktop.png).
- [Registry mobile](canonical-ui-20261110/registry-mobile.png).

The earlier registry-save evidence below is historical.

Captured on the reviewed fixes at `130f5f025e` (the exact revision and Web tree are recorded in `browser-results.json`).
The follow-up evidence commit changes only these artifacts; the rendered Web
source is identical. The app is the actual Vite dashboard at
`/config/providers.registry?model=chat.fast.us`, rendered with headless Chromium
in `en-US`. API responses use synthetic fixtures and masked credentials.
These captures do not establish live-vendor or real-backend behavior.

- [Desktop editor](web-models-desktop.png): 1440 × 1000, exact dotted model key.
- [Invalid JSON](web-invalid-json-desktop.png): rejected before sending a write.
- [Banner save in progress](web-save-pending-desktop.png): editing, saving,
  and reloading remain disabled until the masked server snapshot is refreshed.
- [Mobile editor](web-models-mobile.png): 390 × 844.
- [Mobile actions](web-actions-mobile.png): the save/reload controls after scrolling.
- [Browser result](browser-results.json): request sequence, two successful saves,
  reload of the admitted document, and zero page script errors.

The browser opened `chat.fast.us`, changed its upstream model from `chat-v1` to
`chat-v2`, saved the complete registry, checked its original masked CAS
snapshot, submitted invalid JSON without an additional write, and reloaded.
It then changed the model to `chat-v3`, saved through the banner with a delayed
PUT response, verified that the editor and its actions were disabled, and
waited for refresh before resizing to the mobile viewport. The surrounding dashboard and navigation are
visible so wrapping and the action area can be reviewed.
