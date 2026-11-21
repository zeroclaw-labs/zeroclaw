/** New Quickstart detail is optional and can outpace this client. */
export function applyErrorMessage(
  fallback: string,
  detail: unknown,
  translate: (key: string) => string,
): string {
  if (typeof detail !== "object" || detail === null || !("kind" in detail)) {
    return fallback;
  }
  if (detail.kind === "goal_policy_preparation_failed") {
    return translate("quickstart.goal.preparation");
  }
  if (
    detail.kind !== "goal_policy_completion_failed" ||
    !("published" in detail) ||
    typeof detail.published !== "boolean"
  ) {
    return fallback;
  }
  const state = detail.published ? "published" : "notPublished";
  if (!("cancelled_goals" in detail)) {
    return translate(`quickstart.goal.${state}`);
  }
  const count = detail.cancelled_goals;
  if (typeof count !== "number" || !Number.isSafeInteger(count) || count < 0) {
    return fallback;
  }
  return translate(`quickstart.goal.${state}Count`).replace("{count}", String(count));
}
