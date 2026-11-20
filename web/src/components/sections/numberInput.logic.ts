// A cleared optional number means unset, not zero. The server treats `null`
// on an `Option` field as unset; required numbers keep their existing parse.

/** Returns whether a Rust type hint starts with `Option<`, ignoring whitespace. */
export function isOptionTypeHint(typeHint: string): boolean {
  return typeHint.replace(/\s+/g, "").startsWith("Option<");
}

/**
 * Parses a numeric draft while preserving optional-field clears as `null`.
 * Empty or whitespace-only `Option<…>` drafts become `null`; otherwise the
 * numeric parse is unchanged and non-numeric text is forwarded for server
 * validation. Required fields therefore keep `""` → `0`.
 */
export function parseNumberDraft(
  typeHint: string,
  raw: string,
): number | string | null {
  if (raw.trim().length === 0 && isOptionTypeHint(typeHint)) return null;
  const n = Number(raw);
  return Number.isNaN(n) ? raw : n;
}
