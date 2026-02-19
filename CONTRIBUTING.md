# Contributing to ClawSuite

Thanks for your interest in contributing! Here's how to get started.

## Quick Start

1. **Fork** the repo and clone your fork
2. **Install dependencies:** `npm install`
3. **Install Playwright:** `npx playwright install chromium` (required for Browser tab)
4. **Set up environment:**
   ```bash
   cp .env.example .env
   # Edit .env with your gateway URL and token
   # Find token: openclaw config get gateway.auth.token
   ```
5. **Run dev server:** `npm run dev`
6. **Make your changes** on a feature branch
7. **Open a PR** against `main`

## Development

```bash
# Install dependencies
npm install

# Install Playwright browser
npx playwright install chromium

# Dev server (default: localhost:3000)
npm run dev

# Type check
npm run typecheck

# Lint
npm run lint

# Build for production
npm run build
```

**First-time setup:**
- Copy `.env.example` to `.env`
- Set `CLAWDBOT_GATEWAY_URL` (default: `ws://127.0.0.1:18789`)
- Set `CLAWDBOT_GATEWAY_TOKEN` (find with `openclaw config get gateway.auth.token`)
- See [README.md](README.md#environment-setup) for detailed environment variable documentation

## Guidelines

- **One PR per feature/fix** — keep them focused
- **Test your changes** — make sure the app builds and runs
- **Describe what you changed** — clear PR title + description
- **No secrets** — never commit API keys, tokens, or passwords
- **Follow existing patterns** — match the code style you see

## Architecture

- **Framework:** TanStack Start + React
- **Styling:** Tailwind CSS
- **State:** TanStack Query + React hooks
- **Gateway communication:** WebSocket via OpenClaw RPC

Key directories:

```
src/
├── components/     # Shared UI components
├── hooks/          # Custom React hooks
├── lib/            # Utilities and helpers
├── routes/         # TanStack Router pages + API routes
├── screens/        # Major screen layouts (chat, dashboard)
└── server/         # Server-side gateway communication
```

## Reporting Issues

- Use [GitHub Issues](https://github.com/outsourc-e/clawsuite/issues)
- Include: what you expected, what happened, steps to reproduce
- Screenshots help!

## License

By contributing, you agree that your contributions will be licensed under the [MIT License](LICENSE).
