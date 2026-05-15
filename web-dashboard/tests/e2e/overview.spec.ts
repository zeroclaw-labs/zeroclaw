/**
 * Overview page smoke (M5.0, US-004).
 *
 * Stands alone like the existing M3/M4b tests: Vite dev server with
 * route-mocked gateway responses. Mocks reflect the wire shapes from
 * `crates/zeroclaw-gateway/src/api.rs` handlers (memory_list, cron_list,
 * integrations, tools).
 *
 * Coverage:
 *   1. /overview renders the header with assistant identity + the four
 *      card titles.
 *   2. SectionNav highlights "Overview" as active when the URL matches.
 *   3. Memory + cron + integrations counts render from mocked payloads.
 *   4. A failing endpoint surfaces an error in only its card; sibling
 *      cards still render their data.
 */
import { test, expect } from "@playwright/test";

const BOOTSTRAP_BODY = {
  server_version: "0.0.0-test",
  assistant_identity: { name: "ZeroClaw" },
  themes: { default_theme: "default", default_mode: "dark" },
  max_chat_width_ch: 80,
};

// Only fields the Overview cards read (key, content, timestamp). The
// real wire shape carries id, category, namespace etc. — Memory deep
// page fixtures (M5.2) will widen this when those fields matter.
const MEMORY_BODY = {
  entries: [
    {
      key: "fav_color",
      content: "blue",
      timestamp: "2026-05-15T10:00:00Z",
    },
    {
      key: "morning_routine",
      content: "coffee, then code",
      timestamp: "2026-05-15T09:00:00Z",
    },
  ],
};

const CRON_BODY = {
  jobs: [
    {
      id: "j1",
      name: "Morning digest",
      expression: "0 9 * * *",
      next_run: "2026-05-16T09:00:00Z",
      enabled: true,
    },
    {
      id: "j2",
      name: "Cleanup",
      expression: "0 0 * * 0",
      next_run: "2026-05-17T00:00:00Z",
      enabled: false,
    },
  ],
};

const INTEGRATIONS_BODY = {
  integrations: [
    {
      name: "Telegram",
      description: "Telegram bot",
      category: "Chat",
      status: "Active",
    },
    {
      name: "Slack",
      description: "Slack workspace",
      category: "Chat",
      status: "Available",
    },
    {
      name: "Anthropic",
      description: "Anthropic API",
      category: "AiModel",
      status: "Active",
    },
  ],
};

const TOOLS_BODY = {
  tools: [
    { name: "memory_recall", description: "Recall memory" },
    { name: "cron_add", description: "Add cron job" },
    { name: "web_fetch", description: "Fetch URL" },
  ],
};

async function mockGateway(page: import("@playwright/test").Page) {
  await page.route("**/api/control-ui/config", (route) =>
    route.fulfill({ json: BOOTSTRAP_BODY }),
  );
  await page.route("**/api/memory", (route) =>
    route.fulfill({ json: MEMORY_BODY }),
  );
  await page.route("**/api/cron", (route) =>
    route.fulfill({ json: CRON_BODY }),
  );
  await page.route("**/api/integrations", (route) =>
    route.fulfill({ json: INTEGRATIONS_BODY }),
  );
  await page.route("**/api/tools", (route) =>
    route.fulfill({ json: TOOLS_BODY }),
  );
}

test("overview page renders all four cards", async ({ page }) => {
  await mockGateway(page);

  await page.goto("/overview");

  await expect(
    page.getByRole("heading", { level: 1, name: "Overview" }),
  ).toBeVisible();

  await expect(
    page.getByRole("heading", { level: 2, name: "Memory" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { level: 2, name: "Scheduled jobs" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { level: 2, name: "Integrations" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { level: 2, name: "Skills" }),
  ).toBeVisible();
});

test("overview section nav marks Overview active", async ({ page }) => {
  await mockGateway(page);

  await page.goto("/overview");

  // aria-current="page" lands only on the active link. Multiple
  // SectionNav layouts could render; restrict to the page header.
  const overviewLink = page.getByRole("link", { name: "Overview" });
  await expect(overviewLink.first()).toHaveAttribute("aria-current", "page");
});

test("overview cards render counts from mocked endpoints", async ({ page }) => {
  await mockGateway(page);

  await page.goto("/overview");

  // Memory: 2 entries.
  const memoryCard = page
    .getByRole("article")
    .filter({ has: page.getByRole("heading", { name: "Memory" }) });
  await expect(memoryCard.getByText("2", { exact: true })).toBeVisible();
  await expect(memoryCard.getByText(/entries/)).toBeVisible();

  // Crons: 1 enabled / 2 total.
  const cronCard = page
    .getByRole("article")
    .filter({ has: page.getByRole("heading", { name: "Scheduled jobs" }) });
  await expect(cronCard.getByText("1", { exact: true })).toBeVisible();
  await expect(cronCard.getByText(/2 total/)).toBeVisible();

  // Integrations: 2 active / 3 available.
  const integrationsCard = page
    .getByRole("article")
    .filter({ has: page.getByRole("heading", { name: "Integrations" }) });
  await expect(integrationsCard.getByText("2", { exact: true })).toBeVisible();
  await expect(integrationsCard.getByText(/3 available/)).toBeVisible();

  // Skills: 3 tools.
  const skillsCard = page
    .getByRole("article")
    .filter({ has: page.getByRole("heading", { name: "Skills" }) });
  await expect(skillsCard.getByText("3", { exact: true })).toBeVisible();
  await expect(skillsCard.getByText(/tools registered/)).toBeVisible();
});

test("a failing endpoint shows an error in its card without breaking siblings", async ({
  page,
}) => {
  await page.route("**/api/control-ui/config", (route) =>
    route.fulfill({ json: BOOTSTRAP_BODY }),
  );
  await page.route("**/api/memory", (route) =>
    route.fulfill({ status: 500, body: "boom" }),
  );
  await page.route("**/api/cron", (route) =>
    route.fulfill({ json: CRON_BODY }),
  );
  await page.route("**/api/integrations", (route) =>
    route.fulfill({ json: INTEGRATIONS_BODY }),
  );
  await page.route("**/api/tools", (route) =>
    route.fulfill({ json: TOOLS_BODY }),
  );

  await page.goto("/overview");

  const memoryCard = page
    .getByRole("article")
    .filter({ has: page.getByRole("heading", { name: "Memory" }) });
  await expect(memoryCard.getByRole("alert")).toBeVisible();

  // Sibling cards still render content despite the memory error.
  const skillsCard = page
    .getByRole("article")
    .filter({ has: page.getByRole("heading", { name: "Skills" }) });
  await expect(skillsCard.getByText("3", { exact: true })).toBeVisible();
});
