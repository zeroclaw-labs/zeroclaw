#!/usr/bin/env node
// Private stdin lease. No debugging endpoint, generic script, screenshot,
// storage export, trace, video, profile or page-content protocol is exposed.
import { chromium } from 'playwright-core';
import { createInterface } from 'node:readline';
import { pathToFileURL } from 'node:url';
import { readFileSync } from 'node:fs';
import { isAbsolute } from 'node:path';

// One canonical checked-in form program, loaded only from the trusted install.
// No script body is accepted from the caller, including in the open message.
const FORM_SOURCE = readFileSync(new URL('../../crates/zeroclaw-tools/src/browser_credential_form.js', import.meta.url), 'utf8');

export class ManagedBrowser {
  #browser;
  #context;
  #page;
  #origin;
  #launch;
  #launching;
  #closed = false;

  constructor(launch = options => chromium.launch(options)) { this.#launch = launch; }

  async command(request) {
    if (this.#closed) throw new Error('closed');
    switch (request.op) {
      case 'open': {
        if (this.#browser) throw new Error('already open');
        const url = new URL(request.url);
        if (url.protocol !== 'https:' || url.username || url.password || url.hash ||
            (request.executable != null && (typeof request.executable !== 'string' || !isAbsolute(request.executable)))) throw new Error('invalid');
        this.#origin = url.origin;
        // Always headless with the Chromium sandbox enabled. No attach/CDP,
        // existing profile, extensions, downloads, recordVideo or tracing.
        this.#launching = this.#launch({
          headless: true, chromiumSandbox: true,
          timeout: 15_000,
          ...(request.executable ? { executablePath: request.executable } : {}),
          env: Object.fromEntries(Object.entries(process.env).filter(([key]) =>
            ['PATH', 'SystemRoot', 'WINDIR', 'TEMP', 'TMP', 'HOME', 'USERPROFILE', 'LOCALAPPDATA', 'XDG_RUNTIME_DIR'].includes(key))),
        });
        this.#browser = await this.#launching;
        if (this.#closed) throw new Error('closed');
        this.#context = await this.#browser.newContext({
          acceptDownloads: false, serviceWorkers: 'block', permissions: [],
          ignoreHTTPSErrors: false,
        });
        await this.#context.route('**/*', route => {
          const destination = new URL(route.request().url());
          return destination.origin === this.#origin && destination.protocol === 'https:'
            ? route.continue() : route.abort();
        });
        await this.#context.routeWebSocket('**/*', socket => socket.close());
        this.#context.on('page', page => {
          if (this.#page && page !== this.#page) void page.close();
        });
        this.#page = await this.#context.newPage();
        this.#page.setDefaultTimeout(15_000);
        await this.#page.goto(url.href, { waitUntil: 'domcontentloaded', timeout: 20_000 });
        return { ok: true };
      }
      case 'origin':
        if (!this.#page || new URL(this.#page.url()).origin !== this.#origin) throw new Error('origin');
        return { ok: true, origin: this.#origin };
      case 'submit': {
        if (!this.#page || request.payload?.origin !== this.#origin ||
            request.payload.field != null || request.payload.frame != null ||
            new URL(this.#page.url()).origin !== this.#origin) throw new Error('origin');
        const navigation = this.#page.waitForNavigation({ waitUntil: 'domcontentloaded', timeout: 15_000 })
          .then(() => true, () => false);
        const submitted = await this.#page.evaluate(`(${FORM_SOURCE})(${JSON.stringify(request.payload)})`);
        if (submitted !== true || !await navigation || new URL(this.#page.url()).origin !== this.#origin) throw new Error('unconfirmed');
        return { ok: true };
      }
      default: throw new Error('unsupported');
    }
  }

  async close() {
    if (this.#closed) return;
    this.#closed = true;
    const browser = await this.#launching?.catch(() => undefined);
    try { await this.#context?.close(); }
    finally { await browser?.close(); }
  }
}

export async function serve(input = process.stdin, output = process.stdout, createBrowser = () => new ManagedBrowser()) {
  const browser = createBrowser();
  const lines = createInterface({ input, crlfDelay: Infinity });
  // EOF must interrupt an operation, rather than waiting behind navigation.
  let closed;
  const cleanup = () => closed ??= browser.close();
  // The finally block awaits and propagates the same cleanup result.
  const beginCleanup = () => { void cleanup().catch(() => {}); };
  input.once('end', beginCleanup);
  input.once('error', beginCleanup);
  const terminate = () => { lines.close(); input.destroy(); beginCleanup(); };
  process.once('SIGTERM', terminate);
  process.once('SIGINT', terminate);
  try {
    for await (const line of lines) {
      if (Buffer.byteLength(line) > 65_536) throw new Error('size');
      try {
        const result = await browser.command(JSON.parse(line));
        output.write(JSON.stringify(result) + '\n');
      } catch {
        // No raw error, URL, DOM text, console message or supplied input.
        output.write('{"ok":false}\n');
        break;
      }
    }
  } finally {
    lines.close();
    await cleanup();
    process.removeListener('SIGTERM', terminate);
    process.removeListener('SIGINT', terminate);
  }
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  serve().catch(() => { process.exitCode = 1; });
}
