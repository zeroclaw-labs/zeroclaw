import test from 'node:test';
import assert from 'node:assert/strict';
import { PassThrough } from 'node:stream';
import { ManagedBrowser, serve } from './managed-browser.mjs';
import { chromium } from 'playwright-core';

const origin = 'https://login.example.test';
const executable = process.env.ZEROCLAW_TEST_CHROMIUM;

test('no generic observation, script, clipboard or export commands exist, even before login', async () => {
  const worker = new ManagedBrowser(() => { throw new Error('must not launch'); });
  for (const op of ['eval', 'add_init_script', 'snapshot', 'screenshot', 'get_text', 'get_url', 'cookies', 'storage_state', 'download', 'clipboard', 'trace', 'attach']) {
    await assert.rejects(worker.command({ op, script: 'capture password', session: 'other-task' }));
  }
  await worker.close();
});

test('stdin EOF closes a worker without forwarding arbitrary input or errors', async () => {
  const input = new PassThrough(), output = new PassThrough();
  let text = '';
  output.on('data', value => { text += value; });
  const done = serve(input, output);
  input.end('{"op":"eval","script":"SYNTHETIC_SECRET"}\n');
  await done;
  assert.equal(text, '{"ok":false}\n');
});

test('closing during browser startup closes the late browser too', async () => {
  let ready, closed = 0;
  const worker = new ManagedBrowser(() => new Promise(resolve => { ready = resolve; }));
  const opening = worker.command({ op: 'open', url: origin });
  const closing = worker.close();
  ready({ close: async () => { closed++; } });
  await assert.rejects(opening);
  await closing;
  assert.equal(closed, 1);
});

test('stdin EOF interrupts an in-flight operation and errors also await cleanup', async () => {
  for (const cancel of [false, true]) {
    const input = new PassThrough(), output = new PassThrough();
    let entered, stop, calls = 0, text = '';
    const started = new Promise(resolve => { entered = resolve; });
    const stopped = new Promise(resolve => { stop = resolve; });
    output.on('data', value => { text += value; });
    const done = serve(input, output, () => ({
      async command() { entered(); if (cancel) await stopped; throw new Error('synthetic-secret'); },
      async close() { calls++; stop(); },
    }));
    input.write('{"op":"open"}\n');
    await started;
    if (cancel) input.end();
    await done;
    input.destroy();
    assert.equal(calls, 1);
    assert.equal(text, '{"ok":false}\n');
  }
});

test('real Chromium: independent contexts, pre-hook refusal, secret echo suppression and cleanup', { skip: !executable, timeout: 20_000 }, async () => {
  const contexts = [], browsers = [], posts = [];
  let enteredNavigation;
  const navigationPending = new Promise(resolve => { enteredNavigation = resolve; });
  const launch = async options => {
    assert.equal(options.chromiumSandbox, true);
    assert.equal(options.headless, true);
    assert.equal(options.recordVideo, undefined);
    const browser = await chromium.launch(options);
    browsers.push(browser);
    return {
      newContext: async options => {
        assert.deepEqual(options, { acceptDownloads: false, serviceWorkers: 'block', permissions: [], ignoreHTTPSErrors: false });
        const context = await browser.newContext(options);
        contexts.push(context);
        // Network fixture only: the production helper still performs its
        // origin checks, evaluates the checked-in form and owns the context.
        const actualPage = context.newPage.bind(context);
        context.newPage = async () => {
          const page = await actualPage();
          await page.route('**/*', async route => {
            const request = route.request();
            if (new URL(request.url()).pathname === '/stall') {
              enteredNavigation();
              return new Promise(() => {});
            }
            const otp = new URL(request.url()).pathname === '/otp-only';
            if (request.method() === 'POST') {
              const fields = Object.fromEntries(new URLSearchParams(request.postData()));
              posts.push(fields);
              assert.deepEqual(fields, otp ? { otp: '123456' } : { username: 'fixture', password: 'synthetic-password' });
              await route.fulfill({ contentType: 'text/html', headers: { 'Set-Cookie': 'session=synthetic; Secure; SameSite=Strict' }, body: '<h1>synthetic-password</h1><script>localStorage.setItem("secret", "synthetic-password")</script>' });
            } else {
              assert.equal(request.headers().cookie, undefined, 'another task must never inherit the authenticated cookie');
              await route.fulfill({ contentType: 'text/html', body: otp ? '<form method="post" action="/otp-only"><input name="otp" autocomplete="one-time-code"><button>Verify</button></form>' : '<form method="post" action="/login"><input name="username" autocomplete="username"><input name="password" type="password"><button>Login</button></form>' });
            }
          });
          return page;
        };
        return context;
      },
      close: () => browser.close(),
    };
  };
  const first = new ManagedBrowser(launch), second = new ManagedBrowser(launch), otp = new ManagedBrowser(launch), pending = new ManagedBrowser(launch);
  try {
    await assert.rejects(first.command({ op: 'add_init_script', script: 'observe inputs' }));
    await first.command({ op: 'open', url: origin, executable });
    assert.deepEqual(await first.command({ op: 'submit', payload: { origin, stage: 'login', values: { username: 'fixture', password: 'synthetic-password' } } }), { ok: true });
    assert.equal((await contexts[0].cookies()).some(cookie => cookie.name === 'session'), true);
    assert.equal(await contexts[0].pages()[0].locator('h1').innerText(), 'synthetic-password');
    for (const op of ['snapshot', 'screenshot', 'eval', 'get_text', 'cookies', 'storage_state', 'clipboard', 'attach']) {
      await assert.rejects(first.command({ op }));
    }
    // There is no session id, cookie or page handle to pass to another task.
    await second.command({ op: 'open', url: origin, executable, session: 'first' });
    assert.notEqual(contexts[0], contexts[1]);
    assert.deepEqual(await contexts[1].cookies(), []);
    assert.equal(posts.length, 1);
    await otp.command({ op: 'open', url: origin + '/otp-only', executable });
    assert.deepEqual(await otp.command({ op: 'submit', payload: { origin, stage: 'otp', values: { otp: '123456' } } }), { ok: true });
    assert.equal(posts.length, 2);
    const opening = pending.command({ op: 'open', url: origin + '/stall', executable });
    const refused = assert.rejects(opening);
    await navigationPending;
    await pending.close();
    await refused;
  } finally {
    await first.close();
    await second.close();
    await otp.close();
    await pending.close();
  }
  for (const browser of browsers) assert.equal(browser.isConnected(), false);
  for (const context of contexts) assert.equal(context.pages().length, 0);
  await assert.rejects(first.command({ op: 'origin' }));
});
