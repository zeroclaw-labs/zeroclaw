// Actual dashboard browser evidence with deterministic gateway-shaped fixtures.
// This checks rendering and interaction, not real OIDC or gateway authorization.
// Start `npm run dev`, then run this with Playwright installed separately.
const assert = require('node:assert/strict');
const fs = require('node:fs/promises');
const path = require('node:path');
const { execFileSync } = require('node:child_process');
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');

const origin = process.env.WEB_SMOKE_URL || 'http://127.0.0.1:5173';
const output = path.resolve(process.env.WEB_SMOKE_OUTPUT || path.join(__dirname, '../../tmp/scoped-chat-smoke'));
const revision = execFileSync('git', ['rev-parse', 'HEAD'], { encoding: 'utf8' }).trim();
const worktreeDirty = execFileSync('git', ['status', '--porcelain'], { encoding: 'utf8' }).trim() !== '';
const results = [];

async function exercise(browser, viewport) {
  const context = await browser.newContext({ viewport, reducedMotion: 'reduce' });
  const page = await context.newPage();
  const errors = [];
  page.on('pageerror', (error) => errors.push(error.message));
  let agentsFail = true;
  let disabled = false;
  let sockets = 0;
  const prompts = [];
  const requests = [];
  const prefix = `${viewport.width}x${viewport.height}`;
  await context.addInitScript(() => {
    if (!localStorage.getItem('zeroclaw-locale')) localStorage.setItem('zeroclaw-locale', 'en');
  });
  await page.route('**/*', async (route) => {
    const url = new URL(route.request().url());
    if (url.origin !== origin) return route.abort();
    const pathname = url.pathname;
    const respond = (body, status = 200) => route.fulfill({
      status, contentType: 'application/json', body: JSON.stringify(body),
    });
    if (pathname === '/health') return respond({ status: 'ok', require_pairing: true });
    if (!pathname.startsWith('/api/')) return route.continue();
    requests.push(pathname);
    if (pathname === '/api/auth/methods') return respond({
      pairing_required: true, oidc: [{ alias: 'review', provider: 'Review IdP' }],
    });
    if (pathname === '/api/auth/session') return respond({
      kind: 'oidc', provider: 'oidc.review', principal_id: 'review-account',
      display_name: 'Review account', admin: false,
      expires_at: Math.floor(Date.now() / 1000) + 3600, csrf_token: 'fixture-only',
      features: { chat: true, operator_pages: false, core_sessions: true },
    });
    if (pathname === '/api/agents/available') return agentsFail
      ? respond({ error: 'temporarily unavailable' }, 503)
      : respond({ agents: [{ alias: 'support', enabled: true }] });
    if (pathname === '/api/status') return respond({ model: 'fixture-model', locale: 'en' });
    return respond({ error: 'not available', reason: 'operator_required' }, 403);
  });
  await page.routeWebSocket('**/ws/chat**', (socket) => {
    sockets += 1;
    setTimeout(() => socket.send(JSON.stringify({
      type: 'session_start', session_id: 'fixture-conversation', resumed: false, message_count: 0,
    })), 30);
    socket.onMessage((raw) => {
      const frame = JSON.parse(String(raw));
      if (frame.type !== 'message') return;
      prompts.push(frame.content);
      socket.send(JSON.stringify(disabled ? {
        type: 'error', code: 'AGENT_NOT_DISPATCHABLE',
        message: 'Agent `support` is not enabled for dispatch',
      } : { type: 'done', full_response: 'Fixture reply: conversation remains available.' }));
    });
  });
  async function capture(name) {
    await page.screenshot({ path: path.join(output, `${prefix}-${name}.png`), fullPage: true });
    results.push({ viewport, name, url: page.url() });
  }
  try {
    await page.goto(`${origin}/agents`);
    await page.locator('[data-agents-retry]').waitFor();
    assert.equal(sockets, 0);
    await capture('agents-retry');
    agentsFail = false;
    await page.locator('[data-agents-retry]').click();
    await page.locator('[data-available-agent="support"]').waitFor();
    assert.equal(await page.locator('[data-agents-load-error]').count(), 0);
    await capture('agents-recovered');

    agentsFail = true;
    await page.goto(`${origin}/agent/support`);
    await page.locator('[data-agents-retry]').waitFor();
    assert.equal(await page.locator('[data-not-available]').count(), 0);
    assert.equal(sockets, 0);
    await capture('chat-retry');
    agentsFail = false;
    await page.locator('[data-agents-retry]').click();
    await page.locator('textarea').waitFor();
    await capture('chat-recovered');

    await page.evaluate(() => localStorage.setItem('zeroclaw-locale', 'es'));
    await page.reload();
    await page.locator('textarea').waitFor();
    disabled = true;
    await page.locator('textarea').fill('Check the unavailable-agent message.');
    await page.locator('textarea').press('Enter');
    await page.getByText('Este agente no puede recibir mensajes en este momento.', { exact: false }).waitFor();
    assert.equal(await page.getByText('not enabled for dispatch', { exact: false }).count(), 0);
    await capture('localized-refusal');
    disabled = false;
    await page.locator('textarea').fill('Continue this conversation.');
    await page.locator('textarea').press('Enter');
    await page.getByText('Fixture reply: conversation remains available.', { exact: false }).waitFor();
    assert.equal(sockets, 1);
    assert.equal(prompts.length, 2);
    await capture('same-conversation-recovery');
    assert.deepEqual(errors, []);
    assert.equal(requests.some((url) => url === '/api/sessions'), false);
  } finally {
    await context.close();
  }
}

(async () => {
  await fs.mkdir(output, { recursive: true });
  const browser = await chromium.launch({
    headless: true, executablePath: process.env.WEB_SMOKE_BROWSER || undefined,
  });
  try {
    for (const viewport of [{ width: 1280, height: 800 }, { width: 390, height: 844 }]) {
      await exercise(browser, viewport);
    }
    const report = { revision, worktreeDirty, fixtureBoundary: 'HTTP and WebSocket', results };
    await fs.writeFile(path.join(output, 'report.json'), `${JSON.stringify(report, null, 2)}\n`);
    console.log(`PASS: ${results.length} dashboard states at ${revision}${worktreeDirty ? ' (working tree)' : ''}`);
  } finally {
    await browser.close();
  }
})().catch((error) => {
  console.error(error);
  process.exitCode = 1;
});
