import assert from "node:assert/strict";
import test from "node:test";
import { applyErrorMessage } from "./apply-error.ts";

const fallback = "Existing error message";

async function loadFrenchTranslation() {
  Object.defineProperty(globalThis, "window", {
    configurable: true,
    value: { __ZEROCLAW_BASE__: "" },
  });
  try {
    const { tLocale } = await import("../../lib/i18n.ts");
    return (key: string) => tLocale(key, "fr");
  } finally {
    delete (globalThis as { window?: unknown }).window;
  }
}

test("Quickstart Goal errors distinguish preparation from publication", async () => {
  const french = await loadFrenchTranslation();
  assert.match(applyErrorMessage(fallback, {
    kind: "goal_policy_preparation_failed",
  }, french), /ni enregistrée ni publiée/);
  assert.match(applyErrorMessage(fallback, {
    kind: "goal_policy_completion_failed",
    published: true,
    cancelled_goals: 0,
  }, french), /enregistré et publié.*Ne le recréez pas.*Objectifs annulés : 0/);
  const notPublished = applyErrorMessage(fallback, {
    kind: "goal_policy_completion_failed",
    published: false,
  }, french);
  assert.match(notPublished, /pas été publiée/);
  assert.doesNotMatch(notPublished, /enregistr|annulés/);
});

test("Quickstart retains legacy, unknown and malformed error messages", async () => {
  const french = await loadFrenchTranslation();
  for (const detail of [
    undefined,
    null,
    "bad detail",
    { kind: "future_kind" },
    { kind: "goal_policy_completion_failed", published: "true" },
    ...[null, -1, 0.5, Number.MAX_SAFE_INTEGER + 1].map((cancelled_goals) => ({
      kind: "goal_policy_completion_failed", published: true, cancelled_goals,
    })),
  ]) {
    assert.equal(applyErrorMessage(fallback, detail, french), fallback);
  }
});
