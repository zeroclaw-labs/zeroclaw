import assert from "node:assert/strict";
import test from "node:test";

import {
  NEAR_BOTTOM_PX,
  distanceFromBottom,
  isNearBottom,
} from "./agentChatScroll.ts";

// A 2000px transcript inside an 800px viewport scrolled all the way down.
const atTail = { scrollHeight: 2000, scrollTop: 1200, clientHeight: 800 };

test("a pane pinned to the tail counts as near the bottom", () => {
  assert.equal(distanceFromBottom(atTail), 0);
  assert.equal(isNearBottom(atTail), true);
});

test("distance grows as the reader scrolls up", () => {
  assert.equal(distanceFromBottom({ ...atTail, scrollTop: 1100 }), 100);
  assert.equal(distanceFromBottom({ ...atTail, scrollTop: 900 }), 300);
});

test("the near-bottom threshold is inclusive", () => {
  const exactlyAtThreshold = { ...atTail, scrollTop: atTail.scrollTop - NEAR_BOTTOM_PX };
  const justBeyond = { ...atTail, scrollTop: atTail.scrollTop - NEAR_BOTTOM_PX - 1 };

  assert.equal(isNearBottom(exactlyAtThreshold), true);
  assert.equal(isNearBottom(justBeyond), false);
});

test("a transcript shorter than the viewport is always at the bottom", () => {
  // scrollHeight < clientHeight happens when there is nothing to scroll yet.
  assert.equal(distanceFromBottom({ scrollHeight: 300, scrollTop: 0, clientHeight: 800 }), 0);
  assert.equal(isNearBottom({ scrollHeight: 300, scrollTop: 0, clientHeight: 800 }), true);
});

test("overscroll is clamped instead of reporting a negative distance", () => {
  // Elastic overscroll (iOS) can push scrollTop past the tail.
  assert.equal(distanceFromBottom({ ...atTail, scrollTop: 1300 }), 0);
});
