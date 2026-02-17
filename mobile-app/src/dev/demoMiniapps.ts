import type { MiniAppKind } from "../../ui/miniapps/MiniApps";

export const miniAppKindForId = (id: string): MiniAppKind => {
  if (id.includes("booking")) return "booking";
  if (id.includes("tasks") || id.includes("list")) return "checklist";
  if (id.includes("notes")) return "notes";
  if (id.includes("budget") || id.includes("expense") || id.includes("finance")) return "expenses";
  if (id.includes("counter") || id.includes("streak")) return "counter";
  // Stable fallback variety.
  const n = Array.from(id).reduce((acc, ch) => (acc + ch.charCodeAt(0)) % 1000, 0);
  return (n % 3 === 0 ? "booking" : n % 3 === 1 ? "checklist" : "counter") as MiniAppKind;
};
