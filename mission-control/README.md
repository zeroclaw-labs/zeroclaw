# ClawPilot Workbench (Mission Control)

Mission Control is the workspace-first UI for ClawPilot.

## Runtime bridge

Mission Control talks to local runtime files/APIs:

- run queue: `RUNTIME_QUEUE_ROOT` (default `/var/lib/clawpilot/queue`)
- run results: `RUNTIME_RESULTS_ROOT` (default `/var/lib/clawpilot/results`)
- mission state data: `MISSION_CONTROL_DATA_ROOT` (default `/var/lib/clawpilot/mission-control`)

## Developer mode

```bash
npm install
npm run dev
```

Optional checks:

```bash
npm run lint
npm run test
```

## Production mode

```bash
npm ci
npm run build
npm run start
```

No Convex dev server is required in production mode.
