// Runs the splash page's real script under controlled Tauri answers and checks
// that it asks to open the dashboard only once the backend reports a ready
// startup.
//
// Usage: node splash_startup.cjs <path to splash/index.html>
"use strict";

const fs = require("fs");
const vm = require("vm");
const assert = require("assert");

const pagePath = process.argv[2];
const source = fs
  .readFileSync(pagePath, "utf8")
  .match(/<script>([\s\S]*?)<\/script>/)[1];
const flush = () => new Promise((resolve) => setImmediate(resolve));

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

// Load the page's script with every `get_startup` answer held by the test.
function load() {
  const calls = [];
  const pending = [];
  const elements = new Map();
  let listener = null;
  let interval = null;
  const context = {
    document: {
      getElementById(id) {
        if (!elements.has(id)) {
          elements.set(id, {
            style: {},
            classList: { add() {}, remove() {} },
            textContent: "",
          });
        }
        return elements.get(id);
      },
    },
    window: {
      __TAURI__: {
        core: {
          invoke(cmd) {
            calls.push(cmd);
            if (cmd === "get_startup") {
              const answer = deferred();
              pending.push(answer);
              return answer.promise;
            }
            return Promise.resolve();
          },
        },
        event: {
          listen(_name, callback) {
            listener = callback;
            return Promise.resolve(() => {});
          },
        },
      },
    },
    setInterval(callback) {
      interval = callback;
    },
  };
  vm.runInNewContext(source, context, { filename: pagePath });
  return {
    calls,
    status: () => elements.get("status").textContent,
    async answer(startup) {
      assert(pending.length > 0, "no get_startup call is waiting");
      pending.shift().resolve(startup);
      await flush();
      await flush();
    },
    async announce(startup) {
      listener({ payload: startup });
      await flush();
      await flush();
    },
    async poll() {
      interval();
      await flush();
    },
  };
}

const READY = { state: "ready" };
const PENDING = { state: "pending", message: "Starting the ZeroClaw daemon…" };
const INCOMPATIBLE = {
  state: "failed",
  kind: "incompatible",
  message: "bundled core version mismatch",
};

const scenarios = [
  [
    "a pending startup never opens the dashboard",
    async () => {
      const page = load();
      await page.answer(PENDING);
      await page.poll();
      await page.answer(PENDING);
      assert(!page.calls.includes("open_dashboard"), page.calls.join());
      assert.strictEqual(page.status(), PENDING.message);
    },
  ],
  [
    "a failure announced while a check is waiting wins over its answer",
    async () => {
      const page = load();
      await page.announce(INCOMPATIBLE);
      await page.answer(READY);
      await page.poll();
      assert(!page.calls.includes("open_dashboard"), page.calls.join());
      assert.strictEqual(page.status(), INCOMPATIBLE.message);
    },
  ],
  [
    "a failure recorded before the page listened is shown and stops polling",
    async () => {
      const page = load();
      await page.answer(INCOMPATIBLE);
      await page.poll();
      assert(!page.calls.includes("open_dashboard"), page.calls.join());
      assert.deepStrictEqual(page.calls, ["get_startup"]);
      assert.strictEqual(page.status(), INCOMPATIBLE.message);
    },
  ],
  [
    "a ready startup opens the dashboard once",
    async () => {
      const page = load();
      await page.answer(READY);
      await page.poll();
      assert.deepStrictEqual(page.calls, ["get_startup", "open_dashboard"]);
    },
  ],
];

(async () => {
  for (const [name, run] of scenarios) {
    try {
      await run();
      console.log(`ok - ${name}`);
    } catch (error) {
      console.log(`not ok - ${name}: ${error.message}`);
      process.exitCode = 1;
    }
  }
})();
