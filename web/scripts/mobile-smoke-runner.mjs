#!/usr/bin/env node
import { spawnSync } from 'node:child_process';
import path from 'node:path';
import process from 'node:process';
import { fileURLToPath } from 'node:url';
import { createRequire } from 'node:module';

const require = createRequire(import.meta.url);
const rootDir = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const isWin = process.platform === 'win32';

function localBin(name) {
  const suffix = isWin ? '.cmd' : '';
  return path.join(rootDir, 'node_modules', '.bin', `${name}${suffix}`);
}

function run(command, args) {
  const result = spawnSync(command, args, {
    cwd: rootDir,
    stdio: 'inherit',
    env: process.env,
  });

  if (result.error) {
    throw result.error;
  }

  return result.status ?? 1;
}

function hasPlaywright() {
  try {
    require.resolve('@playwright/test', { paths: [rootDir] });
    return true;
  } catch {
    return false;
  }
}

const vitestArgs = [
  'run',
  'src/pages/Dashboard.test.tsx',
  'src/components/layout/Sidebar.test.tsx',
];

if (hasPlaywright()) {
  console.log('[mobile-smoke] Playwright detected. Running browser + fallback smoke tests.');
  const playwrightStatus = run(localBin('playwright'), ['test', 'e2e/dashboard.mobile.spec.ts']);
  if (playwrightStatus !== 0) {
    process.exit(playwrightStatus);
  }
  process.exit(run(localBin('vitest'), vitestArgs));
}

console.log('[mobile-smoke] @playwright/test not vendored. Running Vitest mobile smoke fallback.');
process.exit(run(localBin('vitest'), vitestArgs));
