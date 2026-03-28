#!/usr/bin/env node
//
// playwright-daemon.js — Persistent Playwright browser daemon for MoA.
//
// Inspired by gstack's browser architecture: runs a long-lived HTTP server
// that maintains a persistent Chromium instance with cookies, tabs, and
// login sessions across all commands.
//
// Performance: First call ~3s (browser startup), subsequent calls ~100-200ms.
//
// Usage:
//   node playwright-daemon.js [--port PORT] [--headless] [--idle-timeout SECONDS]
//
// API:
//   POST /command  — Execute browser command (JSON body)
//   GET  /health   — Health check
//   POST /shutdown — Graceful shutdown
//

const http = require("http");
const { chromium } = require("playwright");
const fs = require("fs");
const path = require("path");
const crypto = require("crypto");

// ── Configuration ──────────────────────────────────────────────
const PORT = parseInt(process.env.BROWSER_DAEMON_PORT || process.argv.find((a, i) => process.argv[i-1] === "--port") || "9500", 10);
const HEADLESS = !process.argv.includes("--headed");
const IDLE_TIMEOUT_MS = parseInt(process.env.BROWSER_IDLE_TIMEOUT || process.argv.find((a, i) => process.argv[i-1] === "--idle-timeout") || "1800", 10) * 1000;
const STATE_FILE = path.join(process.env.HOME || process.env.USERPROFILE || ".", ".zeroclaw", "browser-daemon.json");
const AUTH_TOKEN = crypto.randomBytes(16).toString("hex");

let browser = null;
let context = null;
let activePage = null;
let pages = new Map(); // tabId → page
let nextTabId = 1;
let refMap = new Map(); // @e1 → { locator, role, name }
let nextRefId = 1;
let idleTimer = null;
let startedAt = Date.now();

// ── Browser Lifecycle ──────────────────────────────────────────

async function ensureBrowser() {
  if (browser && browser.isConnected()) return;

  const downloadDir = path.join(process.env.HOME || process.env.USERPROFILE || ".", ".zeroclaw", "downloads");
  if (!fs.existsSync(downloadDir)) fs.mkdirSync(downloadDir, { recursive: true });

  browser = await chromium.launch({
    headless: HEADLESS,
    args: [
      "--disable-blink-features=AutomationControlled",
      "--no-sandbox",
      "--disable-dev-shm-usage",
    ],
  });

  context = await browser.newContext({
    userAgent: "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/130.0.0.0 Safari/537.36",
    viewport: { width: 1280, height: 720 },
    locale: "ko-KR",
    acceptDownloads: true,
  });

  activePage = await context.newPage();
  const tabId = nextTabId++;
  pages.set(tabId, activePage);

  // Clear refs on navigation (gstack pattern: stale refs should fail loudly)
  activePage.on("framenavigated", () => {
    refMap.clear();
    nextRefId = 1;
  });

  browser.on("disconnected", () => {
    console.error("[MoA Browser] Chromium disconnected, exiting daemon");
    process.exit(1);
  });

  console.error(`[MoA Browser] Chromium started (headless=${HEADLESS})`);
}

// ── @Ref System (gstack-inspired accessibility tree refs) ──────

async function buildRefMap(page) {
  refMap.clear();
  nextRefId = 1;

  // Get accessibility tree snapshot
  const snapshot = await page.accessibility.snapshot({ interestingOnly: true });
  if (!snapshot) return "Empty page — no accessible elements found.";

  const lines = [];
  function walk(node, depth) {
    const indent = "  ".repeat(depth);
    const role = node.role || "unknown";
    const name = (node.name || "").trim();

    // Assign ref to interactive elements
    const interactiveRoles = new Set([
      "button", "link", "textbox", "checkbox", "radio", "combobox",
      "menuitem", "tab", "switch", "slider", "spinbutton", "searchbox",
      "option", "menuitemcheckbox", "menuitemradio", "treeitem",
    ]);

    let refLabel = "";
    if (interactiveRoles.has(role)) {
      const refId = `@e${nextRefId++}`;
      refMap.set(refId, {
        role,
        name,
        // Build Playwright locator (gstack pattern: getByRole + nth)
        locator: page.getByRole(role, name ? { name, exact: false } : {}).first(),
      });
      refLabel = ` ${refId}`;
    }

    const nameStr = name ? ` "${name}"` : "";
    lines.push(`${indent}[${role}]${nameStr}${refLabel}`);

    if (node.children) {
      for (const child of node.children) {
        walk(child, depth + 1);
      }
    }
  }

  walk(snapshot, 0);
  return lines.join("\n");
}

async function resolveSelector(selector, page) {
  if (!selector) return null;

  // @ref resolution (gstack pattern)
  if (selector.startsWith("@e") || selector.startsWith("@c")) {
    const ref = refMap.get(selector);
    if (!ref) {
      throw new Error(
        `Ref ${selector} not found. Run snapshot to refresh element refs. ` +
        `Available refs: ${[...refMap.keys()].join(", ") || "none (run snapshot first)"}`
      );
    }
    // Staleness check (gstack pattern: async count() before use)
    const count = await ref.locator.count();
    if (count === 0) {
      throw new Error(
        `Ref ${selector} is stale (element [${ref.role}] "${ref.name}" no longer exists). ` +
        `Run snapshot to get fresh refs.`
      );
    }
    return ref.locator;
  }

  // CSS selector fallback
  return page.locator(selector);
}

// ── Command Execution ──────────────────────────────────────────

async function executeCommand(cmd) {
  await ensureBrowser();
  const p = activePage;
  const timeout = cmd.timeout_ms || 30000;

  switch (cmd.action) {
    // ── Navigation ──
    case "open":
    case "goto": {
      const resp = await p.goto(cmd.url, {
        waitUntil: cmd.wait_until || "domcontentloaded",
        timeout,
      });
      return {
        url: p.url(),
        title: await p.title(),
        status: resp?.status() || 0,
      };
    }
    case "back": await p.goBack({ timeout }); return { url: p.url() };
    case "forward": await p.goForward({ timeout }); return { url: p.url() };
    case "reload": await p.reload({ timeout }); return { url: p.url() };

    // ── Snapshot (@ref system) ──
    case "snapshot": {
      const tree = await buildRefMap(p);
      const refCount = refMap.size;
      return {
        tree,
        ref_count: refCount,
        url: p.url(),
        title: await p.title(),
      };
    }

    // ── Interaction ──
    case "click": {
      const locator = await resolveSelector(cmd.selector, p);
      await locator.click({ timeout });
      return { clicked: cmd.selector, url: p.url() };
    }
    case "fill": {
      const locator = await resolveSelector(cmd.selector, p);
      await locator.fill(cmd.value || "", { timeout });
      return { filled: cmd.selector };
    }
    case "type": {
      const locator = await resolveSelector(cmd.selector, p);
      await locator.pressSequentially(cmd.value || "", { delay: cmd.delay || 50, timeout });
      return { typed: cmd.selector };
    }
    case "press": {
      if (cmd.selector) {
        const locator = await resolveSelector(cmd.selector, p);
        await locator.press(cmd.key, { timeout });
      } else {
        await p.keyboard.press(cmd.key);
      }
      return { pressed: cmd.key };
    }
    case "hover": {
      const locator = await resolveSelector(cmd.selector, p);
      await locator.hover({ timeout });
      return { hovered: cmd.selector };
    }
    case "scroll": {
      const x = cmd.x || 0;
      const y = cmd.y || 300;
      await p.mouse.wheel(x, y);
      return { scrolled: { x, y } };
    }
    case "select": {
      const locator = await resolveSelector(cmd.selector, p);
      await locator.selectOption(cmd.value, { timeout });
      return { selected: cmd.value };
    }
    case "wait": {
      if (cmd.selector) {
        await p.waitForSelector(cmd.selector, { state: cmd.state || "visible", timeout });
      } else {
        await p.waitForTimeout(cmd.ms || 1000);
      }
      return { waited: cmd.selector || `${cmd.ms}ms` };
    }

    // ── Read ──
    case "get_text":
    case "text": {
      if (cmd.selector) {
        const locator = await resolveSelector(cmd.selector, p);
        return { text: await locator.textContent() };
      }
      const text = await p.evaluate(() => document.body?.innerText || "");
      // Truncate to prevent oversized responses
      return { text: text.substring(0, 50000) };
    }
    case "get_title":
      return { title: await p.title() };
    case "get_url":
    case "url":
      return { url: p.url() };
    case "html": {
      const html = cmd.selector
        ? await (await resolveSelector(cmd.selector, p)).innerHTML()
        : await p.content();
      return { html: html.substring(0, 100000) };
    }
    case "links": {
      const links = await p.evaluate(() =>
        Array.from(document.querySelectorAll("a[href]")).map(a => ({
          text: a.textContent?.trim().substring(0, 100) || "",
          href: a.href,
        })).slice(0, 100)
      );
      return { links };
    }
    case "forms": {
      const forms = await p.evaluate(() =>
        Array.from(document.querySelectorAll("form")).map(f => ({
          action: f.action,
          method: f.method,
          inputs: Array.from(f.querySelectorAll("input, select, textarea")).map(i => ({
            type: i.type || i.tagName.toLowerCase(),
            name: i.name,
            id: i.id,
            placeholder: i.placeholder || "",
          })),
        }))
      );
      return { forms };
    }
    case "is_visible": {
      const locator = await resolveSelector(cmd.selector, p);
      return { visible: await locator.isVisible() };
    }
    case "find": {
      // Semantic locator (Playwright getByRole/getByText/etc.)
      let locator;
      if (cmd.role) locator = p.getByRole(cmd.role, cmd.name ? { name: cmd.name } : {});
      else if (cmd.text) locator = p.getByText(cmd.text, { exact: cmd.exact || false });
      else if (cmd.label) locator = p.getByLabel(cmd.label);
      else if (cmd.placeholder) locator = p.getByPlaceholder(cmd.placeholder);
      else if (cmd.testid) locator = p.getByTestId(cmd.testid);
      else return { error: "find requires one of: role, text, label, placeholder, testid" };

      const count = await locator.count();
      const items = [];
      for (let i = 0; i < Math.min(count, 10); i++) {
        items.push({
          text: await locator.nth(i).textContent().catch(() => ""),
          visible: await locator.nth(i).isVisible().catch(() => false),
        });
      }
      return { count, items };
    }

    // ── Screenshot ──
    case "screenshot": {
      const opts = { type: "png" };
      if (cmd.full_page) opts.fullPage = true;
      if (cmd.selector) {
        const locator = await resolveSelector(cmd.selector, p);
        const buf = await locator.screenshot(opts);
        if (cmd.path) { fs.writeFileSync(cmd.path, buf); return { path: cmd.path }; }
        return { base64: buf.toString("base64") };
      }
      const buf = await p.screenshot(opts);
      if (cmd.path) { fs.writeFileSync(cmd.path, buf); return { path: cmd.path }; }
      return { base64: buf.toString("base64") };
    }

    // ── JavaScript ──
    case "js":
    case "eval": {
      const result = await p.evaluate(cmd.expression || cmd.code);
      return { result: JSON.parse(JSON.stringify(result ?? null)) };
    }

    // ── Console & Network ──
    case "console": {
      // Return recent console messages
      return { message: "Console logging available via page.on('console') — use snapshot for page state" };
    }
    case "cookies": {
      const cookies = await context.cookies();
      return { cookies: cookies.map(c => ({ name: c.name, domain: c.domain, path: c.path, secure: c.secure })) };
    }

    // ── Tab Management (gstack-inspired) ──
    case "tabs": {
      const tabList = [];
      for (const [id, pg] of pages) {
        tabList.push({ id, url: pg.url(), title: await pg.title(), active: pg === activePage });
      }
      return { tabs: tabList };
    }
    case "newtab": {
      const newPage = await context.newPage();
      const tabId = nextTabId++;
      pages.set(tabId, newPage);
      activePage = newPage;
      newPage.on("framenavigated", () => { refMap.clear(); nextRefId = 1; });
      if (cmd.url) await newPage.goto(cmd.url, { timeout });
      return { tab_id: tabId, url: newPage.url() };
    }
    case "tab": {
      const targetPage = pages.get(cmd.tab_id);
      if (!targetPage) return { error: `Tab ${cmd.tab_id} not found. Use 'tabs' to list.` };
      activePage = targetPage;
      refMap.clear(); nextRefId = 1;
      return { switched_to: cmd.tab_id, url: activePage.url() };
    }
    case "closetab": {
      const tabToClose = cmd.tab_id ? pages.get(cmd.tab_id) : activePage;
      if (!tabToClose) return { error: `Tab not found` };
      const closedId = [...pages.entries()].find(([_, pg]) => pg === tabToClose)?.[0];
      await tabToClose.close();
      pages.delete(closedId);
      if (activePage === tabToClose) {
        activePage = pages.values().next().value || await context.newPage();
        if (!pages.size) { const id = nextTabId++; pages.set(id, activePage); }
      }
      return { closed: closedId };
    }

    // ── Close ──
    case "close": {
      if (browser) { await browser.close().catch(() => {}); browser = null; }
      return { closed: true };
    }

    default:
      throw new Error(
        `Unknown action "${cmd.action}". Available: open, snapshot, click, fill, type, press, ` +
        `hover, scroll, select, wait, text, html, links, forms, screenshot, tabs, newtab, tab, closetab, ` +
        `find, is_visible, js, cookies, back, forward, reload, close`
      );
  }
}

// ── HTTP Server ────────────────────────────────────────────────

function resetIdleTimer() {
  if (idleTimer) clearTimeout(idleTimer);
  idleTimer = setTimeout(async () => {
    console.error(`[MoA Browser] Idle timeout (${IDLE_TIMEOUT_MS / 1000}s), shutting down`);
    if (browser) await browser.close().catch(() => {});
    process.exit(0);
  }, IDLE_TIMEOUT_MS);
}

const server = http.createServer(async (req, res) => {
  resetIdleTimer();

  // Health check
  if (req.method === "GET" && req.url === "/health") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({
      status: "ok",
      browser_connected: browser?.isConnected() || false,
      tabs: pages.size,
      refs: refMap.size,
      uptime_seconds: Math.floor((Date.now() - startedAt) / 1000),
    }));
    return;
  }

  // Shutdown
  if (req.method === "POST" && req.url === "/shutdown") {
    res.writeHead(200, { "Content-Type": "application/json" });
    res.end(JSON.stringify({ status: "shutting_down" }));
    if (browser) await browser.close().catch(() => {});
    process.exit(0);
  }

  // Command execution
  if (req.method === "POST" && req.url === "/command") {
    let body = "";
    req.on("data", chunk => { body += chunk; });
    req.on("end", async () => {
      try {
        const cmd = JSON.parse(body);
        const result = await executeCommand(cmd);
        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ success: true, data: result, error: null }));
      } catch (err) {
        // AI-agent-friendly error messages (gstack pattern)
        const message = err.message || String(err);
        let guidance = "";
        if (message.includes("not found") || message.includes("not interactable")) {
          guidance = " Run `snapshot` to refresh element refs.";
        } else if (message.includes("Timeout")) {
          guidance = " Page may be slow. Try increasing timeout_ms or check the URL.";
        } else if (message.includes("net::ERR_")) {
          guidance = " Network error. Check if the URL is accessible.";
        }

        res.writeHead(200, { "Content-Type": "application/json" });
        res.end(JSON.stringify({
          success: false,
          data: null,
          error: message + guidance,
        }));
      }
    });
    return;
  }

  res.writeHead(404);
  res.end("Not found");
});

// ── Startup ────────────────────────────────────────────────────

server.listen(PORT, "127.0.0.1", () => {
  console.error(`[MoA Browser] Daemon listening on http://127.0.0.1:${PORT}`);
  console.error(`[MoA Browser] Idle timeout: ${IDLE_TIMEOUT_MS / 1000}s`);

  // Write state file for Rust side to discover port and token
  const stateDir = path.dirname(STATE_FILE);
  if (!fs.existsSync(stateDir)) fs.mkdirSync(stateDir, { recursive: true });
  fs.writeFileSync(STATE_FILE, JSON.stringify({
    pid: process.pid,
    port: PORT,
    token: AUTH_TOKEN,
    startedAt: new Date().toISOString(),
    headless: HEADLESS,
  }), { mode: 0o600 });

  resetIdleTimer();
});

// Cleanup on exit
process.on("SIGINT", async () => {
  if (browser) await browser.close().catch(() => {});
  try { fs.unlinkSync(STATE_FILE); } catch {}
  process.exit(0);
});
process.on("SIGTERM", async () => {
  if (browser) await browser.close().catch(() => {});
  try { fs.unlinkSync(STATE_FILE); } catch {}
  process.exit(0);
});
