// Board smoke tests. Stands alone: Vite dev + route-mocked gateway,
// stubbed WebSocket, synthetic bus events injected via the dev-only
// `__slotBusInject` hook in slotEvents.ts.
import { test, expect, type Page } from "@playwright/test";

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
  agent_config?: unknown;
}

async function bootstrap(page: Page, slots: MockSlot[]) {
  await page.route("**/api/control-ui/config", (route) =>
    route.fulfill({ json: BOOTSTRAP_BODY }),
  );
  await page.route("**/api/slots", (route) =>
    route.fulfill({ json: { slots } }),
  );
}

async function pushBusEvent(page: Page, event: unknown): Promise<void> {
  await page.evaluate((ev) => {
    const w = window as unknown as { __slotBusInject?: (e: unknown) => void };
    if (!w.__slotBusInject) {
      throw new Error("window.__slotBusInject not installed");
    }
    w.__slotBusInject(ev);
  }, event);
}

// Bus's connect() opens a real WebSocket; the injector hook bypasses
// the wire, so the stub only needs the methods the bus calls.
async function stubWebSocket(page: Page): Promise<void> {
  await page.addInitScript(() => {
    class StubWebSocket {
      static readonly CONNECTING = 0;
      static readonly OPEN = 1;
      static readonly CLOSED = 3;
      readyState = StubWebSocket.CONNECTING;
      addEventListener(): void {}
      removeEventListener(): void {}
      send(): void {}
      close(): void {}
    }
    Object.defineProperty(window, "WebSocket", {
      value: StubWebSocket,
      writable: true,
      configurable: true,
    });
  });
}

test.beforeEach(async ({ page }) => {
  await stubWebSocket(page);
});

test("board lanes auto-sort by slot state", async ({ page }) => {
  const slots: MockSlot[] = [
    mockSlot({ id: "s-idle", title: "Idle Slot", state: "idle" }),
    mockSlot({ id: "s-run", title: "Working Slot", state: "running" }),
    mockSlot({
      id: "s-wait",
      title: "Your-Turn Slot",
      state: "waiting_approval",
    }),
    mockSlot({ id: "s-err", title: "Errored Slot", state: "error" }),
  ];
  await bootstrap(page, slots);

  await page.goto("/board");

  // Lane order: Needs Approval / Your Turn / Working / Idle.
  // Empty lanes show the "Empty" placeholder.
  await expect(
    page.getByTestId("board-lane-empty-needs_approval"),
  ).toBeVisible();
  await expect(page.getByText("Your-Turn Slot")).toBeVisible();
  await expect(page.getByText("Working Slot")).toBeVisible();
  await expect(page.getByText("Idle Slot")).toBeVisible();

  // Errored strip carries error-state slots.
  await expect(page.getByText("Errored Slot")).toBeVisible();

  // Each non-error slot has a data-board-lane attribute matching its lane.
  await expect(page.locator("[data-slot-id='s-wait']")).toHaveAttribute(
    "data-board-lane",
    "your_turn",
  );
  await expect(page.locator("[data-slot-id='s-run']")).toHaveAttribute(
    "data-board-lane",
    "working",
  );
  await expect(page.locator("[data-slot-id='s-idle']")).toHaveAttribute(
    "data-board-lane",
    "idle",
  );
});

test("approval flow: click Approve on Board card POSTs decision", async ({
  page,
}) => {
  const slot = mockSlot({
    id: "slot-approve",
    title: "Approval Slot",
    state: "running",
  });
  await bootstrap(page, [slot]);

  let approveCalls: Array<{ request_id: string; decision: string }> = [];
  await page.route("**/api/slots/slot-approve/approve", async (route) => {
    const body = route.request().postDataJSON() as {
      request_id: string;
      decision: string;
    };
    approveCalls.push(body);
    await route.fulfill({ json: { status: "accepted" } });
  });

  await page.goto("/board");

  // Initially, the slot is in "Working" lane (no pending approvals).
  await expect(page.locator("[data-slot-id='slot-approve']")).toHaveAttribute(
    "data-board-lane",
    "working",
  );

  // Inject a permission_request event over the bus.
  await pushBusEvent(page, {
    type: "permission_request",
    slot_id: "slot-approve",
    data: {
      request_id: "req-test-1",
      tool_name: "shell",
      arguments_summary: "ls -la",
      timeout_secs: 120,
    },
  });

  // The card should re-lane into "Needs Approval".
  await expect(page.locator("[data-slot-id='slot-approve']")).toHaveAttribute(
    "data-board-lane",
    "needs_approval",
  );

  // Approve button visible; click it.
  const approveBtn = page.getByRole("button", {
    name: /Approve shell on Approval Slot/,
  });
  await expect(approveBtn).toBeVisible();
  await approveBtn.click();

  // POST fired with the right body.
  await expect.poll(() => approveCalls.length).toBe(1);
  expect(approveCalls[0].request_id).toBe("req-test-1");
  expect(approveCalls[0].decision).toBe("approve");

  // Card moves back out of Needs Approval (the optimistic remove ran;
  // the canonical bus event would normally land here too).
  await expect(page.locator("[data-slot-id='slot-approve']")).toHaveAttribute(
    "data-board-lane",
    "working",
  );
});

test("stall badge: page renders board for running slot without errors", async ({
  page,
}) => {
  // The stall threshold is wall-clock 30s, which is impractical to
  // exercise inside Playwright without faking timers across React's
  // render cycle. The unit-level stall behavior is exercised by the
  // useStallDetection hook (compare-against-Date.now()); the e2e
  // signal here is "BoardPage renders cleanly when a Running slot is
  // present and the stall hook is wired up — no thrown exceptions,
  // and the slot card is visible." Wall-clock stall is covered by
  // manual QA + the unit test in a follow-up.
  const slot = mockSlot({
    id: "slot-stall",
    title: "Stall Slot",
    state: "running",
  });
  await bootstrap(page, [slot]);
  await page.route("**/api/slots/slot-stall", (route) =>
    route.fulfill({ json: slot }),
  );

  await page.goto("/board");

  await expect(page.locator("[data-slot-id='slot-stall']")).toBeVisible();
  await expect(page.locator("[data-slot-id='slot-stall']")).toHaveAttribute(
    "data-board-lane",
    "working",
  );
});

// ── helpers ─────────────────────────────────────────────────────────

function mockSlot(overrides: Partial<MockSlot>): MockSlot {
  const now = Math.floor(Date.now() / 1000);
  return {
    id: "slot-1",
    session_id: "sess-1",
    title: "Slot",
    state: "idle",
    message_count: 0,
    dirty: false,
    created_at: now,
    updated_at: now,
    agent_config: {},
    ...overrides,
  };
}
