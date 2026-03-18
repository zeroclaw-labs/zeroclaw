#!/usr/bin/env node
//
// playwright-bridge.js — Playwright automation bridge for ZeroClaw browser tool.
//
// Accepts a single JSON command via argv (base64-encoded) or stdin,
// executes it via Playwright, and returns a JSON response to stdout.
//
// Usage:
//   node playwright-bridge.js <base64-encoded-json>
//   echo '{"action":"open","url":"https://example.com"}' | node playwright-bridge.js
//
// Response format: { "success": bool, "data": any, "error": string|null }
//

const { chromium } = require("playwright");

let browser = null;
let context = null;
let page = null;

async function ensureBrowser(headless) {
  if (!browser || !browser.isConnected()) {
    browser = await chromium.launch({
      headless: headless !== false,
      args: [
        "--disable-blink-features=AutomationControlled",
        "--no-sandbox",
      ],
    });
    context = await browser.newContext({
      userAgent:
        "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
      viewport: { width: 1280, height: 720 },
      locale: "ko-KR",
    });
    page = await context.newPage();
  }
  if (!page || page.isClosed()) {
    page = await context.newPage();
  }
  return page;
}

async function executeAction(cmd) {
  const action = cmd.action;
  const headless = cmd.headless !== false;
  const p = await ensureBrowser(headless);

  switch (action) {
    case "open": {
      const resp = await p.goto(cmd.url, {
        waitUntil: cmd.wait_until || "domcontentloaded",
        timeout: cmd.timeout_ms || 30000,
      });
      return {
        url: p.url(),
        title: await p.title(),
        status: resp ? resp.status() : null,
      };
    }

    case "snapshot": {
      const tree = await p.evaluate((opts) => {
        function walk(node, depth, maxDepth, interactiveOnly) {
          if (maxDepth && depth > maxDepth) return null;
          const tag = node.tagName ? node.tagName.toLowerCase() : "";
          const interactiveTags = [
            "a", "button", "input", "select", "textarea", "details", "summary",
          ];
          const isInteractive =
            interactiveTags.includes(tag) ||
            node.getAttribute?.("role") === "button" ||
            node.getAttribute?.("tabindex") !== null ||
            node.getAttribute?.("onclick") !== null;

          if (interactiveOnly && !isInteractive && node.children) {
            const kids = [];
            for (const child of node.children) {
              const r = walk(child, depth + 1, maxDepth, interactiveOnly);
              if (r) kids.push(r);
            }
            return kids.length ? { children: kids } : null;
          }

          const result = { tag };
          if (node.id) result.id = node.id;
          const text = node.textContent?.trim().slice(0, 200);
          if (text && !node.children?.length) result.text = text;
          if (node.getAttribute?.("href")) result.href = node.getAttribute("href");
          if (node.getAttribute?.("type")) result.type = node.getAttribute("type");
          if (node.getAttribute?.("name")) result.name = node.getAttribute("name");
          if (node.getAttribute?.("value")) result.value = node.getAttribute("value");
          if (node.getAttribute?.("placeholder"))
            result.placeholder = node.getAttribute("placeholder");
          if (node.getAttribute?.("aria-label"))
            result.ariaLabel = node.getAttribute("aria-label");

          if (node.children?.length) {
            result.children = [];
            for (const child of node.children) {
              const r = walk(child, depth + 1, maxDepth, interactiveOnly);
              if (r) result.children.push(r);
            }
          }
          return result;
        }
        return walk(
          document.body,
          0,
          opts.depth || null,
          opts.interactive_only || false
        );
      }, {
        depth: cmd.depth || null,
        interactive_only: cmd.interactive_only || false,
      });
      return { snapshot: tree };
    }

    case "click": {
      await p.click(cmd.selector, { timeout: cmd.timeout_ms || 5000 });
      return { clicked: cmd.selector };
    }

    case "fill": {
      await p.fill(cmd.selector, cmd.value, { timeout: cmd.timeout_ms || 5000 });
      return { filled: cmd.selector, value: cmd.value };
    }

    case "type": {
      await p.type(cmd.selector, cmd.text || cmd.value, {
        delay: cmd.delay || 50,
        timeout: cmd.timeout_ms || 5000,
      });
      return { typed: cmd.selector };
    }

    case "get_text": {
      const text = await p.textContent(cmd.selector, {
        timeout: cmd.timeout_ms || 5000,
      });
      return { text: text || "" };
    }

    case "get_title": {
      return { title: await p.title() };
    }

    case "get_url": {
      return { url: p.url() };
    }

    case "screenshot": {
      const opts = { type: "png" };
      if (cmd.full_page) opts.fullPage = true;
      if (cmd.path) {
        opts.path = cmd.path;
        await p.screenshot(opts);
        return { path: cmd.path };
      } else {
        const buffer = await p.screenshot(opts);
        return { base64: buffer.toString("base64").slice(0, 2000) + "..." };
      }
    }

    case "wait": {
      if (cmd.selector) {
        await p.waitForSelector(cmd.selector, {
          timeout: cmd.timeout_ms || 10000,
        });
        return { waited_for: cmd.selector };
      } else if (cmd.text) {
        await p.waitForFunction(
          (txt) => document.body.innerText.includes(txt),
          cmd.text,
          { timeout: cmd.timeout_ms || 10000 }
        );
        return { waited_for_text: cmd.text };
      } else {
        const ms = cmd.ms || 1000;
        await p.waitForTimeout(ms);
        return { waited_ms: ms };
      }
    }

    case "press": {
      await p.keyboard.press(cmd.key);
      return { pressed: cmd.key };
    }

    case "hover": {
      await p.hover(cmd.selector, { timeout: cmd.timeout_ms || 5000 });
      return { hovered: cmd.selector };
    }

    case "scroll": {
      const dir = cmd.direction || "down";
      const px = cmd.pixels || 300;
      const deltaMap = {
        down: [0, px],
        up: [0, -px],
        right: [px, 0],
        left: [-px, 0],
      };
      const [dx, dy] = deltaMap[dir] || [0, px];
      await p.mouse.wheel(dx, dy);
      return { scrolled: dir, pixels: px };
    }

    case "is_visible": {
      const visible = await p.isVisible(cmd.selector);
      return { visible, selector: cmd.selector };
    }

    case "close": {
      if (page && !page.isClosed()) await page.close();
      if (context) await context.close();
      if (browser) await browser.close();
      page = null;
      context = null;
      browser = null;
      return { closed: true };
    }

    case "find": {
      let locator;
      switch (cmd.by) {
        case "role":
          locator = p.getByRole(cmd.value);
          break;
        case "text":
          locator = p.getByText(cmd.value);
          break;
        case "label":
          locator = p.getByLabel(cmd.value);
          break;
        case "placeholder":
          locator = p.getByPlaceholder(cmd.value);
          break;
        case "testid":
          locator = p.getByTestId(cmd.value);
          break;
        default:
          throw new Error(`Unknown find locator: ${cmd.by}`);
      }
      const findAction = cmd.find_action || cmd.action_on_found || "click";
      switch (findAction) {
        case "click":
          await locator.click({ timeout: cmd.timeout_ms || 5000 });
          return { found_and_clicked: cmd.value };
        case "fill":
          await locator.fill(cmd.fill_value || "", {
            timeout: cmd.timeout_ms || 5000,
          });
          return { found_and_filled: cmd.value };
        case "text":
          return { text: await locator.textContent() };
        case "hover":
          await locator.hover({ timeout: cmd.timeout_ms || 5000 });
          return { found_and_hovered: cmd.value };
        case "check":
          await locator.check({ timeout: cmd.timeout_ms || 5000 });
          return { found_and_checked: cmd.value };
        default:
          throw new Error(`Unknown find action: ${findAction}`);
      }
    }

    case "select": {
      const values = Array.isArray(cmd.value) ? cmd.value : [cmd.value];
      await p.selectOption(cmd.selector, values, {
        timeout: cmd.timeout_ms || 5000,
      });
      return { selected: values, selector: cmd.selector };
    }

    case "evaluate": {
      const result = await p.evaluate(cmd.expression);
      return { result };
    }

    case "get_cookies": {
      const cookies = await context.cookies();
      return { cookies };
    }

    case "set_cookies": {
      await context.addCookies(cmd.cookies);
      return { set: cmd.cookies.length };
    }

    case "wait_for_navigation": {
      await p.waitForNavigation({
        waitUntil: cmd.wait_until || "domcontentloaded",
        timeout: cmd.timeout_ms || 30000,
      });
      return { url: p.url(), title: await p.title() };
    }

    case "get_attribute": {
      const attr = await p.getAttribute(cmd.selector, cmd.attribute, {
        timeout: cmd.timeout_ms || 5000,
      });
      return { attribute: cmd.attribute, value: attr };
    }

    case "get_inner_html": {
      const html = await p.innerHTML(cmd.selector, {
        timeout: cmd.timeout_ms || 5000,
      });
      return { html };
    }

    case "check": {
      await p.check(cmd.selector, { timeout: cmd.timeout_ms || 5000 });
      return { checked: cmd.selector };
    }

    case "uncheck": {
      await p.uncheck(cmd.selector, { timeout: cmd.timeout_ms || 5000 });
      return { unchecked: cmd.selector };
    }

    default:
      throw new Error(`Unknown action: ${action}`);
  }
}

async function main() {
  let input;

  if (process.argv[2]) {
    // Base64-encoded JSON via argv
    try {
      input = JSON.parse(Buffer.from(process.argv[2], "base64").toString("utf8"));
    } catch {
      input = JSON.parse(process.argv[2]);
    }
  } else {
    // Read from stdin
    const chunks = [];
    for await (const chunk of process.stdin) {
      chunks.push(chunk);
    }
    input = JSON.parse(Buffer.concat(chunks).toString("utf8"));
  }

  try {
    const data = await executeAction(input);
    process.stdout.write(
      JSON.stringify({ success: true, data, error: null }) + "\n"
    );
  } catch (err) {
    process.stdout.write(
      JSON.stringify({
        success: false,
        data: null,
        error: err.message || String(err),
      }) + "\n"
    );
  }

  // Keep process alive briefly for piped output
  if (input.action !== "close") {
    // Don't exit immediately — let the browser persist for subsequent calls
    // The Rust side will send "close" to clean up
  }
}

main().catch((err) => {
  process.stdout.write(
    JSON.stringify({ success: false, data: null, error: err.message }) + "\n"
  );
  process.exit(1);
});
