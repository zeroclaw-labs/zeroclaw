// Run against Vite with Playwright + Chrome available. API/RPC fixtures are synthetic.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright");
const fs = require("node:fs");
const assert = require("node:assert/strict");
const out = process.env.WEB_SMOKE_OUTPUT || "/tmp/zeroclaw-workspace-evidence";
const appUrl = process.env.WEB_SMOKE_URL || "http://127.0.0.1:5178/";
fs.mkdirSync(out, { recursive: true });
async function chooseLayout(page, name) {
  await page.getByRole('button', { name: 'More options', exact: true }).click();
  await page.getByRole('button', { name: 'Appearance', exact: true }).click();
  await page.getByRole('radio', { name: new RegExp('^' + name) }).check();
  await page.keyboard.press('Escape');
}

(async () => {
  const browser = await chromium.launch({ channel: "chrome", headless: true });
  const context = await browser.newContext({
    viewport: { width: 1440, height: 980 },
  });
  await context.addInitScript(() => {
    localStorage.setItem("zeroclaw_token", "zc_test_browser_fixture");
    localStorage.setItem("zeroclaw_locale", "en");
  });
  let codeRunning = false;
  let capabilities = true;
  let freshInstall = false;
  const sessions = [
    {
      session_id: "release-plan",
      session_key: "gw_release-plan",
      name: "Plan the next release",
      agent_alias: "builder",
      channel_id: null,
      created_at: "2026-10-01T15:00:00Z",
      last_activity: new Date(Date.now() - 180000).toISOString(),
      message_count: 8,
    },
    {
      session_id: "review-api",
      session_key: "gw_review-api",
      name: "Review the API changes",
      agent_alias: "reviewer",
      channel_id: null,
      created_at: "2026-10-01T14:00:00Z",
      last_activity: new Date(Date.now() - 3600000).toISOString(),
      message_count: 12,
    },
  ];
  const codeHistory = () => [
    {
      session_id: "code-one",
      session_key: "code-one",
      agent_alias: "builder",
      channel_id: null,
      name: "",
      created_at: new Date().toISOString(),
      last_activity: new Date().toISOString(),
      message_count: 3,
      state: codeRunning ? "running" : "idle",
      interaction_surface: "zerocode_code",
    },
  ];
  const sections = [
    {
      key: "agents",
      label: "Agents",
      group: "Foundation",
      shape: "one_tier_alias_map",
      ready: true,
      has_picker: true,
      help: "Configure agents.",
      cost_category: "",
    },
    {
      key: "gateway",
      label: "Gateway",
      group: "Network",
      shape: "direct_form",
      ready: true,
      has_picker: false,
      help: "Gateway settings.",
      cost_category: "",
    },
  ];
  const fields = [
    { path: "agents.builder.env", kind: "secret-map", value: "{}", populated: true, is_secret: true, type_hint: "Map", category: "Agent", tab: "Advanced" },
    {
      path: "agents.builder.max_tool_iterations",
      kind: "integer",
      value: "20",
      populated: true,
      is_secret: false,
      type_hint: "usize",
      category: "Agent",
      tab: "Advanced",
    },
    {
      path: "agents.builder.enabled",
      kind: "bool",
      value: "true",
      populated: true,
      is_secret: false,
      type_hint: "bool",
      category: "Agent",
      tab: "General",
    },
    {
      path: "gateway.port",
      kind: "integer",
      value: "42617",
      populated: true,
      is_secret: false,
      type_hint: "u16",
      category: "Network",
      tab: "Network",
    },
  ];
  await context.route("**/api/**", async (route) => {
    const u = new URL(route.request().url());
    let body = {};
    if (u.pathname === "/api/status")
      body = {
        version: "0.8.5",
        uptime_seconds: 3600,
        gateway_port: 3000,
        locale: "en",
        check_updates: false,
        health: { components: {} },
        channels: {},
        process: {},
        nodes: { connected: [], mdns_peers: [] },
      };
    else if (u.pathname === "/api/cost")
      body = { daily_cost_usd: 1.24, monthly_cost_usd: 8.50, session_cost_usd: 1.24, total_tokens: 12000, request_count: 8, by_model: {}, by_agent: {} };
    else if (u.pathname === "/api/workspace")
      body = {
        agents: capabilities ? ["builder", "reviewer"] : [],
        code: capabilities,
        workflows: capabilities,
        session_persistence: true,
      };
    else if (u.pathname === "/api/quickstart/state")
      body = { quickstart_completed: !freshInstall, agents: freshInstall ? [] : ["builder", "reviewer"], model_provider_types: [], risk_presets: [], runtime_presets: [], memory_kinds: [], channels: [], unassigned_channels: [], channel_types: [], personality_files: [] };
    else if (u.pathname === "/api/sessions") body = { sessions };
    else if (u.pathname === "/api/sessions/running")
      body = { sessions: [{ session_id: "review-api" }] };
    else if (u.pathname.endsWith("/messages"))
      body = {
        session_persistence: true,
        messages: [
          { role: "user", content: "Help plan a release." },
          {
            role: "assistant",
            content: "Let’s start with the release checklist.",
          },
        ],
      };
    else if (u.pathname.endsWith("/state")) body = { state: "idle" };
    else if (u.pathname === "/api/sops/runs")
      body = {
        runs: [
          {
            run_id: "run-one",
            sop_name: "Release checks",
            active: true,
            current_step: 1,
            total_steps: 4,
            started_at: new Date().toISOString(),
            status: "running",
          },
        ],
      };
    else if (u.pathname === "/api/config/sections") body = { sections };
    else if (u.pathname === "/api/config/map-keys")
      body = {
        keys:
          u.searchParams.get("path") === "agents"
            ? ["builder", "reviewer"]
            : [],
      };
    else if (u.pathname === "/api/config/list")
      body = {
        entries: fields.filter(
          (f) =>
            !u.searchParams.get("prefix") ||
            f.path.startsWith(u.searchParams.get("prefix")),
        ),
        drifted: [],
      };
    else if (u.pathname === "/api/config/sections/agents")
      body = {
        items: [
          { key: "builder", label: "builder", badge: "configured" },
          { key: "reviewer", label: "reviewer", badge: "configured" },
        ],
      };
    else if (u.pathname === "/api/config/drift") body = { drifted: [] };
    else if (u.pathname === "/api/config/reload-status")
      body = { pending_reload: false };
    else if (u.pathname === "/api/config")
      body = { type: "object", properties: {} };
    else if (u.pathname.endsWith("/workspace/list"))
      body = {
        path: "",
        entries: [
          { name: "README.md", kind: "file" },
          { name: "src", kind: "dir" },
        ],
      };
    else if (u.pathname.endsWith("/workspace/read"))
      body = {
        path: "README.md",
        is_text: true,
        content:
          "# Example workspace\n\nA small project for interface verification.\n",
        encoding: "utf8",
        size: 72,
      };
    else if (u.pathname === "/api/events") {
      return route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: ": fixture\n\n",
      });
    }
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
  });
  let chatSockets = 0;
  let chatApprovals = 0;
  await context.routeWebSocket('**/ws/chat?*', (ws) => {
    chatSockets++;
    let pendingApproval = false;
    ws.onMessage((raw) => {
      const frame = JSON.parse(raw);
      if (frame.type === 'message') {
        pendingApproval = true;
        ws.send(JSON.stringify({ type: 'chunk', content: 'Checking the task.' }));
        ws.send(JSON.stringify({ type: 'approval_request', request_id: 'chat-approval', tool: 'shell', arguments_summary: 'Run harmless fixture', timeout_secs: 120 }));
      } else if (frame.type === 'approval_response' && pendingApproval) {
        // The gateway resolves each parked approval once, including duplicate replies.
        pendingApproval = false;
        chatApprovals++;
        ws.send(JSON.stringify({ type: 'done', full_response: 'Chat task complete.' }));
      }
    });
  });
  let codeSockets = 0;
  await context.routeWebSocket("**/ws/code", (ws) => {
    codeSockets++;
    let promptId;
    let promptGeneration;
    const reply = (id, result) =>
      ws.send(JSON.stringify({ jsonrpc: "2.0", id, result }));
    const update = (params) =>
      ws.send(
        JSON.stringify({
          jsonrpc: "2.0",
          method: "session/update",
          params: { session_id: "code-one", ...params },
        }),
      );
    ws.onMessage((raw) => {
      const frame = JSON.parse(raw);
      const { id, method } = frame;
      if (method === "initialize") reply(id, { protocol_version: 1 });
      else if (method === "session/list-acp")
        reply(id, { sessions: codeHistory() });
      else if (method === "session/new")
        reply(id, {
          session_id: "code-one",
          agent_alias: "builder",
          workspace_dir: "/example/workspace",
        });
      else if (method === "session/messages")
        reply(id, {
          messages: [
            { role: "user", content: "Explore this project." },
            {
              role: "assistant",
              content:
                "This workspace contains a README and a source directory.",
            },
          ],
        });
      else if (method === "session/state")
        reply(id, { state: codeRunning ? "running" : "idle" });
      else if (method === "session/prompt") {
        promptId = id;
        promptGeneration = frame.params.client_turn_generation;
        codeRunning = true;
        update({
          type: "agent_message_chunk",
          text: "I’ll inspect the workspace.",
        });
        update({
          type: "approval_request",
          request_id: "approval-one",
          tool_name: "shell",
          arguments_summary: "Run project tests",
          timeout_secs: 120,
        });
      } else if (method === "session/approve") {
        assert.equal(frame.params.decision, "allow_once");
        reply(id, { acknowledged: true });
        codeRunning = false;
        update({ type: "turn_complete", client_turn_generation: promptGeneration - 1, content: "Stale predecessor output" });
        update({ type: "turn_complete", client_turn_generation: promptGeneration, content: "The project tests passed." });
        reply(promptId, {});
      } else if (method === "session/cancel") {
        reply(id, {});
        codeRunning = false;
        update({ type: "turn_complete", content: "Stopped." });
        reply(promptId, {});
      }
    });
  });
  const page = await context.newPage();
  globalThis.testPage = page;
  page.on("console", (msg) => {
    if (msg.type() === "error") console.error("CONSOLE", msg.text());
  });
  page.on("requestfailed", (req) =>
    console.error("FAILED", req.url(), req.failure()),
  );
  const errors = [];
  page.on("pageerror", (e) => {
    errors.push(e.message);
    console.error("PAGE ERROR:", e.message);
  });
  await page.goto(appUrl);
  await page.getByRole('navigation', { name: 'Primary navigation', exact: true }).waitFor();
  assert.equal(new URL(page.url()).pathname, '/');
  await page.getByRole('link', { name: 'Plugins', exact: true }).waitFor();
  await page.getByText(':3000', { exact: true }).waitFor();
  await page.waitForTimeout(500); // Let Dashboard's entrance animation settle for visual evidence.
  await page.screenshot({ path: out + '/classic-desktop.png', fullPage: true });
  for (const invalid of ['unknown', '{broken', 'null']) {
    await page.evaluate((value) => localStorage.setItem('zeroclaw-layout', value), invalid);
    await page.reload();
    await page.getByRole('navigation', { name: 'Primary navigation', exact: true }).waitFor();
  }
  await page.addInitScript(() => {
    const get = Storage.prototype.getItem;
    Storage.prototype.getItem = function(key) {
      if (key === 'zeroclaw-layout' && sessionStorage.getItem('block-layout') === 'yes') throw new DOMException('Fixture storage denied', 'SecurityError');
      return get.call(this, key);
    };
  });
  await page.evaluate(() => sessionStorage.setItem('block-layout', 'yes'));
  await page.reload();
  await page.getByRole('navigation', { name: 'Primary navigation', exact: true }).waitFor();
  await page.evaluate(() => sessionStorage.removeItem('block-layout'));
  await page.setViewportSize({ width: 390, height: 844 });
  await page.getByRole('button', { name: 'Primary navigation', exact: true }).click();
  await page.getByRole('dialog').getByRole('link', { name: 'Plugins', exact: true }).waitFor();
  await page.screenshot({ path: out + '/classic-mobile-drawer.png', fullPage: true });
  await page.keyboard.press('Escape');
  await page.setViewportSize({ width: 1440, height: 980 });
  freshInstall = true;
  await page.goto(appUrl);
  await page.waitForURL('**/quickstart');
  assert.equal(await page.getByRole('radio', { name: /^Classic/ }).isChecked(), true);
  freshInstall = false;
  await page.getByRole('radio', { name: /^Focused/ }).check();
  assert.equal(await page.evaluate(() => localStorage.getItem('zeroclaw-layout')), 'focused');
  await page.screenshot({ path: out + '/setup-focused-choice.png', fullPage: true });
  await page.goto(appUrl);
  await page.reload();

  for (const name of ["Agent", "Code", "S.O.P", "Admin"]) {
    await page.getByRole("link", { name, exact: true }).waitFor();
  }
  assert.equal(await page.getByRole("navigation", { name: "Primary" }).count(), 0);
  await page.getByRole("heading", { name: "Your workspace", exact: true }).waitFor();
  await page.getByText("Plan the next release", { exact: true }).waitFor();
  await page.screenshot({ path: out + "/home-desktop.png", fullPage: true });
  await page.keyboard.press("Control+k");
  await page
    .getByRole("textbox", { name: "Search features, sessions, settings…" })
    .fill("max tool iterations");
  await page
    .getByRole("option")
    .filter({ hasText: "max tool iterations" })
    .click();
  await page.locator('[id="agents.builder.max_tool_iterations"]').waitFor();
  await page.waitForTimeout(300);
  assert.equal(
    await page.evaluate(() => document.activeElement.id),
    "agents.builder.max_tool_iterations",
  );
  await page.screenshot({
    path: out + "/settings-search-result.png",
    fullPage: true,
  });
  assert.equal(new URL(page.url()).pathname, "/");
  await page.keyboard.press("Escape");
  await page.getByRole("dialog", { name: "Settings", exact: true }).waitFor({ state: "hidden" });
  await page.keyboard.press("Control+k");
  await page.getByRole("textbox", { name: "Search features, sessions, settings…" }).fill("builder env");
  await page.getByRole("option").filter({ hasText: "env" }).click();
  const secretMap = page.locator('[data-config-field="agents.builder.env"]');
  await secretMap.waitFor();
  await page.waitForTimeout(300);
  assert.equal(await secretMap.locator('input').first().evaluate(el => el === document.activeElement), true, 'search focuses the new grouped secret map');
  await page.keyboard.press("Escape");
  await page.getByRole("dialog", { name: "Settings", exact: true }).waitFor({ state: "hidden" });
  await page.getByRole("link", { name: "Code", exact: true }).click();
  await page
    .getByRole("textbox", { name: "Describe a code change or ask a question…" })
    .waitFor();
  await page.getByText("This workspace contains a README and a source directory.", { exact: true }).waitFor();
  await page
    .getByRole("textbox", { name: "Describe a code change or ask a question…" })
    .fill("Run the project tests");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await page.getByText("Run project tests", { exact: true }).waitFor();
  await page.getByRole("button", { name: "README.md", exact: true }).click();
  await page
    .getByText("A small project for interface verification.", { exact: true })
    .waitFor();
  await page.waitForTimeout(250);
  await page.screenshot({ path: out + "/code-approval.png", fullPage: true });
  const socketsBeforeLayout = codeSockets;
  await chooseLayout(page, 'Classic');
  await page.getByText('Run project tests', { exact: true }).waitFor();
  assert.equal(codeSockets, socketsBeforeLayout);
  assert.equal(codeRunning, true);
  await chooseLayout(page, 'Focused');
  assert.equal(codeSockets, socketsBeforeLayout);

  await page.getByRole("link", { name: "Home", exact: true }).first().click();
  await page.getByRole("link", { name: "Agent", exact: true }).waitFor();
  assert.equal(codeRunning, true);
  await page.getByRole("link", { name: "Needs your attention · Code" }).click();
  await page.getByText("Run project tests", { exact: true }).waitFor();
  await page.getByRole("button", { name: "Approve", exact: true }).click();
  await page.getByText("The project tests passed.", { exact: true }).waitFor();
  assert.equal(codeRunning, false);
  assert.equal(await page.getByText("Stale predecessor output", { exact: true }).count(), 0);
  await page.screenshot({ path: out + "/code-complete.png", fullPage: true });
  // A task resumed from another connection has no terminal event on this one.
  codeRunning = true;
  await page.reload();
  await page.getByText("Working…", { exact: true }).waitFor();
  codeRunning = false;
  await page.getByText("Ready", { exact: true }).waitFor();
  await page.getByText("This workspace contains a README and a source directory.", { exact: true }).waitFor();
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);
  await page.screenshot({ path: out + "/code-mobile.png", fullPage: true });
  await page.getByRole("link", { name: "Home", exact: true }).last().click();
  await page.getByRole("link", { name: "Agent", exact: true }).waitFor();
  await page.getByRole("heading", { name: "Your workspace", exact: true }).waitFor();
  await page.screenshot({ path: out + "/home-mobile.png", fullPage: true });
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
    true,
  );
  // Chat sockets, an unsent composer, and pending approval survive presentation changes.
  await page.setViewportSize({ width: 1440, height: 980 });
  await page.getByRole('link', { name: 'Agent', exact: true }).click();
  const chatInput = page.getByPlaceholder('Type a message...');
  await chatInput.fill('Keep my chat draft');
  const openedChats = chatSockets;
  await chooseLayout(page, 'Classic');
  assert.equal(await chatInput.inputValue(), 'Keep my chat draft');
  await chooseLayout(page, 'Focused');
  assert.equal(await chatInput.inputValue(), 'Keep my chat draft');
  await chatInput.press('Enter');
  await page.getByText('Run harmless fixture', { exact: true }).waitFor();
  await chooseLayout(page, 'Classic');
  await page.getByRole('link', { name: 'Dashboard', exact: true }).first().click();
  await page.getByRole('link', { name: /Needs your attention · builder/ }).click();
  await page.getByRole('button', { name: 'Approve', exact: true }).click();
  await page.getByText('Chat task complete.', { exact: true }).waitFor();
  assert.equal(chatSockets, openedChats);
  assert.equal(chatApprovals, 1);
  await chooseLayout(page, 'Focused');
  await page.getByRole('link', { name: 'Home', exact: true }).first().click();
  // Home keeps the workspace entry points; unavailable workspaces offer setup.
  capabilities = false;
  await page.getByRole("link", { name: "Code", exact: true }).click();
  await page.reload();
  await page.getByRole("link", { name: "Feature settings", exact: true }).waitFor();
  await page.getByRole("link", { name: "Home", exact: true }).click();
  capabilities = true;
  await page.evaluate(() => localStorage.setItem('zeroclaw-theme', JSON.stringify({ theme: 'light', accent: 'cyan' })));
  await page.setViewportSize({ width: 1440, height: 980 });
  await page.reload();
  await page.getByRole('link', { name: 'Code', exact: true }).waitFor();
  await page.getByRole('heading', { name: 'Your workspace', exact: true }).waitFor();
  await page.getByText('Plan the next release', { exact: true }).waitFor();
  await page.screenshot({ path: out + '/home-light.png', fullPage: true });
  await page.getByRole('button', { name: 'More options', exact: true }).click();
  await page.getByRole('button', { name: 'Appearance', exact: true }).click();
  assert.equal(await page.getByRole('radio', { name: /^Focused/ }).isChecked(), true);
  await page.waitForTimeout(350); // Wait for the Appearance entrance animation before visual proof.
  await page.screenshot({ path: out + '/appearance-focused-choice.png', fullPage: true });
  await page.getByRole('radio', { name: /^Classic/ }).check();
  await page.waitForTimeout(350);
  await page.screenshot({ path: out + '/appearance-classic-choice.png', fullPage: true });
  await page.keyboard.press('Escape');
  await page.reload();
  await page.getByRole('navigation', { name: 'Primary navigation', exact: true }).waitFor();
  assert.equal(await page.evaluate(() => JSON.parse(localStorage.getItem('zeroclaw-theme')).theme), 'light');
  await page.getByText(':3000', { exact: true }).waitFor();
  await page.screenshot({ path: out + '/classic-light.png', fullPage: true });
  assert.deepEqual(errors, []);
  console.log(JSON.stringify({ passed: true, codeSockets, evidence: out }));
  await browser.close();
})().catch(async (e) => {
  console.error(e);
  if (globalThis.testPage) {
    console.error(await testPage.locator("body").innerText());
    await testPage.screenshot({ path: out + "/failure.png", fullPage: true });
  }
  process.exit(1);
});
