/** Match the API's map-key limit, including any collision suffix. */
export function nextXmppPolicyKey(alias: string, keys: readonly string[]): string {
  const base = `xmpp_${alias.replace(/[^a-zA-Z0-9_-]/g, "_")}`.slice(0, 63);
  const existing = new Set(keys);
  let key = base;
  for (let suffix = 2; existing.has(key); suffix += 1) {
    const tail = `_${suffix}`;
    key = `${base.slice(0, 63 - tail.length)}${tail}`;
  }
  return key;
}
