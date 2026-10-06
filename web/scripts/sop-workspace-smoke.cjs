// Browser-boundary regression checks. HTTP/RPC fixtures are synthetic; no model calls.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || 'playwright');
const assert = require('node:assert/strict');
const fs = require('node:fs');
const out = process.env.WEB_SMOKE_OUTPUT || '/tmp/zeroclaw-sop-evidence';
const appUrl = process.env.WEB_SMOKE_URL || 'http://127.0.0.1:5179';
fs.mkdirSync(out, { recursive: true });
const step = (number, title, extra = {}) => ({
  number,
  title,
  body: 'Inspect the changes and record the result.',
  kind: 'execute',
  requires_confirmation: false,
  suggested_tools: [],
  ...extra,
});
const sop = (name, description) => ({
  name,
  description,
  version: '1.0.0',
  priority: 'normal',
  execution_mode: 'supervised',
  triggers: [{ type: 'manual' }],
  steps: [
    step(1, 'Review changes'),
    step(2, 'Approve review', { kind: 'checkpoint' }),
    step(3, 'Publish review'),
  ],
  cooldown_secs: 0,
  max_concurrent: 1,
  admission_policy: 'parallel',
  max_pending_approvals: 0,
  deterministic: false,
  agent: 'reviewer',
});
const definitions = {
  'PR review': sop('PR review', 'Review changes, classify findings, and request approval.'),
  'Release checks': sop('Release checks', 'Prepare a release report for review.'),
};
const graph = (definition) => {
  const nodes = [
    ...definition.triggers.map((trigger, index) => ({
      kind: 'trigger',
      step: 1000000 + index,
      trigger_index: index,
      title: trigger.type,
      subtitle: 'Start the workflow',
      inputs: [],
      outputs: [],
    })),
    ...definition.steps.map((step) => ({
      kind: 'step',
      step: step.number,
      title: step.title,
      inputs: [],
      outputs: [],
    })),
  ];
  return {
    nodes,
    wires: nodes.slice(1).map((node, index) => ({
      class: 'flow',
      flow_role: index === 0 ? 'trigger' : 'sequence',
      from_step: nodes[index].step,
      to_step: node.step,
    })),
    diagnostics: [],
    layout: {
      columns: nodes.length,
      rows: 1,
      geometry: { node_w: 210, node_h: 84, col_gap: 130, row_gap: 46, origin: 24 },
      positions: nodes.map((node, index) => ({
        step: node.step,
        col: index,
        row: 0,
        ...definition.steps.find((step) => step.number === node.step)?.pos,
      })),
    },
  };
};
const runs = [
  {
    run_id: 'review-run',
    sop_name: 'PR review',
    active: true,
    status: 'paused_checkpoint',
    current_step: 2,
    total_steps: 3,
    started_at: '2026-10-05T15:00:00Z',
  },
  {
    run_id: 'release-run',
    sop_name: 'Release checks',
    active: true,
    status: 'running',
    current_step: 1,
    total_steps: 3,
    started_at: '2026-10-05T15:01:00Z',
  },
];
const overlay = (run) => ({
  run_id: run.run_id,
  sop_name: run.sop_name,
  status: run.status,
  current_step: run.current_step,
  total_steps: run.total_steps,
  waiting: false,
  paused: run.status === 'paused_checkpoint',
  nodes: [1, 2, 3].map((n) => ({
    step: n,
    state: n < run.current_step ? 'completed' : n === run.current_step ? 'active' : 'pending',
    tool_calls:
      n === 1
        ? [
            {
              index: 0,
              tool: 'file_read',
              args: { path: 'change.txt' },
              output: 'The change is ready for review.',
              success: true,
              duration_ms: 4,
            },
          ]
        : [],
  })),
});
let saves = 0;
let prompts = 0;
let projectedDraft;
let rejectGraph = false;
let rejectSave = false;
let rejectOverlay = false;
let decisions = 0;
let sockets = 0;
let browser;
let page;
(async () => {
  browser = await chromium.launch({ channel: 'chrome', headless: true });
  const context = await browser.newContext({ viewport: { width: 1440, height: 960 } });
  await context.addInitScript(() => {
    localStorage.setItem('zeroclaw_token', 'zc_test_sop_fixture');
    localStorage.setItem('zeroclaw_locale', 'en');
  });
  await context.route('**/api/**', async (route) => {
    const url = new URL(route.request().url());
    const path = decodeURIComponent(url.pathname);
    const method = route.request().method();
    let body = {};
    if (path === '/api/status')
      body = {
        version: '0.8.5',
        locale: 'en',
        check_updates: false,
        health: { components: {} },
        channels: {},
        process: {},
        nodes: { connected: [], mdns_peers: [] },
      };
    else if (path === '/api/quickstart/state')
      body = { quickstart_completed: true, agents: ['reviewer'] };
    else if (path === '/api/workspace')
      body = { agents: ['reviewer'], code: true, workflows: true, session_persistence: true };
    else if (path === '/api/config/drift') body = { drifted: [] };
    else if (path === '/api/config/reload-status') body = { pending_reload: false };
    else if (path === '/api/config/sections') body = { sections: [] };
    else if (path === '/api/config/map-keys') body = { keys: ['reviewer'] };
    else if (path === '/api/config/list')
      body = { entries: [{ path: 'agents.reviewer.enabled', value: true, populated: true }] };
    else if (path === '/api/tools')
      body = {
        tools: [
          {
            name: 'file_read',
            description: 'Read a workspace file.',
            parameters: { type: 'object', properties: { path: { type: 'string' } } },
          },
        ],
      };
    else if (path === '/api/cli-tools') body = { tools: [] };
    else if (path === '/api/sops/runs')
      body = {
        runs: runs.filter(
          (run) => !url.searchParams.get('sop') || run.sop_name === url.searchParams.get('sop'),
        ),
      };
    else if (path === '/api/sops/trigger-sources')
      body = { sources: ['manual'], bound: [], channels: [], operators: [] };
    else if (path === '/api/sops/graph-legend')
      body = { flow_roles: [], pin_classes: [], run_states: [] };
    else if (path === '/api/sops/graph-draft') {
      projectedDraft = route.request().postDataJSON().sop;
      body = graph(projectedDraft);
      if (rejectGraph && projectedDraft.name === 'New workflow') {
        rejectGraph = false;
        body.diagnostics = [{ severity: 'error', message: 'Invalid fixture routing', step: 1 }];
      }
    } else if (path === '/api/sops' && method === 'GET')
      body = {
        sops: Object.values(definitions).map(({ name, description, steps }) => ({
          name,
          description,
          steps: steps.length,
        })),
      };
    else if (path === '/api/sops' && method === 'POST') {
      const value = route.request().postDataJSON();
      definitions[value.name] = {
        ...value,
        steps: value.steps.map((step, index) => ({ ...step, number: index + 1 })),
      };
      saves++;
      body = { created: value.name };
    } else if (path.match(/^\/api\/sops\/[^/]+$/) && method === 'PUT') {
      if (rejectSave) {
        rejectSave = false;
        await route.fulfill({
          status: 409,
          contentType: 'application/json',
          body: JSON.stringify({ error: 'Fixture save conflict' }),
        });
        return;
      }
      const value = route.request().postDataJSON();
      definitions[value.name] = value;
      saves++;
      body = { saved: value.name };
    } else if (path.endsWith('/full')) body = definitions[path.split('/')[3]];
    else if (path.endsWith('/graph')) body = graph(definitions[path.split('/')[3]]);
    else if (path.endsWith('/overlay')) {
      if (rejectOverlay) {
        rejectOverlay = false;
        await route.fulfill({
          status: 503,
          contentType: 'application/json',
          body: JSON.stringify({ error: 'Fixture refresh outage' }),
        });
        return;
      }
      const run = runs.find((r) => r.run_id === path.split('/')[5]);
      // Delay the first run so quick-switching has to reject its stale response.
      if (run.run_id === 'review-run') await new Promise((resolve) => setTimeout(resolve, 250));
      body = overlay(run);
    } else if (path.endsWith('/decide')) {
      assert.equal(path.split('/')[5], 'review-run');
      assert.equal(route.request().postDataJSON(), 'approve');
      decisions++;
      const run = runs[0];
      run.status = 'running';
      run.current_step = 3;
      body = overlay(run);
    } else if (path.endsWith('/cancel')) {
      const run = runs.find((r) => r.run_id === path.split('/')[5]);
      run.status = 'cancel_requested';
      rejectOverlay = true;
      body = { run, status: run.status, already_terminal: false };
    } else if (path.endsWith('/run')) {
      const run = {
        ...runs[0],
        run_id: 'manual-run',
        sop_name: path.split('/')[3],
        status: 'running',
        current_step: 1,
      };
      runs.push(run);
      body = { run_id: run.run_id };
    } else if (path === '/api/sops/wire-draft') {
      const value = route.request().postDataJSON();
      body = { sop: value.sop, graph: graph(value.sop) };
    }
    await route.fulfill({ contentType: 'application/json', body: JSON.stringify(body) });
  });
  await context.routeWebSocket('**/ws/code', (ws) => {
    sockets++;
    let sid = `helper-${sockets}`;
    const reply = (id, result) => ws.send(JSON.stringify({ jsonrpc: '2.0', id, result }));
    ws.onMessage((raw) => {
      const frame = JSON.parse(raw);
      if (frame.method === 'initialize') reply(frame.id, { protocol_version: 1 });
      else if (frame.method === 'session/list-acp') reply(frame.id, { sessions: [] });
      else if (frame.method === 'session/new')
        reply(frame.id, { session_id: sid, agent_alias: 'reviewer' });
      else if (frame.method === 'session/prompt') {
        prompts++;
        assert.ok(
          frame.params.prompt.includes('Treat the following JSON as the current draft data'),
        );
        const proposal = sop('New workflow', 'Drafted with the helper');
        proposal.steps = proposal.steps.map((step) => ({ ...step, number: step.number + 10 }));
        ws.send(
          JSON.stringify({
            jsonrpc: '2.0',
            method: 'session/update',
            params: {
              session_id: sid,
              type: 'turn_complete',
              client_turn_generation: frame.params.client_turn_generation,
              content:
                'Here is the proposed workflow.\n```json\n' +
                JSON.stringify(proposal, null, 2) +
                '\n```',
            },
          }),
        );
        reply(frame.id, {});
      } else if (frame.method === 'session/state') reply(frame.id, { state: 'idle' });
      else if (frame.method === 'session/messages') reply(frame.id, { messages: [] });
    });
  });
  page = await context.newPage();
  page.setDefaultTimeout(10000);
  const errors = [];
  page.on('pageerror', (e) => {
    errors.push(e.message);
    console.error('Browser error:', e.message);
  });
  page.on('dialog', (dialog) => dialog.accept());
  await page.goto(appUrl + '/sops/PR%20review');
  await page.getByRole('heading', { name: 'PR review', exact: true }).waitFor();
  await page
    .locator('svg text')
    .filter({ hasText: /^Review changes$/ })
    .waitFor();
  assert.equal(prompts, 0);
  await page.getByRole('button', { name: 'Fit view', exact: true }).click();
  await page.screenshot({ path: out + '/sop-canvas.png', fullPage: true });
  await page
    .locator('svg text')
    .filter({ hasText: /^Review changes$/ })
    .click();
  await page.getByRole('tab', { name: 'Node', exact: true }).waitFor();
  await page.getByPlaceholder('Step title').fill('Inspect pull request');
  await page.getByRole('button', { name: 'Toggle SOP library' }).click();
  await page.getByRole('button', { name: 'Toggle SOP library' }).click();
  await page.getByRole('button', { name: /^Release checks Prepare/ }).click();
  await page.getByRole('button', { name: /^PR review Review changes/ }).click();
  assert.equal(await page.getByPlaceholder('Step title').inputValue(), 'Inspect pull request');
  assert.equal(saves, 0);
  await page.reload();
  await page.getByRole('button', { name: 'SOP settings', exact: true }).click();
  await page.getByRole('combobox', { name: 'Inspect', exact: true }).selectOption('step:1');
  assert.equal(await page.getByPlaceholder('Step title').inputValue(), 'Inspect pull request');
  rejectSave = true;
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByRole('alert').filter({ hasText: 'Fixture save conflict' }).waitFor();
  assert.equal(saves, 0);
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByText('Saved definition', { exact: true }).filter({ visible: true }).waitFor();
  assert.equal(saves, 1);
  assert.equal(definitions['PR review'].steps[0].title, 'Inspect pull request');
  // At 60% scale, a 60px pointer movement must persist as 100 graph units.
  await page.getByRole('button', { name: 'Reset zoom to 100%' }).click();
  await page.getByRole('button', { name: 'Zoom out', exact: true }).click();
  await page.getByRole('button', { name: 'Zoom out', exact: true }).click();
  const nodeTitle = page.locator('svg text').filter({ hasText: /^Inspect pull request$/ });
  const position = await nodeTitle.boundingBox();
  await page.mouse.move(position.x + 4, position.y + 4);
  await page.mouse.down();
  await page.mouse.move(position.x + 64, position.y + 34, { steps: 5 });
  await page.mouse.up();
  await page.waitForResponse((response) => response.url().endsWith('/api/sops/graph-draft'));
  const moved = projectedDraft.steps[0].pos;
  assert.ok(Math.abs(moved.x - 464) < 2, JSON.stringify(moved));
  assert.ok(Math.abs(moved.y - 74) < 2, JSON.stringify(moved));
  await page.getByRole('button', { name: 'Undo', exact: true }).click();
  await page.getByText('Saved definition', { exact: true }).filter({ visible: true }).waitFor();
  await page.getByRole('button', { name: 'Fit view', exact: true }).click();
  await page.getByText('Add node', { exact: true }).click();
  await page.getByRole('button', { name: /^Human approval Pause/ }).click();
  await page.getByPlaceholder('Step title').fill('Final approval');
  await page
    .locator('svg text')
    .filter({ hasText: /^Final approval$/ })
    .waitFor();
  await page.getByRole('button', { name: 'Fit view', exact: true }).click();
  await page.screenshot({ path: out + '/sop-node-inspector.png', fullPage: true });
  await page.getByRole('button', { name: 'Advanced / Source', exact: true }).click();
  const sourceDialog = page.getByRole('dialog', { name: 'Advanced / Source' });
  await sourceDialog.waitFor();
  await sourceDialog.locator('.cm-content').fill('{');
  await sourceDialog.getByRole('button', { name: 'Apply to draft', exact: true }).click();
  await sourceDialog.getByRole('alert').waitFor();
  await page.keyboard.press('Escape');
  assert.equal(await page.getByRole('button', { name: 'Save', exact: true }).isDisabled(), true);
  await page.getByRole('button', { name: 'Advanced / Source', exact: true }).click();
  await sourceDialog.getByRole('button', { name: 'Reset', exact: true }).click();
  await page.keyboard.press('Escape');
  await page.getByRole('button', { name: 'Reset to saved' }).click();
  await page.getByText('Saved definition', { exact: true }).filter({ visible: true }).waitFor();
  await page.getByText('Add node', { exact: true }).click();
  assert.equal(await page.getByRole('button', { name: /^Wait / }).count(), 0);
  await page.getByText('Add node', { exact: true }).click();
  await page.getByRole('button', { name: 'SOP settings', exact: true }).click();
  await page
    .getByRole('combobox', { name: 'Execution mode', exact: true })
    .selectOption('deterministic');
  await page.getByText('Add node', { exact: true }).click();
  await page.getByRole('button', { name: /^Wait Wait for/ }).click();
  assert.equal(await page.getByLabel('Capability', { exact: true }).inputValue(), 'wait');
  await page.getByRole('button', { name: 'Reset to saved', exact: true }).click();
  await page.getByText('Saved definition', { exact: true }).filter({ visible: true }).waitFor();
  await page.getByRole('button', { name: /^Runs 2$/ }).click();
  await page.getByRole('button', { name: /^PR review paused/ }).click();
  await page.getByRole('button', { name: /^Release checks running/ }).click();
  await page.getByRole('heading', { name: 'Release checks', exact: true }).waitFor();
  await page.getByRole('button', { name: /^Runs 2$/ }).click();
  await page.getByRole('button', { name: /^PR review paused/ }).click();
  await page.getByRole('button', { name: 'Approve', exact: true }).waitFor();
  await page.getByRole('button', { name: 'Fit view', exact: true }).click();
  await page.screenshot({ path: out + '/sop-run-approval.png', fullPage: true });
  await page.getByRole('button', { name: 'Approve', exact: true }).click();
  await page.getByText('Step 3/3', { exact: true }).waitFor();
  assert.equal(decisions, 1);
  await page.getByRole('button', { name: 'Stop', exact: true }).click();
  await page.getByRole('button', { name: 'Stopping…', exact: true }).waitFor();
  await page.getByRole('alert').filter({ hasText: 'refresh' }).waitFor();
  await page.getByRole('button', { name: 'New SOP', exact: true }).click();
  await page.getByRole('textbox', { name: 'Describe your SOP or ask for a change…' }).waitFor();
  assert.equal(prompts, 0);
  await page
    .getByRole('textbox', { name: 'Describe your SOP or ask for a change…' })
    .fill('Create a PR review workflow.');
  await page.getByRole('button', { name: 'Send', exact: true }).click();
  await page.getByRole('button', { name: 'Apply to draft', exact: true }).waitFor();
  rejectGraph = true;
  await page.getByRole('button', { name: 'Apply to draft', exact: true }).click();
  await page.getByRole('alert').filter({ hasText: 'Invalid fixture routing' }).waitFor();
  assert.equal(saves, 1);
  await page.getByRole('button', { name: 'Apply to draft', exact: true }).click();
  await page.getByRole('heading', { name: 'New workflow', exact: true }).waitFor();
  assert.equal(saves, 1, 'applying the proposal must not save it');
  const beforeSaveSockets = sockets;
  await page.getByRole('button', { name: 'Save', exact: true }).click();
  await page.getByText('Saved definition', { exact: true }).filter({ visible: true }).waitFor();
  assert.equal(saves, 2);
  assert.equal(sockets, beforeSaveSockets, 'saving a new SOP must retain its helper');
  await page.getByRole('button', { name: 'Apply to draft', exact: true }).waitFor();
  await page.getByRole('button', { name: 'Fit view', exact: true }).click();
  await page.waitForFunction(() => {
    const title = [...document.querySelectorAll('svg text')].find((node) => node.textContent === 'Review changes' && node.getBoundingClientRect().width > 0);
    return title?.parentElement.querySelector('text')?.textContent === '1';
  });
  await page.getByRole('button', {name: 'Fit view', exact: true}).click();
  await page.screenshot({ path: out + '/sop-helper.png', fullPage: true });
  await page.getByRole('tab', { name: 'Runs', exact: true }).click();
  await page.keyboard.press('ArrowRight');
  assert.equal(
    await page.getByRole('tab', { name: 'Node', exact: true }).getAttribute('aria-selected'),
    'true',
  );
  await page.getByRole('combobox', { name: 'Inspect', exact: true }).selectOption('step:1');
  assert.equal(
    await page.getByPlaceholder('Step title').inputValue(),
    'Review changes',
    'save reloads normalized step ordinals',
  );
  await page.getByRole('button', { name: 'Close SOP panel' }).click();
  for (const width of [390, 320]) {
    await page.setViewportSize({ width, height: 844 });
    const closeLibrary = page.getByRole('button', { name: 'Close SOP library', exact: true });
    if (await closeLibrary.count()) await closeLibrary.last().click();
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
      true,
    );
    await page.getByRole('button', { name: 'Toggle SOP panel' }).click();
    assert.equal(
      await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth),
      true,
    );
    await page.screenshot({ path: out + `/sop-mobile-${width}.png`, fullPage: true });
    await page.getByRole('button', { name: 'Close SOP panel', exact: true }).last().click();
  }
  assert.deepEqual(errors, []);
  await browser.close();
  console.log(JSON.stringify({ passed: true, saves, prompts, decisions, sockets, evidence: out }));
})().catch(async (error) => {
  console.error(error);
  if (page) console.error(await page.locator('body').ariaSnapshot());
  if (page) {
    await page.screenshot({ path: out + '/failure.png', fullPage: true });
    fs.writeFileSync(out + '/failure.txt', await page.locator('body').innerText());
  }
  await browser?.close();
  process.exitCode = 1;
});
