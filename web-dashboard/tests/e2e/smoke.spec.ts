/**
 * Dashboard smoke test (M3, US-006).
 *
 * Runs against Vite's dev server with route-mocked gateway responses
 * so the test stands alone — no real `zeroclaw-gateway` needed. The
 * mocks reflect the wire shapes from
 * `crates/zeroclaw-gateway/src/api_slots.rs` and
 * `crates/zeroclaw-gateway/src/api.rs`.
 *
 * Coverage:
 *   1. Bootstrap snapshot loads → assistant identity is visible
 *   2. Empty slot list renders the empty-state hint
 *   3. Clicking +New POSTs to /api/slots, the new slot appears
 *   4. Toggling theme writes data-theme on <html> and persists across
 *      a full page reload (FOUC script applies it before React mounts)
 */
import { test, expect } from "@playwright/test";

const BOOTSTRAP_BODY = {
  server_version: "0.0.0-test",
  assistant_identity: { name: "ZeroClaw" },
  themes: { default_theme: "default", default_mode: "dark" },
  max_chat_width_ch: 80,
};

interface MockSlot {
  id: string;
  session_id: string;
  title: string;
  state: "idle" | "running" | "waiting_approval" | "error";
  message_count: number;
  dirty: boolean;
  created_at: number;
  updated_at: number;
}

test.beforeEach(async ({ context }) => {
  // Slots state is per-test; reset by closing context (Playwright does
  // this for us) but each test re-mocks anyway.
  await context.clearCookies();
});

test("bootstrap snapshot + empty slot list renders", async ({ page }) => {
  await page.route("**/api/control-ui/config", (route) =>
    route.fulfill({ json: BOOTSTRAP_BODY }),
  );
  await page.route("**/api/slots", (route) =>
    route.fulfill({ json: { slots: [] } }),
  );

  await page.goto("/");

  // Bootstrap loaded: the sidebar header shows the assistant identity
  // (the test value matches what BOOTSTRAP_BODY ships).
  await expect(page.getByText("ZeroClaw").first()).toBeVisible();

  // Empty-state copy lives next to the +New button.
  await expect(page.getByText(/No slots yet/)).toBeVisible();
  await expect(page.getByRole("button", { name: "Create new slot" })).toBeVisible();
});

test("create slot via +New posts to /api/slots and shows the new row", async ({
  page,
}) => {
  await page.route("**/api/control-ui/config", (route) =>
    route.fulfill({ json: BOOTSTRAP_BODY }),
  );

  let slots: MockSlot[] = [];
  let createCalls = 0;

  await page.route("**/api/slots", async (route) => {
    if (route.request().method() === "POST") {
      createCalls += 1;
      const body = route.request().postDataJSON() as {
        title?: string;
      } | null;
      const now = Math.floor(Date.now() / 1000);
      const slot: MockSlot = {
        id: `slot-${createCalls}`,
        session_id: `sess-${createCalls}`,
        title: body?.title?.trim() || `Slot ${createCalls}`,
        state: "idle",
        message_count: 0,
        dirty: false,
        created_at: now,
        updated_at: now,
      };
      slots = [slot, ...slots];
      await route.fulfill({ json: slot });
      return;
    }
    await route.fulfill({ json: { slots } });
  });

  await page.goto("/");

  await expect(page.getByText(/No slots yet/)).toBeVisible();

  await page.getByRole("button", { name: "Create new slot" }).click();

  // Wait for the next list refetch to surface the slot — React Query
  // invalidates the `slots` cache on success. The new slot title shows
  // up in two places (sidebar row + chat header band), so scope this
  // assertion to the sidebar's button to avoid the double-match.
  await expect(
    page.getByRole("button", { name: "Slot 1" }),
  ).toBeVisible();

  expect(createCalls).toBe(1);
});

test("theme toggle persists across reload", async ({ page }) => {
  await page.route("**/api/control-ui/config", (route) =>
    route.fulfill({ json: BOOTSTRAP_BODY }),
  );
  await page.route("**/api/slots", (route) =>
    route.fulfill({ json: { slots: [] } }),
  );

  await page.goto("/");

  // Open the theme switcher and pick `monochrome` + `light`.
  await page.getByRole("button", { name: /^Theme:/ }).click();
  await page.getByTestId("theme-monochrome").click();
  await page.getByTestId("mode-light").click();

  // Verify the live DOM reflects the choice.
  await expect(page.locator("html")).toHaveAttribute(
    "data-theme",
    "monochrome",
  );
  await expect(page.locator("html")).toHaveAttribute("data-mode", "light");

  // Reload — the FOUC-avoidance script in `index.html` should apply
  // the saved values before React boots, so the attributes never
  // flicker back to defaults.
  await page.reload();

  await expect(page.locator("html")).toHaveAttribute(
    "data-theme",
    "monochrome",
  );
  await expect(page.locator("html")).toHaveAttribute("data-mode", "light");
});
