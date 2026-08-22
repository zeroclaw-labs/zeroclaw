import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

import { runGuard } from "./web-rsc-mode-guard.mjs";

const fixtureRoots = [];
const fixtureParent = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const approvedPlugins = `
    react(),
    tailwindcss(),
    {
      name: "zeroclaw-dev-app-prefix",
      apply: "serve",
      configureServer(server) {
        server.middlewares.use((req, _res, next) => {
          if (req.url?.startsWith("/_app/")) {
            req.url = req.url.slice("/_app".length);
          }
          next();
        });
      },
    },`;

function configWith(properties, plugins = approvedPlugins) {
  return `
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";
import { fileURLToPath } from "node:url";

export default defineConfig(() => ({
  plugins: [${plugins}
  ],
${properties}
}));
`;
}

const validConfig = configWith(`
  base: "/_app/",
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
  build: {
    outDir: "dist",
    target: ["chrome111", "edge111", "firefox113", "safari16.2"],
  },
  server: {
    allowedHosts: undefined,
    proxy: {
      "/api": { target: "http://127.0.0.1:42617", changeOrigin: true },
    },
  },
`);
const validSource = `
import { BrowserRouter } from "react-router-dom";

export { BrowserRouter };
`;

function createFixture(testContext, { source = validSource, config = validConfig } = {}) {
  const repoRoot = fs.mkdtempSync(path.join(fixtureParent, ".rsc-guard-fixture-"));
  const webRoot = path.join(repoRoot, "web");
  fs.mkdirSync(path.join(webRoot, "src"), { recursive: true });
  fs.writeFileSync(
    path.join(webRoot, "package.json"),
    `${JSON.stringify(
      {
        dependencies: { "react-router-dom": "7.18.2" },
        devDependencies: {
          "@tailwindcss/vite": "4.2.1",
          "@vitejs/plugin-react": "6.0.1",
          vite: "8.0.16",
        },
      },
      null,
      2,
    )}\n`,
  );
  fs.writeFileSync(path.join(webRoot, "vite.config.mjs"), config);
  fs.writeFileSync(path.join(webRoot, "src", "main.tsx"), source);
  fixtureRoots.push(repoRoot);
  testContext.after(() => fs.rmSync(repoRoot, { recursive: true, force: true }));
  return { repoRoot, webRoot };
}

async function expectFailure(testContext, repoRoot, pattern) {
  await assert.rejects(() => runGuard(repoRoot), pattern);
}

test.after(() => {
  for (const root of fixtureRoots) {
    fs.rmSync(root, { recursive: true, force: true });
  }
});

test("regex literals cannot hide a following forbidden import", async (t) => {
  const { repoRoot } = createFixture(t, {
    source: `
const marker = /[/*]/;
import { StaticRouter } from "react-router-dom/server";

export { StaticRouter };
`,
  });
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*react-router-dom\/server/);
});

test("a shadowed path.resolve cannot authorize an outside @ alias", async (t) => {
  const { repoRoot, webRoot } = createFixture(t, {
    config: `
import { fileURLToPath } from "node:url";

const path = {
  resolve: () => fileURLToPath(new URL("../outside", import.meta.url)),
};

export default {
  resolve: {
    alias: {
      "@": path.resolve(),
    },
  },
};
`,
  });
  fs.mkdirSync(path.join(repoRoot, "outside"));
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*effective Vite @ alias/);
  assert.equal(fs.existsSync(path.join(webRoot, "src")), true);
});

test("post-declaration config mutation cannot move @ outside web", async (t) => {
  const { repoRoot } = createFixture(t, {
    config: `
import { fileURLToPath } from "node:url";

const config = {
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
};
config.resolve.alias["@"] = fileURLToPath(new URL("../outside", import.meta.url));

export default config;
`,
  });
  fs.mkdirSync(path.join(repoRoot, "outside"));
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*effective Vite @ alias/);
});

test("the effective @ alias rooted at web/src passes", async (t) => {
  const { repoRoot, webRoot } = createFixture(t, {
    source: `
import { page } from "@/page";

export { page };
`,
  });
  fs.writeFileSync(path.join(webRoot, "src", "page.ts"), "export const page = true;\n");
  await runGuard(repoRoot);
});

test("dynamic and deferred Vite config mutation is rejected before execution", async (t) => {
  const mutations = [
    `writeFileSync(target, forbidden);`,
    `setTimeout(() => writeFileSync(target, forbidden), 50);`,
  ];
  for (const mutation of mutations) {
    const { repoRoot, webRoot } = createFixture(t, {
      config: validConfig.replace(
        "export default",
        `
const { writeFileSync } = await import("node:fs");
const target = new URL("./src/main.tsx", import.meta.url);
const forbidden = 'import { StaticRouter } from "react-router-dom/server";\\n';
${mutation}
export default`,
      ),
    });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*cannot use dynamic imports/);
    assert.equal(fs.readFileSync(path.join(webRoot, "src", "main.tsx"), "utf8"), validSource);
  }
});

test("reflective Vite config mutation is rejected before execution", async (t) => {
  const reflections = [
    `
const getBuiltinModule = Reflect.get(process, "getBuiltinModule");
const { writeFileSync } = getBuiltinModule("node:fs");
const defer = Reflect.get(globalThis, "setTimeout");
const target = new URL("./src/main.tsx", import.meta.url);
defer(() => writeFileSync(target, 'import "react-router-dom/server";\\n'), 50);`,
    `
const Execute = globalThis["Reflect"].get(globalThis["Object"], "constructor");
Execute('import("node:fs").then(({ writeFileSync }) => writeFileSync(new URL("./src/main.tsx", import.meta.url), "forbidden"))')();`,
    `
const descriptor = globalThis["Object"]["getOwnPropertyDescriptor"];
const functionPrototype = globalThis["Object"]["getPrototypeOf"](globalThis["Object"]);
const execute = descriptor(functionPrototype, "constructor").value;
execute("return 1")();`,
  ];
  for (const reflection of reflections) {
    const { repoRoot, webRoot } = createFixture(t, {
      config: validConfig.replace("export default", `${reflection}\nexport default`),
    });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*reflective or unrestricted runtime globals/);
    assert.equal(fs.readFileSync(path.join(webRoot, "src", "main.tsx"), "utf8"), validSource);
  }
});

test("aliased imported-module mutation is rejected before execution", async (t) => {
  const cases = [
    `const mutate = fileURLToPath; mutate.extra = true;`,
    `const mutate = fileURLToPath; Object.assign(mutate, { extra: true });`,
    `let mutate; mutate = fileURLToPath; mutate.extra = true;`,
    `function mutate(value) { value.extra = true; } mutate(fileURLToPath);`,
  ];
  for (const mutation of cases) {
    const { repoRoot } = createFixture(t, {
      config: validConfig.replace("export default", `${mutation}\nexport default`),
    });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*mutate (?:imported modules|restricted bindings)/);
  }
});

test("approved Vite environment fields remain read-only", async (t) => {
  const cases = [
    `process.env.ZEROCLAW_GATEWAY_PORT = "9999";`,
    `delete process.env.ZEROCLAW_GATEWAY_PORT;`,
    `process.env.ZEROCLAW_GATEWAY_PORT++;`,
  ];
  for (const mutation of cases) {
    const { repoRoot } = createFixture(t, {
      config: validConfig.replace("export default", `${mutation}\nexport default`),
    });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*cannot mutate process environment fields/);
  }
});

test("an absent generated local module remains bounded by its lexical path", async (t) => {
  const { repoRoot } = createFixture(t, {
    source: `import type { components } from "./api-generated";\nexport const ready = true;`,
  });
  await runGuard(repoRoot);
});

test("forbidden server-capable module specifiers remain rejected", async (t) => {
  const cases = [
    [`import { reactRouter } from "@react-router/dev/vite";`, /@react-router\/dev\/vite/],
    [`import { StaticRouter } from "react-router-dom/server";`, /react-router-dom\/server/],
    [`import { router } from "react-router";`, /react-router/],
    [`import rsc from "@vitejs/plugin-rsc";`, /@vitejs\/plugin-rsc/],
    [`import stream from "react-server-dom-webpack/client";`, /react-server-dom-webpack/],
  ];
  for (const [source, pattern] of cases) {
    const { repoRoot } = createFixture(t, { source });
    await expectFailure(t, repoRoot, pattern);
  }
});

test("nonliteral dynamic import and require are rejected", async (t) => {
  const sources = [
    `const name = "react-router";\nawait import(name);`,
    `const name = "react-router";\nrequire(name);`,
    `await import("react-" + "router");`,
  ];
  for (const source of sources) {
    const { repoRoot } = createFixture(t, { source });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*nonliteral dynamic module specifier/);
  }
});

test("react-router-dom namespace, dynamic, and full re-export use are rejected", async (t) => {
  const sources = [
    `import * as router from "react-router-dom";\nexport { router };`,
    `const router = import("react-router-dom");\nexport { router };`,
    `export * from "react-router-dom";`,
    `export * as router from "react-router-dom";`,
  ];
  for (const source of sources) {
    const { repoRoot } = createFixture(t, { source });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: /);
  }
});

test("renamed non-client react-router-dom value exports are rejected", async (t) => {
  const nonClientExports = [
    "ServerRouter",
    "StaticRouter",
    "StaticRouterProvider",
    "createStaticHandler",
    "createStaticRouter",
    "createRequestHandler",
    "createCookie",
    "createCookieSessionStorage",
    "createMemorySessionStorage",
    "createSession",
    "createSessionStorage",
    "UNSAFE_ServerMode",
    "unstable_getRequest",
    "unstable_matchRSCServerRequest",
    "unstable_routeRSCServerRequest",
    "unstable_RSCStaticRouter",
    "UNSAFE_RSCDefaultRootErrorBoundary",
    "UNSAFE_decodeViaTurboStream",
    "UNSAFE_getHydrationData",
    "UNSAFE_getPatchRoutesOnNavigationFunction",
    "UNSAFE_getTurboStreamSingleFetchDataStrategy",
  ];
  for (const name of nonClientExports) {
    const imported = createFixture(t, {
      source: `import { ${name} as ClientExport } from "react-router-dom";\nexport { ClientExport };`,
    });
    await expectFailure(t, imported.repoRoot, /web-rsc-mode-guard: /);

    const reExported = createFixture(t, {
      source: `export { ${name} as ClientExport } from "react-router-dom";`,
    });
    await expectFailure(t, reExported.repoRoot, /web-rsc-mode-guard: /);
  }

  const defaultImport = createFixture(t, {
    source: `import Router from "react-router-dom";\nexport { Router };`,
  });
  await expectFailure(t, defaultImport.repoRoot, /default import from react-router-dom/);
});

test("unstable RSC identifiers and server directives are rejected", async (t) => {
  const sources = [
    `const router = RSCStaticRouter;\nexport { router };`,
    `import { "unstable_createCallServer" as create } from "react-router-dom";\nexport { create };`,
    `export { "unstable_createCallServer" as create } from "react-router-dom";`,
    `"use server";\nexport const action = () => true;`,
    `"react-server";\nexport const action = () => true;`,
  ];
  for (const source of sources) {
    const { repoRoot } = createFixture(t, { source });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: /);
  }
});

test("code-generation identifiers are rejected without lexical false positives", async (t) => {
  for (const source of [
    "eval();",
    "Function();",
    "new Function();",
    `globalThis["Function"]("return 1")();`,
    `window["eval"]("1")`,
    `globalThis["e" + "val"]("1")`,
    `self[["Function"].join("")]("return 1")()`,
  ]) {
    const { repoRoot } = createFixture(t, { source });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*(?:forbidden code-generation identifier|forbidden computed global access)/);
  }

  const safe = createFixture(t, {
    source: `const note = "eval Function"; // eval() and Function() are text only\nexport { note };`,
  });
  await runGuard(safe.repoRoot);
});

test("timer handlers must be callable across global timer forms", async (t) => {
  const cases = [
    [`setTimeout("import('react-router-dom/server')", 0);`, /handler/],
    ["setInterval(`import('react-router-dom/server')`, 0);", /handler/],
    [`setTimeout("import('react-router-dom/" + "server')", 0);`, /handler/],
    [`window.setTimeout("import('react-router-dom/server')", 0);`, /handler/],
    ["self.setInterval(`import('react-router-dom/server')`, 0);", /handler/],
    [`globalThis.setTimeout("import('react-router-dom/server')", 0);`, /handler/],
    [`const handler = "import('react-router-dom/server')";\nglobalThis.setInterval(handler, 0);`, /handler/],
    [`function handler() {}\nhandler = "import('react-router-dom/server')";\nsetTimeout(handler, 0);`, /handler/],
    [`let handler = () => {};\nsetTimeout(handler, 0);`, /handler/],
    [`setTimeout(unresolved, 0);`, /handler/],
    [`const defer = globalThis.setInterval;\ndefer("import('react-router-dom/server')", 0);`, /indirect global timer/],
    [`window.setTimeout.call(window, "import('react-router-dom/server')", 0);`, /indirect global timer/],
    [`globalThis.setInterval.apply(globalThis, ["import('react-router-dom/server')", 0]);`, /indirect global timer/],
    [`const defer = self.setTimeout.bind(self);\ndefer("import('react-router-dom/server')", 0);`, /indirect global timer/],
    [`(0, setTimeout)("import('react-router-dom/server')", 0);`, /indirect global timer/],
    [`function handler() {}\nfor (handler of ["import('react-router-dom/server')"]) {}\nsetTimeout(handler, 0);`, /handler/],
    [`function handler() {}\nfor (handler in { "import('react-router-dom/server')": true }) {}\nsetTimeout(handler, 0);`, /handler/],
    [`class Promise { constructor(executor) { executor("import('react-router-dom/server')"); } }\nnew Promise((resolve) => setTimeout(resolve, 0));`, /handler/],
    [`globalThis.Promise = class { constructor(executor) { executor("import('react-router-dom/server')"); } };\nnew Promise((resolve) => setTimeout(resolve, 0));`, /global Promise/],
    [`Reflect.apply(Reflect.get(globalThis, "setTimeout"), globalThis, ["import('react-router-dom/server')", 0]);`, /reflective global access/],
    [`Object.defineProperty(globalThis, "Promise", { value: class { constructor(executor) { executor("import('react-router-dom/server')"); } } });\nnew Promise((resolve) => setTimeout(resolve, 0));`, /Object global property access/],
    [`Object.getOwnPropertyDescriptor(globalThis, "setTimeout").value.call(globalThis, "import('react-router-dom/server')", 0);`, /Object global property access/],
    [`const Factory = class Promise { method() { new Promise((resolve) => setTimeout(resolve, 0)); } };`, /handler/],
    [`const timers = globalThis;\ntimers.setTimeout("import('react-router-dom/server')", 0);`, /indirect global object/],
    [`function expose() { return window; }\nexpose().setInterval("import('react-router-dom/server')", 0);`, /indirect global object/],
    [`Object.defineProperty(globalThis, "timerRoot", { get() { return this; } });\nglobalThis.timerRoot.setTimeout("import('react-router-dom/server')", 0);`, /Object global property access/],
  ];
  for (const [source, pattern] of cases) {
    const { repoRoot } = createFixture(t, { source });
    await expectFailure(t, repoRoot, new RegExp(`web-rsc-mode-guard: .*${pattern.source}`));
  }

  const safe = createFixture(t, {
    source: `
function direct() {}
const local = () => {};
const asserted = (() => {}) as () => void;
const nonNull = (() => {})!;
const satisfied = (() => {}) satisfies () => void;
const Local = class Promise {};

setTimeout(direct, 0);
window.setInterval(local, 0);
self.setTimeout(asserted, 0);
globalThis.setInterval(nonNull, 0);
setTimeout(satisfied, 0);
setTimeout(() => {}, 0);
window.setInterval(function () {}, 0);
new Promise((resolve) => setTimeout(resolve, 0));
(setTimeout)(() => {}, 0);
`,
  });
  await runGuard(safe.repoRoot);

  const shadowedMutation = createFixture(t, {
    source: `
const handler = () => {};
const state = {};
state.handler = "not a binding mutation";
function unrelated() {
  let handler;
  handler = "not the outer handler";
}
setTimeout(handler, 0);
`,
  });
  await runGuard(shadowedMutation.repoRoot);

  const localGlobals = createFixture(t, {
    source: `
function setTimeout(value) { return value; }
const window = { setInterval(value) { return value; } };
const api = { setTimeout() {} };
class Adapter { setInterval() {} }
setTimeout("ordinary data");
window.setInterval("ordinary data");
api.setTimeout();
new Adapter().setInterval();
Object.defineProperty(globalThis, "window", { value: {}, configurable: true });
const hasCrypto = "crypto" in (globalThis as object);
delete (globalThis as { window?: unknown }).window;
`,
  });
  await runGuard(localGlobals.repoRoot);
});

test("HTML script bodies and local root-relative sources are inspected", async (t) => {
  const { repoRoot, webRoot } = createFixture(t);
  fs.writeFileSync(
    path.join(webRoot, "index.html"),
    `<script>const note = "eval Function"; // eval() and Function() are text only</script>\n<script type="module" src="/src/main.tsx"></script>\n`,
  );
  await runGuard(repoRoot);
});

test("local imports cannot resolve into skipped web/dist", async (t) => {
  const { repoRoot, webRoot } = createFixture(t, {
    source: `import { distValue } from "../dist/fixture";\nexport { distValue };`,
  });
  fs.mkdirSync(path.join(webRoot, "dist"), { recursive: true });
  fs.writeFileSync(
    path.join(webRoot, "dist", "fixture.ts"),
    "export const distValue = true;\n",
  );
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*dist/);
});

test("HTML script sources cannot resolve into skipped web/dist", async (t) => {
  const { repoRoot, webRoot } = createFixture(t);
  fs.mkdirSync(path.join(webRoot, "dist"), { recursive: true });
  fs.writeFileSync(path.join(webRoot, "dist", "fixture.js"), "export const distValue = true;\n");
  // nosemgrep: javascript.lang.security.audit.unknown-value-with-script-tag.unknown-value-with-script-tag -- controlled negative-test fixture
  fs.writeFileSync(
    path.join(webRoot, "index.html"),
    `<script src="/dist/fixture.js"></script>`,
  );
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*dist/);
});

test("HTML base elements cannot redirect checked script sources", async (t) => {
  const cases = [
    `<base href="/dist/"><script src="fixture.js"></script>`,
    `<base href="https://example.com/"><script src="fixture.js"></script>`,
  ];
  for (const html of cases) {
    const { repoRoot, webRoot } = createFixture(t);
    fs.writeFileSync(path.join(webRoot, "fixture.js"), "export {};\n");
    fs.writeFileSync(path.join(webRoot, "index.html"), html);
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*base element/);
  }

  const commented = createFixture(t);
  fs.writeFileSync(
    path.join(commented.webRoot, "index.html"),
    `<!-- <base href="/dist/"> --><script>const text = "<base href='/dist/'>";</script>`,
  );
  await runGuard(commented.repoRoot);
});

test("HTML script sources fail closed for nonlocal, malformed, and missing paths", async (t) => {
  const cases = [
    [`<script src="https://example.com/app.js"></script>`, /external script src/],
    [`<script src="//example.com/app.js"></script>`, /external script src/],
    [`<script src="data:text/javascript,alert(1)"></script>`, /external script src/],
    [`<script src="javascript:alert(1)"></script>`, /external script src/],
    [`<script src="&#x68;ttps://example.com/app.js"></script>`, /ambiguous script src/],
    [`<script src="/src/main.tsx app.js"></script>`, /ambiguous script src/],
    [`<script src=/src/main.tsx></script>`, /unquoted script src/],
    [`<script src=""></script>`, /malformed script src/],
    [`<script src="/src/missing.tsx"></script>`, /missing script src/],
  ];
  for (const [html, pattern] of cases) {
    const { repoRoot, webRoot } = createFixture(t);
    fs.writeFileSync(path.join(webRoot, "index.html"), html);
    await expectFailure(t, repoRoot, new RegExp(`web-rsc-mode-guard: .*${pattern.source}`));
  }

  const outside = createFixture(t);
  fs.writeFileSync(path.join(outside.repoRoot, "outside.js"), "export const outside = true;\n");
  // nosemgrep: javascript.lang.security.audit.unknown-value-with-script-tag.unknown-value-with-script-tag -- controlled negative-test fixture
  fs.writeFileSync(
    path.join(outside.webRoot, "index.html"),
    `<script src="../outside.js"></script>`,
  );
  await expectFailure(t, outside.repoRoot, /web-rsc-mode-guard: .*escapes the guarded web root/);

  const nodeModules = createFixture(t);
  fs.mkdirSync(path.join(nodeModules.webRoot, "node_modules"), { recursive: true });
  fs.writeFileSync(path.join(nodeModules.webRoot, "node_modules", "local.js"), "export {};\n");
  // nosemgrep: javascript.lang.security.audit.unknown-value-with-script-tag.unknown-value-with-script-tag -- controlled negative-test fixture
  fs.writeFileSync(
    path.join(nodeModules.webRoot, "index.html"),
    `<script src="/node_modules/local.js"></script>`,
  );
  await expectFailure(t, nodeModules.repoRoot, /web-rsc-mode-guard: .*node_modules/);
});

test("server and RSC entry filenames are rejected", async (t) => {
  const { repoRoot, webRoot } = createFixture(t);
  fs.writeFileSync(path.join(webRoot, "entry.rsc.mjs"), "export default {};\n");
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*server\/RSC entry surface/);
});

test("unsupported executable formats fail closed", async (t) => {
  const { repoRoot, webRoot } = createFixture(t);
  fs.writeFileSync(
    path.join(webRoot, "src", "server.mdx"),
    'import { StaticRouter } from "react-router-dom/server";\n',
  );
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*unsupported executable source format/);
});

test("unrecognized component source formats fail closed", async (t) => {
  const { repoRoot, webRoot } = createFixture(t);
  fs.writeFileSync(
    path.join(webRoot, "src", "server.vue"),
    '<script>import { StaticRouter } from "react-router-dom/server";</script>\n',
  );
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*unrecognized source format/);
});

test("imports cannot reach component formats elsewhere under web", async (t) => {
  for (const target of ["server.vue", "scripts/server.vue"]) {
    const { repoRoot, webRoot } = createFixture(t, {
      source: `import { server } from "../${target}";\nexport { server };`,
    });
    const targetPath = path.join(webRoot, target);
    fs.mkdirSync(path.dirname(targetPath), { recursive: true });
    fs.writeFileSync(
      targetPath,
      '<script>import { StaticRouter } from "react-router-dom/server";</script>\n',
    );
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*unrecognized source format/);
  }
});

test("symbolic links are rejected", async (t) => {
  const { repoRoot, webRoot } = createFixture(t);
  const outside = path.join(repoRoot, "outside.ts");
  fs.writeFileSync(outside, "export const outside = true;\n");
  fs.symlinkSync(outside, path.join(webRoot, "src", "bridge.ts"));
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*symbolic link/);
});

test("relative imports cannot escape web or enter node_modules", async (t) => {
  const outsideFixture = createFixture(t, {
    source: `import { bridge } from "../../outside/bridge";\nexport { bridge };`,
  });
  fs.mkdirSync(path.join(outsideFixture.repoRoot, "outside"));
  fs.writeFileSync(
    path.join(outsideFixture.repoRoot, "outside", "bridge.ts"),
    "export const bridge = true;\n",
  );
  await expectFailure(t, outsideFixture.repoRoot, /web-rsc-mode-guard: .*escapes the guarded web root/);

  const nodeModulesFixture = createFixture(t, {
    source: `import { server } from "../node_modules/local-rsc/index";\nexport { server };`,
  });
  fs.mkdirSync(path.join(nodeModulesFixture.webRoot, "node_modules", "local-rsc"), {
    recursive: true,
  });
  fs.writeFileSync(
    path.join(nodeModulesFixture.webRoot, "node_modules", "local-rsc", "index.ts"),
    "export const server = true;\n",
  );
  await expectFailure(t, nodeModulesFixture.repoRoot, /web-rsc-mode-guard: .*node_modules/);
});

test("alias imports cannot escape the effective web/src boundary", async (t) => {
  const { repoRoot, webRoot } = createFixture(t, {
    source: `import { bridge } from "@/../../outside/bridge";\nexport { bridge };`,
  });
  fs.mkdirSync(path.join(repoRoot, "outside"));
  fs.writeFileSync(path.join(repoRoot, "outside", "bridge.ts"), "export const bridge = true;\n");
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*escapes the guarded web root/);
  assert.equal(fs.existsSync(path.join(webRoot, "vite.config.mjs")), true);
});

test("undeclared bare imports are rejected while declarative SPA imports pass", async (t) => {
  const invalid = createFixture(t, {
    source: `import { page } from "local-page";\nexport { page };`,
  });
  await expectFailure(t, invalid.repoRoot, /web-rsc-mode-guard: .*undeclared package or local alias/);

  const valid = createFixture(t, { source: validSource });
  await runGuard(valid.repoRoot);
});

test("declared packages must resolve to real modules inside web", async (t) => {
  const unresolved = createFixture(t, {
    source: `import { page } from "missing-page";\nexport { page };`,
  });
  const unresolvedPackage = JSON.parse(
    fs.readFileSync(path.join(unresolved.webRoot, "package.json"), "utf8"),
  );
  unresolvedPackage.dependencies["missing-page"] = "1.0.0";
  fs.writeFileSync(
    path.join(unresolved.webRoot, "package.json"),
    `${JSON.stringify(unresolvedPackage, null, 2)}\n`,
  );
  await expectFailure(t, unresolved.repoRoot, /web-rsc-mode-guard: .*cannot resolve package import/);

  const linked = createFixture(t, {
    source: `import { page } from "linked-page";\nexport { page };`,
  });
  const linkedPackage = JSON.parse(
    fs.readFileSync(path.join(linked.webRoot, "package.json"), "utf8"),
  );
  linkedPackage.dependencies["linked-page"] = "1.0.0";
  fs.writeFileSync(
    path.join(linked.webRoot, "package.json"),
    `${JSON.stringify(linkedPackage, null, 2)}\n`,
  );
  const outsidePackage = path.join(linked.repoRoot, "outside-package");
  fs.mkdirSync(outsidePackage);
  fs.writeFileSync(
    path.join(outsidePackage, "package.json"),
    `${JSON.stringify({ name: "linked-page", version: "1.0.0", module: "index.js" })}\n`,
  );
  fs.writeFileSync(path.join(outsidePackage, "index.js"), "export const page = true;\n");
  fs.mkdirSync(path.join(linked.webRoot, "node_modules"));
  fs.symlinkSync(outsidePackage, path.join(linked.webRoot, "node_modules", "linked-page"));
  await expectFailure(t, linked.repoRoot, /web-rsc-mode-guard: .*outside the guarded web root/);
});

test("bare package aliases cannot redirect into skipped web/node_modules", async (t) => {
  const { repoRoot, webRoot } = createFixture(t, {
    source: `import { hidden } from "foo";\nexport { hidden };`,
    config: configWith(`
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
      foo: fileURLToPath(new URL("./node_modules/hidden.js", import.meta.url)),
    },
  },`),
  });
  const packageJson = JSON.parse(fs.readFileSync(path.join(webRoot, "package.json"), "utf8"));
  packageJson.dependencies.foo = "1.0.0";
  fs.writeFileSync(
    path.join(webRoot, "package.json"),
    `${JSON.stringify(packageJson, null, 2)}\n`,
  );
  fs.mkdirSync(path.join(webRoot, "node_modules"), { recursive: true });
  fs.writeFileSync(
    path.join(webRoot, "node_modules", "hidden.js"),
    `import { StaticRouter } from "react-router-dom";\nexport { StaticRouter as hidden };\n`,
  );
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*node_modules/);
});

test("virtual package modules fail closed", async (t) => {
  const { repoRoot, webRoot } = createFixture(t, {
    source: `import { page } from "virtual-page";\nexport { page };`,
    config: `
import { fileURLToPath } from "node:url";

export default {
  plugins: [{
    name: "virtual-page-test",
    resolveId(source) {
      return source === "virtual-page" ? "\\0virtual-page" : null;
    },
  }],
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
};
`,
  });
  const packageJson = JSON.parse(fs.readFileSync(path.join(webRoot, "package.json"), "utf8"));
  packageJson.dependencies["virtual-page"] = "1.0.0";
  fs.writeFileSync(
    path.join(webRoot, "package.json"),
    `${JSON.stringify(packageJson, null, 2)}\n`,
  );
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*virtual module/);
});

test("unknown build-only Vite transforms are rejected", async (t) => {
  const { repoRoot } = createFixture(t, {
    config: configWith(
      `
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },`,
      `${approvedPlugins}
    {
    name: "fixture-build-transform",
    apply: "build",
    transform() {
      return null;
    },
  },`,
    ),
  });
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*exactly the approved React/);
});

test("approved Vite plugin names cannot be spoofed", async (t) => {
  const { repoRoot } = createFixture(t, {
    config: configWith(
      `
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },`,
      `${approvedPlugins}
    {
      name: "vite:react-babel",
      apply: "build",
      transform() {
        return "import { ServerRouter } from 'react-router-dom/server'";
      },
    },`,
    ),
  });
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*exactly the approved React/);
});

test("required Vite plugins cannot be removed", async (t) => {
  const { repoRoot } = createFixture(t, {
    config: configWith(
      `
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },`,
      `
    react(),
    tailwindcss(),`,
    ),
  });
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*exactly the approved React/);
});

test("nested build and worker plugin options are rejected", async (t) => {
  const nestedOptions = [
    `
  build: {
    rollupOptions: {
      plugins: [{ name: "fixture-rollup-transform", transform() { return null; } }],
    },
  },`,
    `
  worker: {
    plugins: () => [{ name: "fixture-worker-transform", transform() { return null; } }],
  },`,
  ];
  for (const nested of nestedOptions) {
    const { repoRoot } = createFixture(t, {
      config: configWith(`
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },
${nested}`),
    });
    await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*nested plugin options/);
  }
});

test("Vite code-generation configuration options are rejected", async (t) => {
  const cases = [
    [
      `
  define: {
    __RSC_INJECT__: "import(\\"react-router-dom\\").then((m) => m.StaticRouter)",
  },`,
      /unapproved top-level property: define/,
      `console.log(__RSC_INJECT__);\n`,
    ],
    [
      `
  esbuild: {
    jsxInject: 'import React from "react";',
  },`,
      /unapproved top-level property: esbuild/,
    ],
    [
      `
  worker: {
    format: "es",
  },`,
      /unapproved top-level property: worker/,
    ],
    [
      `
  ssr: {
    noExternal: ["react-router-dom"],
  },`,
      /unapproved top-level property: ssr/,
    ],
    [
      `
  build: {
    rollupOptions: {
      input: "./src/main.tsx",
    },
  },`,
      /build contains an unapproved property: rollupOptions/,
    ],
  ];
  for (const [properties, pattern, source = validSource] of cases) {
    const { repoRoot } = createFixture(t, {
      source,
      config: configWith(
        `${properties}
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },`,
      ),
    });
    await expectFailure(t, repoRoot, new RegExp(`web-rsc-mode-guard: .*${pattern.source}`));
  }
});

test("inherited enumerable Vite define options are rejected before config execution", async (t) => {
  const inheritedDefine = `
Object.defineProperty(Object.prototype, "define", {
  configurable: true,
  enumerable: true,
  value: {
    __RSC_INJECT__: "import(\\"react-router-dom\\").then((m) => m.StaticRouter)",
  },
});
`;
  const { repoRoot } = createFixture(t, {
    source: `console.log(__RSC_INJECT__);\n`,
    config: validConfig.replace("export default", `${inheritedDefine}\nexport default`),
  });
  assert.equal(Object.hasOwn(Object.prototype, "define"), false);
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*prototype mutation primitives/);
  assert.equal(Object.hasOwn(Object.prototype, "define"), false);
});

test("aliased intrinsic corruption is rejected in an isolated guard process", (t) => {
  const intrinsicCorruption = `
const O = globalThis.Object;
const originalHasOwn = O.hasOwn;
const originalSetHas = globalThis.Set.prototype.has;
O.defineProperty(O.prototype, "define", {
  configurable: true,
  enumerable: true,
  value: {
    __RSC_INJECT__: "import(\\"react-router-dom\\").then((m) => m.StaticRouter)",
  },
});
O.hasOwn = (value, key) => key === "define" || originalHasOwn(value, key);
globalThis.Set.prototype.has = function has(value) {
  return value === "define" || originalSetHas.call(this, value);
};
`;
  const { repoRoot } = createFixture(t, {
    source: `console.log(__RSC_INJECT__);\n`,
    config: validConfig.replace("export default", `${intrinsicCorruption}\nexport default`),
  });
  const result = spawnSync(
    process.execPath,
    [fileURLToPath(new URL("./web-rsc-mode-guard.mjs", import.meta.url))],
    {
      encoding: "utf8",
      env: { ...process.env, ZEROCLAW_RSC_GUARD_ROOT: repoRoot },
    },
  );
  assert.equal(result.status, 1);
  assert.match(result.stderr, /web-rsc-mode-guard: .*mutable object prototypes/);
  assert.equal(Object.hasOwn(Object.prototype, "define"), false);
});

test("serve-prefix middleware behavior is required", async (t) => {
  const noOpPrefix = `
    react(),
    tailwindcss(),
    {
      name: "zeroclaw-dev-app-prefix",
      apply: "serve",
      configureServer() {},
    },`;
  const { repoRoot } = createFixture(t, {
    config: configWith(
      `
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },`,
      noOpPrefix,
    ),
  });
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*install exactly one middleware/);
});

test("Vite config cannot obscure plugin ownership through a top-level spread", async (t) => {
  const { repoRoot } = createFixture(t, {
    config: configWith(`
  ...{ base: "/" },
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },`),
  });
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*spread or computed/);
});

test("Vite config cannot inherit nested build plugins through __proto__", async (t) => {
  const { repoRoot } = createFixture(t, {
    config: configWith(`
  __proto__: {
    build: {
      rollupOptions: {
        plugins: [{ name: "prototype-build-transform", transform() { return null; } }],
      },
    },
  },
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },`),
  });
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*__proto__/);
});

test("function-valued config containers cannot carry build plugins", async (t) => {
  const { repoRoot } = createFixture(t, {
    config: configWith(`
  build: Object.assign(() => {}, {
    rollupOptions: {
      plugins: [{ name: "function-build-transform", transform() { return null; } }],
    },
  }),
  resolve: {
    alias: {
      "@": fileURLToPath(new URL("./src", import.meta.url)),
    },
  },`),
  });
  await expectFailure(t, repoRoot, /web-rsc-mode-guard: .*reflective or unrestricted runtime globals/);
});
