// Tiny fuzzy filter: matches when every query character appears in the target
// in order, scoring by tightness (consecutive matches > spread-out). Returns
// the matching items sorted by score (best first). Empty query returns the
// input unchanged. Case-insensitive.
//
// No npm dep — the dashboard's filter inputs need this in 4-5 places and the
// problem is small enough to solve inline.

export function fuzzyFilter<T>(items: T[], query: string, getText: (item: T) => string): T[] {
  if (!query.trim()) return items;
  const q = query.toLowerCase();

  const scored: Array<{ item: T; score: number }> = [];
  for (const item of items) {
    const text = getText(item).toLowerCase();
    const score = scoreFuzzy(text, q);
    if (score !== null) scored.push({ item, score });
  }
  scored.sort((a, b) => b.score - a.score);
  return scored.map((s) => s.item);
}

/** Returns null when the query characters can't be matched in order. */
function scoreFuzzy(text: string, query: string): number | null {
  let score = 0;
  let textIdx = 0;
  let consecutive = 0;
  let lastMatchIdx = -1;
  for (const qc of query) {
    let found = -1;
    for (let i = textIdx; i < text.length; i++) {
      if (text[i] === qc) {
        found = i;
        break;
      }
    }
    if (found === -1) return null;
    // Consecutive matches score double; gap resets the streak.
    if (found === lastMatchIdx + 1) {
      consecutive += 1;
      score += 2 + consecutive;
    } else {
      consecutive = 0;
      score += 1;
    }
    // Bonus when the match starts at the beginning.
    if (found === 0) score += 5;
    // Bonus for matching after a separator (- _ . space).
    const prev = text[found - 1];
    if (found > 0 && prev && /[-_. ]/.test(prev)) score += 3;
    lastMatchIdx = found;
    textIdx = found + 1;
  }
  return score;
}
