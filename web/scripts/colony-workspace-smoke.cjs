// Browser-boundary proof for the production Colony renderer. HTTP and model
// responses are synthetic; gateway/runtime tests own execution and policy proof.
const { chromium } = require(process.env.PLAYWRIGHT_MODULE || "playwright");
const fs = require("node:fs");
const path = require("node:path");
const { createHash } = require("node:crypto");
const { execFileSync } = require("node:child_process");
const { createServer } = require("node:http");
const assert = require("node:assert/strict");
const out = process.env.WEB_SMOKE_OUTPUT || "/tmp/zeroclaw-colony-evidence";
let url = process.env.WEB_SMOKE_URL || "http://127.0.0.1:5180";
let distServer;
fs.mkdirSync(out, { recursive: true });
(async () => {
  if (process.env.WEB_SMOKE_DIST === "1") {
    const dist = path.resolve(__dirname, "../dist");
    assert.ok(
      fs.existsSync(path.join(dist, "index.html")),
      "run npm run build before checking the compiled renderer",
    );
    // Static fixture transport only: mirror the gateway's /_app prefix strip.
    // Every API/provider response below remains explicitly synthetic.
    distServer = createServer((request, response) => {
      const pathname = new URL(request.url, "http://127.0.0.1").pathname;
      const relative = pathname.startsWith("/_app/")
        ? pathname.slice(5)
        : "/index.html";
      const filename = path.resolve(dist, "." + relative);
      if (
        !filename.startsWith(dist + path.sep) ||
        !fs.existsSync(filename) ||
        !fs.statSync(filename).isFile()
      ) {
        response.writeHead(404);
        response.end();
        return;
      }
      const contentType =
        {
          ".html": "text/html",
          ".js": "text/javascript",
          ".css": "text/css",
          ".png": "image/png",
          ".svg": "image/svg+xml",
          ".json": "application/json",
        }[path.extname(filename)] || "application/octet-stream";
      response.writeHead(200, { "Content-Type": contentType });
      response.end(fs.readFileSync(filename));
    });
    await new Promise((resolve) => distServer.listen(0, "127.0.0.1", resolve));
    url = `http://127.0.0.1:${distServer.address().port}`;
  }
  const browser = await chromium.launch({ channel: "chrome", headless: true });
  const context = await browser.newContext({
    viewport: { width: 1440, height: 980 },
  });
  await context.addInitScript(() => {
    localStorage.setItem("zeroclaw_token", "zc_synthetic_colony_fixture");
    localStorage.setItem("zeroclaw_locale", "en");
  });
  const agents = ["builder", "reviewer", "resume"].map((alias) => ({
    alias,
    enabled: true,
    core_command: {
      builder: "Find relevant job postings",
      reviewer: "Evaluate suitability against user preferences",
      resume: "Prepare a tailored resume for suitable jobs",
    }[alias],
    colony_id: null,
    model_provider: "example_model",
    risk_profile: "restricted",
    runtime_profile: "standard",
    active_turns: 0,
    access: {
      allowed_tools: ["web_search", "file_write"],
      allowed_commands: [],
      allowed_roots: ["/example/workspace"],
      workspace_only: true,
      always_ask: ["submit_application"],
      excluded_tools: ["shell"],
      network_domains: {
        browser: ["jobs.example.test"],
        http_request: [],
        web_fetch: ["jobs.example.test"],
      },
      max_actions_per_hour: 30,
      max_cost_per_day_cents: 100,
      daily_limit_usd: 1,
      monthly_limit_usd: 10,
      cost_tracking_enabled: true,
    },
    context_sources: [],
  }));
  const state = {
    colonies: [],
    agents,
    channels: [{ id: "email.jobs", label: "email.jobs", enabled: true }],
    risk_profiles: ["restricted", "read_only"],
    runtime_profiles: ["standard"],
    risk_profile_access: {
      restricted: {
        allowed_tools: ["web_search", "file_write"],
        allowed_commands: [],
        allowed_roots: ["/example/workspace"],
        workspace_only: true,
        always_ask: ["submit_application"],
        excluded_tools: ["shell"],
      },
      read_only: {
        allowed_tools: ["web_search"],
        allowed_commands: [],
        allowed_roots: ["/example/workspace"],
        workspace_only: true,
        always_ask: ["submit_application"],
        excluded_tools: ["shell", "file_write"],
      },
    },
    runtime_profile_access: {
      standard: { max_actions_per_hour: 30, max_cost_per_day_cents: 100 },
    },
  };
  const actions = [],
    creates = [],
    saves = [],
    chatSockets = [],
    messages = [];
  let clarification = 0;
  let failNextDetailRead = false;
  let proposeFutureTeam = false;
  await context.route("**/api/**", async (route) => {
    const request = route.request(),
      u = new URL(request.url()),
      method = request.method();
    const data =
      method === "POST" || method === "PUT" ? request.postDataJSON() : null;
    let body = {};
    if (u.pathname === "/api/status")
      body = {
        version: "0.8.5",
        locale: "en",
        check_updates: false,
        health: { components: {} },
        channels: {},
        process: {},
        nodes: { connected: [], mdns_peers: [] },
      };
    else if (u.pathname === "/api/workspace")
      body = {
        agents: agents.map((a) => a.alias),
        code: false,
        workflows: false,
        session_persistence: true,
      };
    else if (u.pathname === "/api/quickstart/state")
      body = { quickstart_completed: true, agents: agents.map((a) => a.alias) };
    else if (u.pathname === "/api/config/map-keys")
      body = {
        keys:
          u.searchParams.get("path") === "agents"
            ? agents.map((a) => a.alias)
            : [],
      };
    else if (u.pathname === "/api/config/list")
      body = {
        entries: agents.map((a) => ({
          path: `agents.${a.alias}.enabled`,
          populated: true,
          value: "true",
          kind: "bool",
          is_secret: false,
        })),
        drifted: [],
      };
    else if (u.pathname === "/api/config/sections") body = { sections: [] };
    else if (u.pathname === "/api/config/drift") body = { drifted: [] };
    else if (u.pathname === "/api/config/reload-status")
      body = { pending_reload: false };
    else if (u.pathname === "/api/sessions") body = { sessions: [] };
    else if (u.pathname === "/api/sessions/running") body = { sessions: [] };
    else if (u.pathname === "/api/colonies/context") {
      assert.ok(
        state.agents.some(
          (agent) => agent.alias === u.searchParams.get("agent"),
        ),
        "prior context must resolve an existing agent, not a draft Queen",
      );
      body = {
        sources: [
          {
            key: "memory:job_preferences",
            category: "core",
            preview:
              "Prefer remote engineering roles with a clear salary range.",
          },
        ],
      };
    } else if (u.pathname === "/api/colonies") {
      if (method === "POST") {
        creates.push(data);
        const colony = {
          id: data.id,
          definition: structuredClone(data.definition),
          goals: [],
        };
        state.colonies.push(colony);
        state.agents.push({
          ...structuredClone(agents[0]),
          alias: data.definition.queen,
          core_command:
            data.core_commands?.[data.definition.queen] ||
            "Coordinate the team",
          colony_id: colony.id,
        });
        for (const alias of [
          data.definition.queen,
          ...data.definition.members,
        ]) {
          const agent = state.agents.find((a) => a.alias === alias);
          agent.colony_id = colony.id;
          agent.core_command = data.core_commands[alias];
          Object.assign(agent, data.agent_profiles[alias]);
          Object.assign(
            agent.access,
            state.risk_profile_access[agent.risk_profile],
            state.runtime_profile_access[agent.runtime_profile],
          );
        }
        body = colony;
      } else body = state;
    } else if (u.pathname.startsWith("/api/colonies/")) {
      const parts = u.pathname.split("/"),
        colony = state.colonies.find((c) => c.id === parts[3]);
      assert.ok(colony, "fixture colony must exist");
      if (parts[4] === "clarify") {
        clarification++;
        body = {
          questions:
            clarification === 1
              ? ["Should applications be drafts for your review?"]
              : [],
          summary:
            "Find remote roles, evaluate fit, then prepare tailored application drafts for review.",
          assignments: [
            {
              agent: "builder",
              instruction: "Find postings with salary and remote work details.",
            },
            {
              agent: "reviewer",
              instruction: "Assess roles against approved preferences.",
            },
            { agent: "resume", instruction: "Prepare tailored resume drafts." },
          ],
          new_agents: proposeFutureTeam
            ? [
                {
                  alias: "job_stats",
                  core_command: "Compare approved job metrics",
                  template: "reviewer",
                  connections: [
                    { from: colony.definition.queen, to: "job_stats" },
                    { from: "job_stats", to: colony.definition.queen },
                    { from: "job_stats", to: "role_filter" },
                  ],
                },
                {
                  alias: "role_filter",
                  core_command: "Filter roles using approved criteria",
                  template: "reviewer",
                  connections: [
                    { from: colony.definition.queen, to: "role_filter" },
                    { from: "role_filter", to: colony.definition.queen },
                  ],
                },
              ]
            : [],
          connections: [],
        };
      } else if (parts[4] === "agents") {
        assert.deepEqual(data.expected_definition, colony.definition);
        state.agents.push({
          ...structuredClone(agents[0]),
          ...data,
          colony_id: colony.id,
        });
        colony.definition.members.push(data.alias);
        colony.definition.connections.push(...data.connections);
        body = colony;
      } else if (parts[4] === "goals" && !parts[5]) {
        if (data.proposal.new_agents.length) {
          assert.equal(
            data.approve_new_agents,
            true,
            "reviewed initial roles require one explicit operator batch approval",
          );
          for (const agent of data.proposal.new_agents) {
            state.agents.push({
              ...structuredClone(
                state.agents.find((source) => source.alias === agent.template),
              ),
              alias: agent.alias,
              core_command: agent.core_command,
              colony_id: colony.id,
            });
            colony.definition.members.push(agent.alias);
          }
          for (const agent of data.proposal.new_agents) {
            assert.ok(
              agent.connections.every(
                (edge) =>
                  [
                    colony.definition.queen,
                    ...colony.definition.members,
                  ].includes(edge.from) &&
                  [
                    colony.definition.queen,
                    ...colony.definition.members,
                  ].includes(edge.to),
              ),
            );
            colony.definition.connections.push(...agent.connections);
          }
        }
        const goal = {
          task: {
            id: `goal_${colony.goals.length + 1}`,
            agent: colony.definition.queen,
            status:
              colony.definition.start_mode === "automatic"
                ? "running"
                : "paused",
            kind: "goal",
            started_at: new Date().toISOString(),
          },
          goal: {
            task_id: `goal_${colony.goals.length + 1}`,
            objective: data.objective,
            effective_token_limit: data.token_limit,
            effective_cost_limit_usd: data.cost_limit_usd,
            pause_reason: "operator_paused",
            pause_description: null,
            blockers: [],
          },
          execution: {
            colony_id: colony.id,
            task_id: `goal_${colony.goals.length + 1}`,
            mode: data.mode,
            previous_goal_id: data.previous_goal_id,
            proposal: data.proposal,
            next_assignment: 0,
            active_child_id: null,
            active_inbox_id: null,
            summary_message_id: null,
            pending_plan: null,
            plan_rounds: 0,
            summarizing: false,
          },
          messages: [],
          error: null,
          approvals: [],
          turn_attached: false,
        };
        colony.goals.unshift(goal);
        body = goal;
      } else if (parts[4] === "goals" && parts[6] === "approvals") {
        const goal = colony.goals.find((g) => g.task.id === parts[5]);
        goal.approvals.find((a) => a.id === parts[7]).decision = data.approved;
        body = goal;
      } else if (parts[4] === "goals" && parts[6] === "clarify") {
        const goal = colony.goals.find((g) => g.task.id === parts[5]);
        assert.ok(data.answers.every((answer) => answer.answer.trim()));
        goal.execution.pending_plan.questions = [];
        goal.execution.pending_plan.summary =
          "Refine the salary shortlist within the existing review boundaries.";
        body = goal;
      } else if (parts[4] === "goals" && parts[6] === "control") {
        const goal = colony.goals.find((g) => g.task.id === parts[5]);
        actions.push(data.action);
        goal.task.status = {
          start: "running",
          resume: "running",
          pause: "paused",
          cancel: "cancelled",
          confirm_plan: "running",
        }[data.action];
        if (data.action === "confirm_plan") {
          assert.equal(goal.execution.pending_plan.questions.length, 0);
          for (const agent of goal.execution.pending_plan.new_agents) {
            state.agents.push({
              ...structuredClone(agents[0]),
              ...agent,
              colony_id: colony.id,
            });
            colony.definition.members.push(agent.alias);
            colony.definition.connections.push(...agent.connections);
          }
          goal.execution.pending_plan = null;
        }
        body = goal;
      } else if (parts[4] === "messages") {
        if (method === "POST")
          messages.push({
            id: `msg_${messages.length}`,
            colony_id: colony.id,
            task_id: colony.goals[0]?.task.id ?? null,
            sender: "user",
            recipient: data.recipient,
            content: data.content,
            created_at: new Date().toISOString(),
          });
        body = {
          messages: messages.filter(
            (m) =>
              m.colony_id === colony.id &&
              m.recipient ===
                (data?.recipient || u.searchParams.get("recipient")),
          ),
        };
      } else if (method === "PUT") {
        assert.deepEqual(
          data.expected_definition,
          colony.definition,
          "optimistic definition must match the canonical saved team",
        );
        saves.push(data);
        colony.definition = structuredClone(data.definition);
        body = colony;
      } else if (method === "GET" && failNextDetailRead) {
        failNextDetailRead = false;
        return route.fulfill({
          status: 503,
          contentType: "application/json",
          body: JSON.stringify({
            error: "Synthetic snapshot read failed after confirmed mutation.",
          }),
        });
      } else body = colony;
    } else if (u.pathname.endsWith("/messages"))
      body = { session_persistence: true, messages: [] };
    else if (u.pathname.endsWith("/state")) body = { state: "idle" };
    else if (u.pathname === "/api/events")
      return route.fulfill({
        status: 200,
        contentType: "text/event-stream",
        body: ": fixture\n\n",
      });
    await route.fulfill({
      status: 200,
      contentType: "application/json",
      body: JSON.stringify(body),
    });
  });
  await context.routeWebSocket("**/ws/chat?*", (ws) => {
    chatSockets.push(ws);
    ws.onMessage(() => {});
  });
  const page = await context.newPage();
  globalThis.testPage = page;
  const errors = [];
  const idle = () =>
    page
      .getByRole("button", { name: "Refresh", exact: true })
      .click({ trial: true });
  page.on("pageerror", (e) => {
    errors.push(e.message);
    console.error(e);
  });
  await page.goto(url + "/agent/builder");
  const chat = page.getByPlaceholder("Type a message...");
  await chat.fill("Keep this unsent conversation draft.");
  const socketCount = chatSockets.length;
  await page.getByRole("button", { name: "Colony", exact: true }).click();
  await page.getByTestId("colony-canvas").waitFor();
  await page.getByRole("button", { name: "Fit view", exact: true }).click();
  // The rectangle gesture exercises the actual pointer-to-world selection path.
  const a = await page.getByTestId("colony-node-agent:builder").boundingBox();
  const b = await page.getByTestId("colony-node-agent:reviewer").boundingBox();
  await page.mouse.move(Math.min(a.x, b.x) - 10, Math.min(a.y, b.y) - 10);
  await page.mouse.down();
  await page.mouse.move(
    Math.max(a.x + a.width, b.x + b.width) + 10,
    Math.max(a.y + a.height, b.y + b.height) + 10,
    { steps: 10 },
  );
  await page.screenshot({
    path: out + "/rectangle-selection.png",
    fullPage: true,
  });
  await page.mouse.up();
  await page.getByRole("button", { name: "Create colony · 2" }).waitFor();
  await page.getByRole("checkbox", { name: "Select agent resume" }).check();
  await page.getByRole("button", { name: "Create colony · 3" }).click();
  await page
    .getByRole("textbox", { name: "What should this team accomplish?" })
    .fill("Find suitable engineering roles and prepare application drafts.");
  await page
    .getByRole("textbox", { name: "What counts as success?" })
    .fill("Three suitable roles with tailored resumes.");
  await page.getByRole("textbox", { name: "Team name" }).fill("Job search");
  await page.getByText("Choose access profiles", { exact: true }).click();
  await page
    .getByRole("combobox", { name: "reviewer Risk profile", exact: true })
    .selectOption("read_only");
  await page.getByText("Prior context · 0", { exact: true }).click();
  await page
    .getByText("Prefer remote engineering roles with a clear salary range.")
    .first()
    .waitFor();
  await page
    .getByRole("checkbox")
    .filter({ hasText: "job_preferences" })
    .count();
  await page
    .locator("label")
    .filter({ hasText: "job_preferences" })
    .first()
    .locator("input")
    .check();
  await page.screenshot({
    path: out + "/setup-review-desktop.png",
    fullPage: true,
  });
  await page.getByRole("button", { name: "Clarify goal", exact: true }).click();
  await idle();
  await page
    .getByRole("textbox", {
      name: "Should applications be drafts for your review?",
    })
    .fill("Prepare drafts only. I will review and submit them.");
  await page.getByRole("button", { name: "Clarify goal", exact: true }).click();
  await page.getByText("Proposed assignments", { exact: true }).waitFor();
  await page
    .getByRole("button", { name: "Start goal", exact: true })
    .click({ trial: true });
  await page.screenshot({
    path: out + "/goal-review-desktop.png",
    fullPage: true,
  });
  await page.getByRole("button", { name: "Start goal", exact: true }).click();
  await page.getByRole("button", { name: "Pause", exact: true }).waitFor();
  assert.equal(creates.length, 1);
  assert.equal(creates[0].definition.baseline_context.length, 1);
  assert.equal(creates[0].definition.baseline_context[0].agent, "builder");
  assert.equal(creates[0].definition.connections.length, 6);
  assert.equal(creates[0].agent_profiles.reviewer.risk_profile, "read_only");
  assert.equal(
    chatSockets.length,
    socketCount,
    "Colony toggle must preserve chat sockets",
  );
  state.colonies[0].goals[0].approvals.push({
    id: "approval_one",
    goal_id: "goal_1",
    agent: "resume",
    tool: "file_write",
    arguments: { path: "/example/workspace/resume.md" },
    decision: null,
  });
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await idle();
  await page.getByRole("button", { name: "Approve once", exact: true }).click();
  assert.equal(state.colonies[0].goals[0].approvals[0].decision, true);
  await idle();
  await page
    .getByRole("button", { name: "Close Colony panel", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Connect from reviewer", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Connect to resume", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Goal progress", exact: true })
    .click();
  failNextDetailRead = true;
  await page.getByRole("button", { name: "Pause", exact: true }).click();
  await idle();
  await page
    .getByText("Synthetic snapshot read failed after confirmed mutation.", {
      exact: true,
    })
    .waitFor();
  await page.getByText("Unsaved changes", { exact: true }).waitFor();
  assert.equal(
    await page.getByRole("button", { name: "Resume", exact: true }).isEnabled(),
    true,
  );
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await idle();
  await page.getByRole("button", { name: "Resume", exact: true }).click();
  const pendingGoal = state.colonies[0].goals[0];
  pendingGoal.task.status = "paused";
  pendingGoal.goal.pause_reason = "needs_user_input";
  pendingGoal.execution.pending_plan = {
    questions: ["Should I prioritize transparent salary bands?"],
    summary: "A salary specialist can refine the shortlist.",
    assignments: [
      {
        agent: "salary_stats",
        instruction: "Compare the approved salary bands.",
      },
    ],
    new_agents: [
      {
        alias: "salary_stats",
        core_command: "Compare salary bands for shortlisted roles",
        template: "reviewer",
        connections: [
          { from: state.colonies[0].definition.queen, to: "salary_stats" },
        ],
      },
    ],
  };
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await idle();
  await page
    .getByRole("textbox", {
      name: "Should I prioritize transparent salary bands?",
    })
    .fill("Yes, prioritize those roles.");
  assert.equal(
    await page
      .getByRole("button", { name: "Resume", exact: true })
      .isDisabled(),
    true,
  );
  await page.getByRole("button", { name: "Clarify goal", exact: true }).click();
  await page
    .getByRole("button", { name: "Confirm plan and continue", exact: true })
    .waitFor();
  await page
    .getByRole("button", { name: "Confirm plan and continue", exact: true })
    .click({ trial: true });
  await page.screenshot({
    path: out + "/pending-plan-review.png",
    fullPage: true,
  });
  await page
    .getByRole("button", { name: "Confirm plan and continue", exact: true })
    .click();
  assert.ok(state.colonies[0].definition.members.includes("salary_stats"));
  await page.getByRole("button", { name: "Cancel goal", exact: true }).click();
  assert.deepEqual(actions, [
    "start",
    "pause",
    "resume",
    "confirm_plan",
    "cancel",
  ]);
  const colony = state.colonies[0];
  await page
    .getByRole("button", { name: "Close Colony panel", exact: true })
    .click();
  await page.getByRole("button", { name: "Fit view", exact: true }).click();
  await page.screenshot({
    path: out + "/colony-canvas-desktop.png",
    fullPage: true,
  });
  assert.ok(
    colony.definition.connections.some(
      (edge) => edge.from === "reviewer" && edge.to === "resume",
    ),
  );
  assert.ok(
    !colony.definition.connections.some(
      (edge) => edge.from === "resume" && edge.to === "reviewer",
    ),
  );
  await page
    .getByTestId(`colony-node-${colony.id}::agent:reviewer`)
    .getByRole("button", { name: "reviewer", exact: true })
    .click();
  await page
    .getByRole("textbox", { name: "Message this agent or room…" })
    .fill("Review salary transparency as well.");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  await page
    .getByText("Review salary transparency as well.", { exact: true })
    .waitFor();
  assert.equal(messages[0].recipient, "reviewer");
  await page
    .getByRole("button", { name: "Close Colony panel", exact: true })
    .click();
  await page.getByText("Add node", { exact: true }).click();
  await page
    .getByRole("button", { name: "Add recurring prompt", exact: true })
    .click();
  await page
    .getByRole("textbox", { name: "Prompt text", exact: true })
    .fill("Use the approved job preferences in each run.");
  await page
    .getByRole("group", { name: "Inject into" })
    .getByRole("checkbox", { name: "reviewer", exact: true })
    .check();
  await page
    .getByRole("radio", { name: "Next admitted run", exact: true })
    .check();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page
    .getByRole("button", { name: "Save", exact: true })
    .waitFor({ state: "hidden" });
  await idle();
  assert.equal(saves.at(-1).prompt_activation, "next_run");
  await page
    .getByRole("button", { name: "Close Colony panel", exact: true })
    .click();
  await page.getByText("Add node", { exact: true }).click();
  await page
    .getByRole("button", { name: "Add internal room", exact: true })
    .click();
  await page
    .getByRole("textbox", { name: "Room name" })
    .fill("Shortlist discussion");
  await page
    .getByRole("combobox", { name: "Who responds?" })
    .selectOption("queen_selected");
  await page
    .getByRole("group", { name: "May read" })
    .getByRole("checkbox", { name: "reviewer", exact: true })
    .check();
  await page
    .getByRole("group", { name: "May publish" })
    .getByRole("checkbox", { name: "reviewer", exact: true })
    .check();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page
    .getByRole("button", { name: "Save", exact: true })
    .waitFor({ state: "hidden" });
  await idle();
  assert.equal(colony.definition.rooms[0].responders, "queen_selected");
  await page
    .getByRole("textbox", { name: "Message this agent or room…" })
    .fill("Discuss the strongest candidates.");
  await page.getByRole("button", { name: "Send", exact: true }).click();
  assert.ok(messages.at(-1).recipient.startsWith("room:"));
  await page
    .getByRole("button", { name: "Close Colony panel", exact: true })
    .click();
  await page.getByText("Add node", { exact: true }).click();
  await page
    .getByRole("button", { name: "Add external channel", exact: true })
    .click();
  await page
    .getByRole("textbox", { name: "Exact recipient or conversation" })
    .fill("application-drafts");
  await page
    .getByRole("group", { name: "Outbound from agents" })
    .getByRole("checkbox", { name: "reviewer", exact: true })
    .check();
  await page.getByRole("button", { name: "Save", exact: true }).click();
  await page
    .getByRole("button", { name: "Save", exact: true })
    .waitFor({ state: "hidden" });
  await idle();
  assert.deepEqual(colony.definition.channels[0].inbound_agents, []);
  assert.deepEqual(colony.definition.channels[0].outbound_agents, ["reviewer"]);
  await page.getByRole("button", { name: "Create agent", exact: true }).click();
  await page
    .getByRole("textbox", { name: "Agent alias", exact: true })
    .fill("salary_reviewer");
  await page
    .getByRole("textbox", { name: "Core command", exact: true })
    .fill("Evaluate salary ranges against the user’s preferences.");
  await page
    .getByRole("button", { name: "Review and add agent", exact: true })
    .click();
  assert.ok(colony.definition.members.includes("salary_reviewer"));
  await page
    .getByRole("button", { name: "Close Colony panel", exact: true })
    .click();
  await page.getByRole("button", { name: "Fit view", exact: true }).click();
  await page.screenshot({
    path: out + "/colony-all-node-types.png",
    fullPage: true,
  });
  await page
    .getByRole("button", { name: "New goal", exact: true })
    .first()
    .click();
  await page.getByRole("radio", { name: "Continue", exact: true }).waitFor();
  await page.getByRole("radio", { name: "Start fresh", exact: true }).check();
  await page
    .getByRole("textbox", { name: "What should this team accomplish?" })
    .fill("Find the next set of suitable roles.");
  await page.screenshot({
    path: out + "/reuse-review-desktop.png",
    fullPage: true,
  });
  await page
    .getByRole("button", { name: "Close Colony panel", exact: true })
    .click();
  await page
    .getByRole("button", { name: "Conversations", exact: true })
    .click();
  await chat.waitFor();
  assert.equal(await chat.inputValue(), "Keep this unsent conversation draft.");
  assert.equal(chatSockets.length, socketCount);
  await page.getByRole("button", { name: "Colony", exact: true }).click();
  await page.setViewportSize({ width: 390, height: 844 });
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
    true,
  );
  await page.screenshot({ path: out + "/colony-mobile.png", fullPage: true });
  await page
    .getByRole("button", { name: "New goal", exact: true })
    .first()
    .click();
  await page.screenshot({ path: out + "/setup-mobile.png", fullPage: true });
  await page.setViewportSize({ width: 320, height: 720 });
  assert.equal(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= innerWidth,
    ),
    true,
  );
  await page.screenshot({
    path: out + "/setup-mobile-320.png",
    fullPage: true,
  });
  await page.setViewportSize({ width: 1440, height: 980 });
  await page.evaluate(() =>
    localStorage.setItem(
      "zeroclaw-theme",
      JSON.stringify({ theme: "light", accent: "cyan" }),
    ),
  );
  await page.reload();
  await page.getByRole("button", { name: "Colony", exact: true }).click();
  await page.getByRole("button", { name: "Fit view", exact: true }).click();
  await page.screenshot({ path: out + "/colony-light.png", fullPage: true });
  // An automatic first run must not issue a second Start request.
  colony.definition.start_mode = "automatic";
  await page.getByRole("button", { name: "Refresh", exact: true }).click();
  await page
    .getByRole("combobox", { name: "Choose a saved colony", exact: true })
    .selectOption(colony.id);
  await page
    .getByRole("button", { name: "New goal", exact: true })
    .first()
    .click();
  await page
    .getByRole("textbox", { name: "What should this team accomplish?" })
    .fill("Prepare a second reviewed shortlist.");
  await page.getByRole("button", { name: "Clarify goal", exact: true }).click();
  await page.getByRole("button", { name: "Pause", exact: true }).waitFor();
  assert.equal(actions.filter((action) => action === "start").length, 1);
  await idle();
  await page.getByRole("button", { name: "Cancel goal", exact: true }).click();
  await idle();
  proposeFutureTeam = true;
  await page
    .getByRole("button", { name: "New goal", exact: true })
    .first()
    .click();
  await page
    .getByRole("textbox", { name: "What should this team accomplish?" })
    .fill("Refine the shortlist with two cooperating specialists.");
  await page.getByRole("button", { name: "Clarify goal", exact: true }).click();
  await idle();
  assert.equal(
    colony.goals.length,
    2,
    "automatic supervised mode must stop for new-role review",
  );
  await page
    .getByRole("button", { name: "Approve team and start", exact: true })
    .click();
  await idle();
  assert.ok(
    colony.definition.members.includes("job_stats") &&
      colony.definition.members.includes("role_filter"),
  );
  assert.ok(
    colony.definition.connections.some(
      (edge) => edge.from === "job_stats" && edge.to === "role_filter",
    ),
  );
  assert.ok(
    !colony.definition.connections.some(
      (edge) => edge.from === "role_filter" && edge.to === "job_stats",
    ),
  );
  await page
    .getByTestId(`colony-node-${colony.id}::agent:role_filter`)
    .waitFor();
  // An installation with no agents still exposes Colony and agent settings.
  const socketsBeforeEmpty = chatSockets.length;
  state.agents.splice(0);
  state.colonies.splice(0);
  await page.goto(url + "/agent");
  await page.getByRole("button", { name: "Colony", exact: true }).click();
  await page
    .getByText("Configure an enabled agent to create a Queen and team.", {
      exact: true,
    })
    .waitFor();
  assert.equal(chatSockets.length, socketsBeforeEmpty);
  await page.screenshot({ path: out + "/empty-colony.png", fullPage: true });
  assert.deepEqual(errors, []);
  const sourceFiles = [
    "src/pages/ColonyWorkspace.tsx",
    "src/components/ColonyCanvas.tsx",
    "src/pages/ChatWorkspace.tsx",
    "src/pages/AgentLanding.tsx",
    "src/components/AgentSidebar.tsx",
    "src/lib/colonies.ts",
    "src/lib/i18n.ts",
  ];
  const sourceSha256 = Object.fromEntries(
    sourceFiles.map((file) => [
      file,
      createHash("sha256")
        .update(fs.readFileSync(path.resolve(__dirname, "..", file)))
        .digest("hex"),
    ]),
  );
  const rendererHtml = await (await context.request.get(url)).text();
  fs.writeFileSync(
    out + "/result.json",
    JSON.stringify(
      {
        passed: true,
        actions,
        creates: creates.length,
        saves: saves.length,
        chatSockets: chatSockets.length,
        viewport: [1440, 980],
        synthetic_http_and_models: true,
        compiled_renderer: process.env.WEB_SMOKE_DIST === "1",
        browser_version: browser.version(),
        git_revision: execFileSync("git", ["rev-parse", "HEAD"], {
          cwd: path.resolve(__dirname, "../.."),
          encoding: "utf8",
        }).trim(),
        source_sha256: sourceSha256,
        renderer_assets: [
          ...rendererHtml.matchAll(/(?:src|href)="([^\"]*\/assets\/[^\"]+)"/g),
        ].map((match) => match[1]),
      },
      null,
      2,
    ),
  );
  console.log(JSON.stringify({ passed: true, evidence: out }));
  await browser.close();
  if (distServer) await new Promise((resolve) => distServer.close(resolve));
})().catch(async (error) => {
  console.error(error);
  if (globalThis.testPage) {
    console.error(await testPage.locator("body").innerText());
    await testPage.screenshot({ path: out + "/failure.png", fullPage: true });
  }
  process.exit(1);
});
