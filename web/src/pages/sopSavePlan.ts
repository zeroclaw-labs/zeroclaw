/**
 * What pressing Save in the SOP editor should actually do.
 *
 * A SOP is stored under its own name, and `PUT /api/sops/{name}` writes to the
 * path the submitted SOP names. Saving a draft whose name was edited would
 * therefore write a second SOP and leave the original behind. Renaming is its
 * own collision-checked operation on the daemon, so the editor has to sequence
 * the two rather than hope one call covers both.
 */
export type SopSavePlan =
  | { kind: 'create' }
  | { kind: 'save' }
  | { kind: 'save-then-rename'; from: string; to: string };

/**
 * Decide what an editor save means, from the name the edit started under and
 * the name the draft carries now. `editingName` is `null` for a new draft.
 *
 * `from` is deliberately the name the edit started under, not the one in the
 * draft: the edits have to land on the SOP they were made against before
 * anything moves, or the save writes a second copy under the new name.
 *
 * The comparison is exact. A name is passed through as typed, because the
 * daemon is the authority on which names are legal, and trimming here would
 * quietly store something other than what the author entered.
 */
export function planSopSave(editingName: string | null, draftName: string): SopSavePlan {
  if (editingName === null) return { kind: 'create' };
  if (draftName === editingName) return { kind: 'save' };
  return { kind: 'save-then-rename', from: editingName, to: draftName };
}

/**
 * Turn a failed SOP request into something worth showing an author.
 *
 * The SOP routes answer with `{"error": "..."}` rather than the structured
 * envelope `apiFetch` knows how to unwrap, so its fallback stringifies the
 * whole body into the message. Left alone that puts raw JSON in front of the
 * author; the sentence inside it is the part they can act on.
 */
export function sopErrorText(error: unknown): string {
  const raw = error instanceof Error ? error.message : String(error);
  const body = raw.match(/^API \d+: (.*)$/s)?.[1];
  if (!body) return raw;
  try {
    const parsed: unknown = JSON.parse(body);
    if (parsed && typeof parsed === 'object' && 'error' in parsed) {
      const inner = (parsed as { error: unknown }).error;
      if (typeof inner === 'string' && inner.length > 0) return inner;
    }
  } catch {
    // Not JSON after all; the raw text is the best available message.
  }
  return raw;
}
