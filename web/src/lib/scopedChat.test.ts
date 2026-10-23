import assert from "node:assert/strict";
import test from "node:test";

// Data and command rules for dashboard chat by an account without the
// operator dashboard: the agents it may use, and the commands it gets.

const fakeWindow = Object.assign(new EventTarget(), {
  location: { protocol: "https:", host: "zc.example", pathname: "/", search: "" },
});
Object.assign(globalThis, { window: fakeWindow });

let agentsBody: unknown = { agents: [] };
const requested: string[] = [];
globalThis.fetch = async (input) => {
  const url = typeof input === "string" ? input : input instanceof URL ? input.toString() : input.url;
  requested.push(url);
  return new Response(JSON.stringify(agentsBody), {
    status: 200,
    headers: { "Content-Type": "application/json" },
  });
};

const { getAvailableAgents } = await import("./api.ts");
const { COMMANDS, commandsFor, helpText, matchCommands } = await import("./slashCommands.ts");

test("available agents come from /api/agents/available, malformed rows dropped", async () => {
  agentsBody = {
    agents: [
      { alias: "support", enabled: true },
      { alias: "triage", enabled: false },
      { alias: "legacy" },
      { alias: "" },
      { enabled: true },
      null,
    ],
  };
  assert.deepEqual(await getAvailableAgents(), [
    { alias: "support", enabled: true },
    { alias: "triage", enabled: false },
    { alias: "legacy", enabled: true },
  ]);
  assert.ok(requested[requested.length - 1]?.endsWith("/api/agents/available"));
});

test("a body without an agents list reads as no agents", async () => {
  agentsBody = { nope: true };
  assert.deepEqual(await getAvailableAgents(), []);
});

test("operators keep every command; scoped accounts lose /model and /upload", () => {
  assert.deepEqual(commandsFor(true), COMMANDS);
  const scoped = commandsFor(false).map((c) => c.name);
  assert.deepEqual(scoped, ["help", "clear", "new"]);
  assert.deepEqual(matchCommands("m", commandsFor(false)), []);
  assert.deepEqual(matchCommands("m").map((c) => c.name), ["model"]);
  assert.equal(helpText(commandsFor(false)).includes("/model"), false);
  assert.equal(helpText(commandsFor(false)).includes("/upload"), false);
  assert.ok(helpText().includes("/model"));
});
