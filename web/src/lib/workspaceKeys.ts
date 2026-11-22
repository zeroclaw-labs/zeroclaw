export const workspaces = [
  { id: "agent", path: "/agent" },
  { id: "code", path: "/code" },
  { id: "sop", path: "/sops" },
] as const;
export type WorkspaceId = typeof workspaces[number]["id"];

export function workspaceForPath(path: string): WorkspaceId | undefined {
  if (path === "/agent" || path === "/agents" || /^\/agent\/[^/]+$/.test(path)) return "agent";
  if (path === "/code") return "code";
  if (path === "/sops" || path.startsWith("/sops/")) return "sop";
  return undefined;
}

export function adjacentItem(ids: readonly string[], selected: string | undefined, step: 1 | -1) {
  const index = selected === undefined ? -1 : ids.indexOf(selected);
  return index < 0 ? ids[0] : ids[index + step];
}

/** Keep preferred keys, then reserve prefixes for the two-letter hints. */
export function assignHints(preferred: readonly (string | undefined)[]): (string | undefined)[] {
  const alphabet = "abcdefghijklmnopqrstuvwxyz".split("");
  const used = new Set<string>();
  const result = preferred.map((key) => {
    if (!key || !alphabet.includes(key) || used.has(key)) return undefined;
    used.add(key);
    return key;
  });
  const free = alphabet.filter((key) => !used.has(key));
  const unassigned = preferred.length - used.size;
  const prefixCount = Math.min(free.length, Math.max(0, Math.ceil((unassigned - free.length) / 25)));
  const single = free.slice(prefixCount);
  const paired = free.slice(0, prefixCount).flatMap((prefix) => alphabet.map((key) => prefix + key));
  const available = [...single, ...paired];
  let next = 0;
  return result.map((key) => key ?? available[next++]);
}
