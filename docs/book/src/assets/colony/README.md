# Colony browser evidence

These captures use the compiled dashboard renderer in Chrome with synthetic
HTTP, agent, channel and model responses. They show interface behavior; the
gateway and runtime tests provide execution and permission-boundary evidence.
All names, context, domains and access settings in the captures are fixtures.

- [Desktop graph](desktop-graph.png): Queen, agents, recurring text, room and
  explicit external channel, with separate communication directions.
- [Desktop review](desktop-review.png): grouped goal setup and consolidated
  review before Start.
- [320-pixel mobile setup](mobile-320.png): compact setup without horizontal
  document overflow.
- [Run metadata](result.json): browser version, source fingerprints, compiled
  asset names and exercised mutation counts.

To reproduce after generating the API and building the dashboard:

```sh
cd web
npm run build
WEB_SMOKE_DIST=1 npm run test:colony
```

The opt-in smoke requires Playwright and a Chrome installation. If Playwright
is provided by an external runtime, set `PLAYWRIGHT_MODULE` to its module path.
Set `WEB_SMOKE_OUTPUT` to change the capture directory. The default is
`/tmp/zeroclaw-colony-evidence`; no fixture credentials reach a provider or
external channel.

The smoke exercises selection, retained chat drafts, grouped clarification,
explicit initial agent-batch approval, continued and fresh goals, one-way
wiring, prompt timing, room/channel grants, one-time tool approval, final
Queen plan review, pause/resume/cancel, partial mutation success, empty
installations, and desktop/mobile themes.
