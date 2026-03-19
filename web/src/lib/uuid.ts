/**
 * Generate a UUID v4 string.
 *
 * Uses `crypto.randomUUID()` when available (modern browsers, secure contexts)
 * and falls back to a manual implementation backed by `crypto.getRandomValues()`
 * for older browsers (e.g. Safari < 15.4, some Electron/Raspberry-Pi builds).
 *
 * Closes #3303, #3261.
 */
export function generateUUID(): string {
  if (
    typeof crypto !== "undefined" &&
    typeof crypto.randomUUID === "function"
  ) {
    return crypto.randomUUID();
  }

  // Fallback: RFC 4122 version 4 UUID via getRandomValues
  // crypto must exist if we reached here (only randomUUID is missing)
  const c = globalThis.crypto;
  const bytes = new Uint8Array(16);
  c.getRandomValues(bytes);

  // Set version (4) and variant (10xx) bits per RFC 4122
  bytes[6] = (bytes[6]! & 0x0f) | 0x40;
  bytes[8] = (bytes[8]! & 0x3f) | 0x80;

  const hex = Array.from(bytes, (b) => b.toString(16).padStart(2, "0")).join(
    "",
  );
  return `${hex.slice(0, 8)}-${hex.slice(8, 12)}-${hex.slice(12, 16)}-${hex.slice(16, 20)}-${hex.slice(20)}`;
}
