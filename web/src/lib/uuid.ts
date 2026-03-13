/**
 * UUID generation utility with fallback for environments where crypto.randomUUID
 * is not available (e.g., older browsers, certain embedded environments).
 *
 * This addresses issue #3261 where crypto.randomUUID is not a function
 * in some environments like Raspberry Pi.
 */

/**
 * Generate a random UUID v4.
 * Uses crypto.randomUUID() when available, falls back to a manual implementation
 * using crypto.getRandomValues() for environments that don't support randomUUID.
 */
export function generateUUID(): string {
  // Try to use native crypto.randomUUID if available
  if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
    return crypto.randomUUID();
  }

  // Fallback: manual UUID v4 generation using crypto.getRandomValues
  // Format: xxxxxxxx-xxxx-4xxx-yxxx-xxxxxxxxxxxx
  // where y is 8, 9, a, or b
  const uuid = new Array(36);
  const rnds = new Uint8Array(16);

  // Get random values
  if (typeof crypto !== 'undefined' && typeof crypto.getRandomValues === 'function') {
    crypto.getRandomValues(rnds);
  } else {
    // Last resort fallback using Math.random (less secure but functional)
    for (let i = 0; i < 16; i++) {
      rnds[i] = Math.floor(Math.random() * 256);
    }
  }

  // Set version (4) and variant bits
  rnds[6] = (rnds[6]! & 0x0f) | 0x40; // Version 4
  rnds[8] = (rnds[8]! & 0x3f) | 0x80; // Variant 10

  // Convert to string
  const hex = '0123456789abcdef';
  let idx = 0;

  for (let i = 0; i < 16; i++) {
    uuid[idx++] = hex[rnds[i]! >> 4];
    uuid[idx++] = hex[rnds[i]! & 0x0f];

    // Add dashes at positions 8, 12, 16, 20
    if (idx === 8 || idx === 13 || idx === 18 || idx === 23) {
      uuid[idx++] = '-';
    }
  }

  return uuid.join('');
}
