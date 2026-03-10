import { defineConfig } from '@playwright/test';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const FRONTEND_PORT = Number(process.env.PW_WEB_PORT ?? '4173');
const BACKEND_PORT = Number(process.env.PW_API_PORT ?? '4174');
const rootDir = path.dirname(fileURLToPath(import.meta.url));

export default defineConfig({
  testDir: './e2e',
  timeout: 60_000,
  expect: {
    timeout: 10_000,
  },
  use: {
    baseURL: `http://127.0.0.1:${FRONTEND_PORT}`,
    headless: true,
    trace: 'on-first-retry',
  },
  webServer: [
    {
      command: `node e2e/mock-server.mjs`,
      url: `http://127.0.0.1:${BACKEND_PORT}/health`,
      reuseExistingServer: true,
      cwd: rootDir,
    },
    {
      command: `VITE_API_TARGET=http://127.0.0.1:${BACKEND_PORT} VITE_WS_TARGET=ws://127.0.0.1:${BACKEND_PORT} npm run dev -- --host 127.0.0.1 --port ${FRONTEND_PORT}`,
      url: `http://127.0.0.1:${FRONTEND_PORT}/_app/`,
      reuseExistingServer: true,
      cwd: rootDir,
    },
  ],
});
