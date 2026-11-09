# Provider registry dashboard evidence

Captured on the merged source at `cf0b95137119d751cff75c682079daaf8d3fa67b` (Web tree `4f8b8a4c133996c6a76a805833382142b0994d64`).
The follow-up evidence commit changes only these artifacts; the rendered Web
source is identical. The app is the actual Vite dashboard at
`/config/providers.registry?model=chat.fast.us`, rendered with headless Chromium
in `en-US`. API responses use synthetic fixtures and masked credentials.
These captures do not establish live-vendor or real-backend behavior.

- [Desktop editor](web-models-desktop.png): 1440 × 1000, exact dotted model key.
- [Invalid JSON](web-invalid-json-desktop.png): rejected before sending a write.
- [Mobile editor](web-models-mobile.png): 390 × 844.
- [Mobile actions](web-actions-mobile.png): the save/reload controls after scrolling.
- [Browser result](browser-results.json): request sequence, one successful save,
  reload of the admitted document, and zero page script errors.

The browser opened `chat.fast.us`, changed its upstream model from `chat-v1` to
`chat-v2`, saved the complete registry, checked its original masked CAS
snapshot, submitted invalid JSON without an additional write, reloaded, and
resized to the mobile viewport. The surrounding dashboard and navigation are
visible so wrapping and the action area can be reviewed.
