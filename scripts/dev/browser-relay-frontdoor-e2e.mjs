#!/usr/bin/env node
import { spawn } from 'node:child_process';
import { randomBytes, createHash } from 'node:crypto';
import fs from 'node:fs';
import fsp from 'node:fs/promises';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import process from 'node:process';
import { URL } from 'node:url';

const DEFAULT_TIMEOUT_MS = 120_000;

function usage() {
  return `Usage:
  scripts/dev/browser-relay-frontdoor-e2e.mjs \\
    --url https://127.0.0.1:8459/ \\
    --node-id testbed-daemon \\
    --pairing-code 123456

Options:
  --browser <path>       Chrome/Chromium binary. Defaults to common PATH names.
  --profile-dir <path>   Browser user-data dir. Defaults to a temp dir.
  --headed               Run a visible browser instead of headless.
  --timeout-ms <ms>      Overall UI wait timeout. Default: ${DEFAULT_TIMEOUT_MS}.
  -h, --help             Show this help.

Environment:
  ZC_BROWSER_E2E_URL
  ZC_BROWSER_E2E_NODE_ID
  ZC_BROWSER_E2E_PAIRING_CODE
  ZC_BROWSER_E2E_BROWSER
  ZC_BROWSER_E2E_PROFILE_DIR
  ZC_BROWSER_E2E_HEADED=1
  ZC_BROWSER_E2E_TIMEOUT_MS`;
}

function parseArgs(argv) {
  const opts = {
    url: process.env.ZC_BROWSER_E2E_URL || '',
    nodeId: process.env.ZC_BROWSER_E2E_NODE_ID || '',
    pairingCode: process.env.ZC_BROWSER_E2E_PAIRING_CODE || '',
    browser: process.env.ZC_BROWSER_E2E_BROWSER || process.env.BROWSER || '',
    profileDir: process.env.ZC_BROWSER_E2E_PROFILE_DIR || '',
    headed: process.env.ZC_BROWSER_E2E_HEADED === '1',
    timeoutMs: Number(process.env.ZC_BROWSER_E2E_TIMEOUT_MS || DEFAULT_TIMEOUT_MS)
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '-h' || arg === '--help') {
      console.log(usage());
      process.exit(0);
    } else if (arg === '--url') {
      opts.url = needValue(argv, ++i, arg);
    } else if (arg === '--node-id') {
      opts.nodeId = needValue(argv, ++i, arg);
    } else if (arg === '--pairing-code') {
      opts.pairingCode = needValue(argv, ++i, arg);
    } else if (arg === '--browser') {
      opts.browser = needValue(argv, ++i, arg);
    } else if (arg === '--profile-dir') {
      opts.profileDir = needValue(argv, ++i, arg);
    } else if (arg === '--headed') {
      opts.headed = true;
    } else if (arg === '--timeout-ms') {
      opts.timeoutMs = Number(needValue(argv, ++i, arg));
    } else {
      throw new Error(`unknown argument: ${arg}\n\n${usage()}`);
    }
  }

  if (!opts.url) {
    throw new Error(`missing --url\n\n${usage()}`);
  }
  if (!opts.nodeId) {
    throw new Error(`missing --node-id\n\n${usage()}`);
  }
  if (!opts.pairingCode) {
    throw new Error(`missing --pairing-code\n\n${usage()}`);
  }
  if (!Number.isFinite(opts.timeoutMs) || opts.timeoutMs <= 0) {
    throw new Error('--timeout-ms must be a positive number');
  }

  return opts;
}

function needValue(argv, index, flag) {
  const value = argv[index];
  if (!value || value.startsWith('--')) {
    throw new Error(`${flag} requires a value`);
  }
  return value;
}

function findBrowser(explicit) {
  const candidates = [
    explicit,
    process.env.CHROME,
    process.env.CHROMIUM,
    'chromium-shell',
    'chromium',
    'chromium-browser',
    'google-chrome',
    'google-chrome-stable',
    'chrome'
  ].filter(Boolean);

  for (const candidate of candidates) {
    const resolved = candidate.includes(path.sep) ? candidate : which(candidate);
    if (resolved && isExecutable(resolved)) {
      return resolved;
    }
  }

  throw new Error(
    'Chrome/Chromium was not found. Install Chromium or pass --browser /path/to/chrome.'
  );
}

function which(bin) {
  for (const dir of (process.env.PATH || '').split(path.delimiter)) {
    if (!dir) {
      continue;
    }
    const candidate = path.join(dir, bin);
    if (isExecutable(candidate)) {
      return candidate;
    }
  }
  return null;
}

function isExecutable(file) {
  try {
    fs.accessSync(file, fs.constants.X_OK);
    return true;
  } catch (_) {
    return false;
  }
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  const browser = findBrowser(opts.browser);
  const profile = opts.profileDir || await fsp.mkdtemp(path.join(os.tmpdir(), 'zc-browser-e2e-'));
  const removeProfile = !opts.profileDir;
  let browserProcess = null;
  let cdp = null;

  try {
    browserProcess = await startBrowser(browser, profile, opts.headed, opts.timeoutMs);
    const target = await createTarget(browserProcess.port);
    cdp = await Cdp.connect(target.webSocketDebuggerUrl);
    await preparePage(cdp);
    await navigate(cdp, opts.url, opts.timeoutMs);
    await ensureServiceWorker(cdp, opts.timeoutMs);
    await submitPairing(cdp, opts.nodeId, opts.pairingCode);
    const sas = await waitForSas(cdp, opts.timeoutMs);
    await click(cdp, 'sas-confirm');
    const ready = await waitForReady(cdp, opts.timeoutMs);

    console.log('browser relay frontdoor e2e ok');
    console.log(`  url: ${opts.url}`);
    console.log(`  node: ${opts.nodeId}`);
    console.log(`  sas: ${sas}`);
    console.log(`  status: ${ready.status}`);
  } finally {
    if (cdp) {
      await cdp.send('Browser.close', {}, 2000).catch(() => {});
      cdp.close();
      cdp = null;
    }
    await stopBrowser(browserProcess);
    if (removeProfile) {
      await fsp.rm(profile, { recursive: true, force: true });
    }
  }
}

async function startBrowser(browser, profile, headed, timeoutMs) {
  await fsp.mkdir(profile, { recursive: true });
  const args = [
    `--user-data-dir=${profile}`,
    '--remote-debugging-port=0',
    '--no-first-run',
    '--no-default-browser-check',
    '--disable-background-networking',
    '--disable-popup-blocking',
    '--disable-sync',
    '--disable-dev-shm-usage',
    '--ignore-certificate-errors',
    '--allow-insecure-localhost',
    '--disable-features=DialMediaRouteProvider'
  ];
  if (!headed) {
    args.push('--headless=new');
  }
  if (typeof process.getuid === 'function' && process.getuid() === 0) {
    args.push('--no-sandbox');
  }
  args.push('about:blank');

  const child = spawn(browser, args, {
    detached: true,
    stdio: ['ignore', 'ignore', 'pipe']
  });
  let stderr = '';
  child.stderr.on('data', (chunk) => {
    stderr += chunk.toString();
    if (stderr.length > 16_384) {
      stderr = stderr.slice(-16_384);
    }
  });

  const activePort = path.join(profile, 'DevToolsActivePort');
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (child.exitCode !== null) {
      throw new Error(`browser exited early (${child.exitCode})\n${stderr.trim()}`);
    }
    if (fs.existsSync(activePort)) {
      const [portLine] = fs.readFileSync(activePort, 'utf8').split(/\r?\n/);
      const port = Number(portLine);
      if (Number.isInteger(port) && port > 0) {
        return { child, port };
      }
    }
    await delay(100);
  }
  throw new Error(`timed out waiting for Chrome DevToolsActivePort\n${stderr.trim()}`);
}

async function stopBrowser(browserProcess) {
  const child = browserProcess?.child;
  if (!child) {
    return;
  }

  const signalBrowserGroup = (signal) => {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch (_) {
      // The process group may already be gone if Chromium re-parented itself.
    }
    try {
      child.kill(signal);
    } catch (_) {
      // Nothing left to stop.
    }
  };

  signalBrowserGroup('SIGTERM');
  if (child.exitCode !== null) {
    await delay(250);
    signalBrowserGroup('SIGKILL');
    return;
  }

  const exited = new Promise((resolve) => child.once('exit', resolve));
  const timedOut = delay(2000).then(() => 'timeout');
  if (await Promise.race([exited, timedOut]) === 'timeout') {
    signalBrowserGroup('SIGKILL');
    await Promise.race([exited, delay(1000)]);
  }
}

async function createTarget(port) {
  const endpoint = `http://127.0.0.1:${port}/json/new?${encodeURIComponent('about:blank')}`;
  let response = await fetch(endpoint, { method: 'PUT' });
  if (response.status === 405) {
    response = await fetch(endpoint);
  }
  if (!response.ok) {
    throw new Error(`create DevTools target failed: HTTP ${response.status}`);
  }
  const target = await response.json();
  if (!target.webSocketDebuggerUrl) {
    throw new Error('DevTools target did not return a websocket URL');
  }
  return target;
}

async function preparePage(cdp) {
  await cdp.send('Page.enable');
  await cdp.send('Runtime.enable');
  await cdp.send('Log.enable').catch(() => {});
  await cdp.send('Security.enable').catch(() => {});
  await cdp.send('Security.setIgnoreCertificateErrors', { ignore: true }).catch(() => {});
}

async function navigate(cdp, url, timeoutMs) {
  const load = cdp.waitForEvent('Page.loadEventFired', () => true, timeoutMs).catch(() => null);
  await cdp.send('Page.navigate', { url });
  await load;
  await waitForEval(cdp, `
    (() => ({
      ok: document.readyState === 'complete',
      readyState: document.readyState,
      title: document.title
    }))()
  `, timeoutMs, 'page load');
}

async function ensureServiceWorker(cdp, timeoutMs) {
  for (let attempt = 0; attempt < 4; attempt += 1) {
    const state = await cdp.evaluate(`
      (async () => {
        if (!('serviceWorker' in navigator)) {
          return { ok: false, reason: 'service workers are unavailable' };
        }
        const registration = await navigator.serviceWorker.ready;
        const activeState = registration.active ? registration.active.state : null;
        return {
          ok: true,
          controlled: Boolean(navigator.serviceWorker.controller),
          activeState,
          scope: registration.scope
        };
      })()
    `, timeoutMs);

    if (!state.ok) {
      throw new Error(state.reason || 'service worker unavailable');
    }
    if (state.controlled) {
      break;
    }

    await cdp.evaluate(`
      new Promise((resolve) => {
        if (navigator.serviceWorker.controller) {
          resolve(true);
          return;
        }
        const timeout = setTimeout(() => resolve(false), 1000);
        navigator.serviceWorker.addEventListener('controllerchange', () => {
          clearTimeout(timeout);
          resolve(true);
        }, { once: true });
      })
    `, timeoutMs).catch(() => false);

    const afterControllerChange = await cdp.evaluate(`
      (() => Boolean(navigator.serviceWorker.controller))()
    `).catch(() => false);
    if (afterControllerChange) {
      break;
    }

    const load = cdp.waitForEvent('Page.loadEventFired', () => true, timeoutMs).catch(() => null);
    await cdp.send('Page.reload', { ignoreCache: true });
    await load;
    await waitForEval(cdp, `
      (() => ({
        ok: document.readyState === 'complete',
        readyState: document.readyState
      }))()
    `, timeoutMs, 'service worker reload');
  }

  await waitForEval(cdp, `
    (async () => {
      if (!navigator.serviceWorker.controller) {
        return { ok: false, reason: 'service worker does not control the page yet' };
      }
      const response = await fetch('/__zeroclaw/tunnel/state');
      const body = await response.json().catch(() => ({}));
      return {
        ok: response.ok && body.ready === true && body.enrollmentTls === true && body.mtlsEngine === true,
        status: response.status,
        body
      };
    })()
  `, timeoutMs, 'service worker tunnel state');
}

async function submitPairing(cdp, nodeId, pairingCode) {
  await cdp.evaluate(`
    (() => {
      const node = document.getElementById('server-id');
      const code = document.getElementById('pairing-code');
      node.value = ${JSON.stringify(nodeId)};
      code.value = ${JSON.stringify(pairingCode)};
      node.dispatchEvent(new Event('input', { bubbles: true }));
      code.dispatchEvent(new Event('input', { bubbles: true }));
      document.getElementById('pair').requestSubmit();
      return true;
    })()
  `);
}

async function waitForSas(cdp, timeoutMs) {
  const result = await waitForEval(cdp, `
    (() => {
      const status = document.getElementById('status')?.textContent || '';
      const panel = document.getElementById('sas-panel');
      const sas = document.getElementById('sas-code')?.textContent?.trim() || '';
      return {
        ok: Boolean(panel && !panel.hidden && /^[A-F0-9]{4}-[A-F0-9]{4}$/.test(sas)),
        status,
        sas
      };
    })()
  `, timeoutMs, 'SAS prompt');
  return result.sas;
}

async function click(cdp, id) {
  await cdp.evaluate(`
    (() => {
      const button = document.getElementById(${JSON.stringify(id)});
      if (!button) {
        throw new Error('missing button ${id}');
      }
      button.click();
      return true;
    })()
  `);
}

async function waitForReady(cdp, timeoutMs) {
  return waitForEval(cdp, `
    (() => {
      const status = document.getElementById('status')?.textContent || '';
      const chat = document.getElementById('chat-panel');
      const send = document.getElementById('send');
      return {
        ok: Boolean(chat && !chat.hidden && send && !send.disabled && status.includes('Secure tunnel ready')),
        status,
        chatHidden: chat ? chat.hidden : null,
        sendDisabled: send ? send.disabled : null
      };
    })()
  `, timeoutMs, 'secure tunnel ready');
}

async function waitForEval(cdp, expression, timeoutMs, label) {
  const deadline = Date.now() + timeoutMs;
  let last = null;
  while (Date.now() < deadline) {
    last = await cdp.evaluate(expression).catch((error) => ({
      ok: false,
      error: error.message
    }));
    if (last?.ok) {
      return last;
    }
    await delay(250);
  }
  throw new Error(`${label} timed out; last state: ${JSON.stringify(last)}`);
}

class Cdp {
  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    this.eventWaiters = [];
    this.lastException = null;
    ws.onMessage((text) => this.handleMessage(text));
    ws.onClose(() => {
      for (const pending of this.pending.values()) {
        pending.reject(new Error('DevTools websocket closed'));
      }
      this.pending.clear();
    });
  }

  static async connect(wsUrl) {
    return new Cdp(await RawWebSocket.connect(wsUrl));
  }

  send(method, params = {}, timeoutMs = 30_000) {
    const id = this.nextId++;
    const payload = JSON.stringify({ id, method, params });
    return new Promise((resolve, reject) => {
      const timeout = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`${method} timed out`));
      }, timeoutMs);
      this.pending.set(id, {
        resolve: (value) => {
          clearTimeout(timeout);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timeout);
          reject(error);
        }
      });
      this.ws.sendText(payload);
    });
  }

  async evaluate(expression, timeoutMs = 30_000) {
    const response = await this.send('Runtime.evaluate', {
      expression,
      awaitPromise: true,
      returnByValue: true,
      userGesture: true
    }, timeoutMs);
    if (response.exceptionDetails) {
      const details = response.exceptionDetails;
      const message = details.exception?.description || details.text || 'evaluation failed';
      throw new Error(message);
    }
    return response.result?.value;
  }

  waitForEvent(method, predicate, timeoutMs) {
    return new Promise((resolve, reject) => {
      const waiterRef = {
        method,
        predicate,
        resolve: (value) => {
          clearTimeout(timeout);
          resolve(value);
        },
        reject: (error) => {
          clearTimeout(timeout);
          reject(error);
        }
      };
      const timeout = setTimeout(() => {
        this.eventWaiters = this.eventWaiters.filter((waiter) => waiter !== waiterRef);
        reject(new Error(`${method} event timed out`));
      }, timeoutMs);
      this.eventWaiters.push(waiterRef);
    });
  }

  handleMessage(text) {
    let msg = null;
    try {
      msg = JSON.parse(text);
    } catch (_) {
      return;
    }
    if (msg.id) {
      const pending = this.pending.get(msg.id);
      if (!pending) {
        return;
      }
      this.pending.delete(msg.id);
      if (msg.error) {
        pending.reject(new Error(msg.error.message || 'CDP command failed'));
      } else {
        pending.resolve(msg.result || {});
      }
      return;
    }
    if (msg.method === 'Runtime.exceptionThrown') {
      this.lastException = msg.params;
    }
    if (msg.method) {
      for (const waiter of [...this.eventWaiters]) {
        if (waiter.method !== msg.method) {
          continue;
        }
        try {
          if (!waiter.predicate || waiter.predicate(msg.params || {})) {
            this.eventWaiters = this.eventWaiters.filter((item) => item !== waiter);
            waiter.resolve(msg.params || {});
          }
        } catch (error) {
          this.eventWaiters = this.eventWaiters.filter((item) => item !== waiter);
          waiter.reject(error);
        }
      }
    }
  }

  close() {
    for (const pending of this.pending.values()) {
      pending.reject(new Error('DevTools websocket closed'));
    }
    this.pending.clear();
    this.ws.close();
  }
}

class RawWebSocket {
  constructor(socket) {
    this.socket = socket;
    this.buffer = Buffer.alloc(0);
    this.messageHandlers = [];
    this.closeHandlers = [];
    this.closed = false;
    socket.on('data', (chunk) => this.feed(chunk));
    socket.on('close', () => this.handleClose());
    socket.on('error', () => this.handleClose());
  }

  static async connect(wsUrl) {
    const url = new URL(wsUrl);
    if (url.protocol !== 'ws:') {
      throw new Error(`unsupported DevTools websocket URL: ${wsUrl}`);
    }
    const key = randomBytes(16).toString('base64');
    const socket = net.createConnection({
      host: url.hostname,
      port: Number(url.port || 80)
    });

    const pathWithQuery = `${url.pathname}${url.search}`;
    const request = [
      `GET ${pathWithQuery} HTTP/1.1`,
      `Host: ${url.host}`,
      'Upgrade: websocket',
      'Connection: Upgrade',
      `Sec-WebSocket-Key: ${key}`,
      'Sec-WebSocket-Version: 13',
      '\r\n'
    ].join('\r\n');

    await new Promise((resolve, reject) => {
      socket.once('connect', resolve);
      socket.once('error', reject);
    });
    socket.write(request);

    const { head, rest } = await readHttpUpgrade(socket);
    if (!/^HTTP\/1\.1 101\b/i.test(head)) {
      throw new Error(`DevTools websocket upgrade failed:\n${head}`);
    }
    const expectedAccept = createHash('sha1')
      .update(`${key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11`)
      .digest('base64');
    if (!head.toLowerCase().includes(`sec-websocket-accept: ${expectedAccept}`.toLowerCase())) {
      throw new Error('DevTools websocket accept header did not match');
    }

    const ws = new RawWebSocket(socket);
    if (rest.length > 0) {
      ws.feed(rest);
    }
    return ws;
  }

  onMessage(handler) {
    this.messageHandlers.push(handler);
  }

  onClose(handler) {
    this.closeHandlers.push(handler);
  }

  sendText(text) {
    if (this.closed) {
      throw new Error('websocket is closed');
    }
    const payload = Buffer.from(text);
    let header = null;
    if (payload.length < 126) {
      header = Buffer.alloc(2);
      header[0] = 0x81;
      header[1] = 0x80 | payload.length;
    } else if (payload.length <= 0xffff) {
      header = Buffer.alloc(4);
      header[0] = 0x81;
      header[1] = 0x80 | 126;
      header.writeUInt16BE(payload.length, 2);
    } else {
      header = Buffer.alloc(10);
      header[0] = 0x81;
      header[1] = 0x80 | 127;
      header.writeBigUInt64BE(BigInt(payload.length), 2);
    }
    const mask = randomBytes(4);
    const masked = Buffer.alloc(payload.length);
    for (let i = 0; i < payload.length; i += 1) {
      masked[i] = payload[i] ^ mask[i % 4];
    }
    this.socket.write(Buffer.concat([header, mask, masked]));
  }

  feed(chunk) {
    this.buffer = Buffer.concat([this.buffer, chunk]);
    while (this.buffer.length >= 2) {
      const first = this.buffer[0];
      const second = this.buffer[1];
      const opcode = first & 0x0f;
      const masked = Boolean(second & 0x80);
      let length = second & 0x7f;
      let offset = 2;
      if (length === 126) {
        if (this.buffer.length < offset + 2) {
          return;
        }
        length = this.buffer.readUInt16BE(offset);
        offset += 2;
      } else if (length === 127) {
        if (this.buffer.length < offset + 8) {
          return;
        }
        const wideLength = this.buffer.readBigUInt64BE(offset);
        if (wideLength > BigInt(Number.MAX_SAFE_INTEGER)) {
          throw new Error('websocket frame too large');
        }
        length = Number(wideLength);
        offset += 8;
      }
      let mask = null;
      if (masked) {
        if (this.buffer.length < offset + 4) {
          return;
        }
        mask = this.buffer.subarray(offset, offset + 4);
        offset += 4;
      }
      if (this.buffer.length < offset + length) {
        return;
      }
      let payload = this.buffer.subarray(offset, offset + length);
      this.buffer = this.buffer.subarray(offset + length);
      if (masked && mask) {
        const unmasked = Buffer.alloc(payload.length);
        for (let i = 0; i < payload.length; i += 1) {
          unmasked[i] = payload[i] ^ mask[i % 4];
        }
        payload = unmasked;
      }
      if (opcode === 0x1) {
        const text = payload.toString('utf8');
        for (const handler of this.messageHandlers) {
          handler(text);
        }
      } else if (opcode === 0x8) {
        this.close();
      } else if (opcode === 0x9) {
        this.sendPong(payload);
      }
    }
  }

  sendPong(payload) {
    if (this.closed) {
      return;
    }
    const header = Buffer.from([0x8a, 0x80 | payload.length]);
    const mask = randomBytes(4);
    const masked = Buffer.alloc(payload.length);
    for (let i = 0; i < payload.length; i += 1) {
      masked[i] = payload[i] ^ mask[i % 4];
    }
    this.socket.write(Buffer.concat([header, mask, masked]));
  }

  close() {
    if (this.closed) {
      return;
    }
    this.closed = true;
    this.socket.end();
    this.socket.destroy();
    this.handleClose();
  }

  handleClose() {
    if (!this.closed) {
      this.closed = true;
    }
    for (const handler of this.closeHandlers.splice(0)) {
      handler();
    }
  }
}

function readHttpUpgrade(socket) {
  return new Promise((resolve, reject) => {
    let buffer = Buffer.alloc(0);
    const onData = (chunk) => {
      buffer = Buffer.concat([buffer, chunk]);
      const split = buffer.indexOf('\r\n\r\n');
      if (split < 0) {
        return;
      }
      socket.off('data', onData);
      socket.off('error', reject);
      const head = buffer.subarray(0, split + 4).toString('utf8');
      const rest = buffer.subarray(split + 4);
      resolve({ head, rest });
    };
    socket.on('data', onData);
    socket.once('error', reject);
  });
}

function delay(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}

main().then(() => {
  process.exit(0);
}).catch((error) => {
  console.error(`browser relay frontdoor e2e failed: ${error.message}`);
  process.exit(1);
});
