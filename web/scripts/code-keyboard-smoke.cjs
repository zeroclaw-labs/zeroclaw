// Exercise the built dashboard with synthetic gateway and Code RPC frames.
const assert = require("node:assert/strict");
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright");

const base = process.env.WEB_SMOKE_URL || "http://127.0.0.1:5178";
const models = [
  { model: "claude-fable-5-2", model_provider: "anthropic.test", reasoning_options: ["low", "high"] },
  { model: "claude-sonnet-5-5", model_provider: "anthropic.test", reasoning_options: ["low", "high"] },
];
const makeSession = (id, agent, name, day) => ({
  session_id: id, session_key: id, agent_alias: agent, name, channel_id: null,
  created_at: `2026-10-${day}T12:00:00Z`, last_activity: `2026-10-${day}T12:00:00Z`,
  interaction_surface: "zerocode_code", message_count: 2,
});
const first = makeSession("fixture-a", "builder", "Build the release", "02");
const second = makeSession("fixture-b", "reviewer", "Review the release", "01");

(async () => {
  const browser = await chromium.launch({ channel: "chrome", headless: true });
  let page;
  try {
    const context = await browser.newContext({ viewport: { width: 1440, height: 950 } });
    await context.addInitScript(() => {
      localStorage.setItem("zeroclaw_token", "zc_synthetic_code_keyboard");
      localStorage.setItem("zeroclaw-layout", "focused");
      localStorage.setItem("zeroclaw_locale", "en");
    });
    await context.route("**/api/**", (route) => {
      const path = new URL(route.request().url()).pathname;
      const responses = {
        "/api/status": { model: models[0].model, locale: "en", version: "0.9.0", check_updates: false, gateway_port: 42628 },
        "/api/quickstart/state": { quickstart_completed: true, agents: ["builder", "reviewer"] },
        "/api/workspace": { code: true, agents: ["builder", "reviewer"], workflows: true, session_persistence: true },
        "/api/config/drift": { drifted: [] },
        "/api/config/reload-status": { pending_reload: false },
        "/api/sops": { sops: [] },
        "/api/sops/runs": { runs: [] },
      };
      if (path === "/api/events") return route.fulfill({ contentType: "text/event-stream", body: ": fixture\n\n" });
      if (path.endsWith("/workspace/list")) return route.fulfill({ json: { path: "", entries: [] } });
      return route.fulfill({ json: responses[path] ?? {} });
    });
    const messages = {
      "fixture-a": [{ role: "assistant", content: Array.from({ length: 45 }, (_, i) => `[L${i}](#fixture-${i})`).join(" ") }],
      "fixture-b": [{ role: "assistant", content: "The review conversation." }],
    };
    const settings = {};
    for (const row of [first, second]) {
      settings[row.session_id] = { ...models[0], models, can_change: true, reasoning_effort: null };
    }
    const active = new Map();
    const approvals = [];
    const edits = [];
    let lastSocket;
    await context.routeWebSocket("**/ws/code", (ws) => {
      lastSocket = ws;
      const reply = (id, result) => ws.send(JSON.stringify({ jsonrpc: "2.0", id, result }));
      const update = (session_id, fields) => ws.send(JSON.stringify({
        jsonrpc: "2.0", method: "session/update", params: { session_id, ...fields },
      }));
      ws.onMessage((raw) => {
        const { id, method, params } = JSON.parse(raw);
        const sid = params?.session_id;
        if (method === "initialize") reply(id, { protocol_version: 1 });
        if (method === "session/list-acp") reply(id, { sessions: [first, second].map((row) => ({
          ...row, state: active.has(row.session_id) ? "running" : "idle",
        })) });
        if (method === "session/new") reply(id, { session_id: sid, agent_alias: params.agent_alias });
        if (method === "session/messages") reply(id, { messages: messages[sid], has_older: false });
        if (method === "session/state") reply(id, {
          state: active.has(sid) ? "running" : "idle", code_settings: settings[sid],
        });
        if (method === "session/configure") {
          assert.equal(active.has(sid), false, "settings reached an active turn");
          const chosen = models.find((item) => item.model === params.overrides.model);
          settings[sid] = { ...settings[sid], ...chosen, ...params.overrides };
          edits.push({ sid, ...params.overrides });
          reply(id, { overrides: params.overrides });
        }
        if (method === "session/prompt") {
          active.set(sid, { id, generation: params.client_turn_generation });
          messages[sid].push({ role: "user", content: params.prompt });
          update(sid, { type: "tool_call", tool_call_id: "test-" + sid, name: "shell", raw_input: { command: "cargo test" } });
          if (sid === second.session_id) {
            update(sid, { type: "approval_request", request_id: "approval-" + id, tool_name: "shell", arguments_summary: "Review tests", timeout_secs: 90 });
          }
        }
        if (method === "session/approve" || method === "session/cancel") {
          const turn = active.get(sid);
          assert.ok(turn, "no active turn for decision");
          if (method === "session/approve") {
            assert.equal(params.request_id, "approval-" + turn.id);
            approvals.push(sid);
          }
          active.delete(sid);
          const content = sid === first.session_id ? "Build finished." : "Review finished.";
          messages[sid].push({ role: "assistant", content });
          update(sid, { type: "tool_result", tool_call_id: "test-" + sid, name: "shell", raw_output: '{"exit_code":0}' });
          update(sid, { type: "turn_complete", content, outcome: "completed", client_turn_generation: turn.generation });
          reply(id, {});
          reply(turn.id, {});
        }
      });
    });

    page = await context.newPage();
    page.setDefaultTimeout(12_000);
    const errors = [];
    page.on("pageerror", (error) => { errors.push(error.message); console.error(error.stack); });
    page.on("requestfailed", (request) => {
      if (request.url().startsWith(base)) console.error(request.url(), request.failure()?.errorText);
    });
    await page.goto(new URL("/code?agent=builder&session=fixture-a", base).toString());
    const chat = page.getByRole("region", { name: "Code conversation" });
    const composer = chat.getByRole("textbox", { name: /Describe a code change/ });
    const navigate = async () => {
      await page.keyboard.press("Escape");
      await page.getByRole("button", { name: "Navigate", exact: true }).waitFor();
    };
    const hint = async (key) => {
      await page.keyboard.down("Shift");
      for (const letter of key) await page.keyboard.press("Key" + letter.toUpperCase());
      await page.keyboard.up("Shift");
    };

    await page.getByRole("link", { name: "L0", exact: true }).waitFor();
    await navigate();
    await page.keyboard.down("Shift");
    const badges = page.locator("kbd.fixed");
    assert.ok(await badges.count() > 26);
    assert.ok((await badges.allTextContents()).some((text) => text.length === 2));
    await page.keyboard.up("Shift");

    await page.keyboard.press("i");
    assert.equal(await composer.evaluate((element) => element === document.activeElement), true);
    await composer.fill("Start build");
    await page.keyboard.press("Control+Enter");
    await chat.getByRole("list", { name: "Progress" }).getByText("Running tests", { exact: true }).waitFor();
    await composer.fill("Draft for A");
    await navigate();
    await hint("m");
    const modelPicker = page.getByRole("dialog", { name: "Model", exact: true });
    await modelPicker.getByRole("combobox").fill("sonnet");
    await page.keyboard.press("Enter");
    await chat.getByText("Applies next turn").waitFor();
    await hint("r");
    const effortPicker = page.getByRole("dialog", { name: "Reasoning", exact: true });
    await effortPicker.getByRole("combobox").fill("high");
    await page.keyboard.press("Enter");
    assert.deepEqual(edits, []);

    await page.keyboard.press("j");
    await chat.getByText("The review conversation.").waitFor();
    assert.equal(active.has(first.session_id), true);
    await page.keyboard.press("i");
    await composer.fill("Start review");
    await page.keyboard.press("Control+Enter");
    await chat.getByText("Review tests", { exact: true }).waitFor();
    await composer.fill("Draft for B");
    await chat.getByRole("button", { name: "Approve", exact: true }).click();
    await chat.getByText("Review finished.", { exact: true }).waitFor();
    assert.deepEqual(approvals, [second.session_id]);
    assert.equal(active.has(first.session_id), true);
    assert.equal(await chat.locator("details[open]").count(), 0);
    await chat.getByText("Activity (1)", { exact: true }).click();
    await chat.getByText("Tests passed", { exact: true }).click();
    await chat.getByText('"exit_code":0', { exact: false }).waitFor();

    await navigate();
    await page.keyboard.press("l");
    await page.waitForURL("**/sops");
    await page.keyboard.press("h");
    await page.waitForURL("**/code?agent=reviewer&session=fixture-b");
    await page.waitForFunction(() => document.activeElement?.matches("[data-workspace-composer]"));
    assert.equal(await composer.inputValue(), "Draft for B");
    await navigate();
    await page.keyboard.press("k");
    await page.waitForURL("**/code?agent=builder&session=fixture-a");
    assert.equal(await composer.inputValue(), "Draft for A");
    await chat.getByRole("button", { name: "Cancel", exact: true }).click();
    await chat.getByRole("button", { name: "Model · claude-sonnet-5-5", exact: true }).waitFor();
    await chat.getByRole("button", { name: "Reasoning · High", exact: true }).waitFor();
    assert.deepEqual(edits, [{ sid: first.session_id, model_provider: "anthropic.test", model: "claude-sonnet-5-5", reasoning_effort: "high" }]);

    const files = chat.getByRole("button", { name: "Workspace files", exact: true });
    assert.equal(await files.getAttribute("aria-expanded"), "false");
    await files.click();
    for (const [name, arrow] of [["Resize Code sessions", "ArrowRight"], ["Resize workspace files", "ArrowLeft"]]) {
      const grip = page.getByRole("separator", { name });
      const initial = Number(await grip.getAttribute("aria-valuenow"));
      await grip.press(arrow);
      assert.equal(Number(await grip.getAttribute("aria-valuenow")), initial + 16);
    }
    const width = await page.getByRole("separator", { name: "Resize workspace files" }).getAttribute("aria-valuenow");
    await page.reload();
    assert.equal(await page.getByRole("separator", { name: "Resize workspace files" }).getAttribute("aria-valuenow"), width);
    await page.setViewportSize({ width: 390, height: 844 });
    const drawer = page.getByRole("dialog", { name: "Workspace files" });
    await drawer.waitFor();
    assert.equal(await drawer.evaluate((element) => element.getBoundingClientRect().width), 390);
    await page.keyboard.press("Escape");
    await drawer.waitFor({ state: "hidden" });
    assert.equal(await page.evaluate(() => document.documentElement.scrollWidth <= innerWidth), true);

    await page.setViewportSize({ width: 1440, height: 950 });
    await navigate();
    await page.keyboard.press("j");
    await page.waitForURL("**/code?agent=reviewer&session=fixture-b");
    await page.keyboard.press("i");
    await composer.fill("Review after reload");
    await page.keyboard.press("Control+Enter");
    await chat.getByText("Review tests", { exact: true }).waitFor();
    const previous = lastSocket;
    active.delete(second.session_id);
    messages[second.session_id].push({ role: "assistant", content: "Recovered the retained transcript." });
    await previous.close();
    await chat.getByText("Recovered the retained transcript.", { exact: true }).waitFor();
    assert.equal(await chat.getByRole("button", { name: "Approve", exact: true }).count(), 0);
    assert.notEqual(lastSocket, previous);
    assert.deepEqual(errors, []);
    console.log("Code keyboard, background chats, queued settings, layout and reconnect checks passed.");
  } catch (error) {
    if (page) {
      console.error(page.url(), await page.locator("body").ariaSnapshot({ timeout: 2000 }).catch(() => ""));
      console.error(await page.locator("#root").innerHTML({ timeout: 2000 }).then((text) => text.slice(0, 500)).catch(() => ""));
    }
    throw error;
  } finally {
    await browser.close();
  }
})().catch((error) => { console.error(error); process.exitCode = 1; });
