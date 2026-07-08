// Cross-section draft store for the config dashboard.
//
// Each field's unsaved value lives here keyed by its dotted prop path so
// drafts survive section navigation (clicking Channels then Memory and
// back doesn't lose the half-typed Channels webhook). The store also
// holds the tombstones (paths the operator clicked "unset" on but hasn't
// committed yet) and per-path comments.
//
// The top "unsaved changes" banner (rendered in Layout) subscribes to
// `useConfigDirtyCount`; FieldForm reads/writes drafts through `useConfigDraft`.

import {
  createContext,
  useCallback,
  useContext,
  useMemo,
  useState,
  type ReactNode,
} from 'react';
import { ApiError, patchConfig, type PatchOp, type PatchResponse } from './api';

type DraftEntry = {
  /** Raw input text the user has typed, as it appears in the input box. */
  input: string;
  /** Parsed JSON value to send on save. `undefined` means parse hasn't
   *  happened yet (input was just typed); FieldForm refreshes this on
   *  every keystroke so the save path can stay parse-free. */
  value?: unknown;
};

type ConfigDraftState = {
  drafts: Record<string, DraftEntry>;
  comments: Record<string, string>;
  tombstones: Set<string>;
};

type ConfigDraftCtx = ConfigDraftState & {
  setDraft: (path: string, input: string, value?: unknown) => void;
  clearDraft: (path: string) => void;
  setComment: (path: string, comment: string) => void;
  clearComment: (path: string) => void;
  stageTombstone: (path: string) => void;
  unstageTombstone: (path: string) => void;
  /** Drop drafts / comments / tombstones for exactly these paths. */
  discardPaths: (paths: readonly string[]) => void;
  /** Drop every draft / comment / tombstone whose path begins with `prefix`. */
  discardSection: (prefix: string) => void;
  /** Drop everything. */
  discardAll: () => void;
  /** Commit every staged draft + tombstone as a single PATCH and clear on success. */
  saveAll: () => Promise<PatchResponse>;
};

const Context = createContext<ConfigDraftCtx | null>(null);

export function ConfigDraftProvider({ children }: { children: ReactNode }) {
  const [drafts, setDrafts] = useState<Record<string, DraftEntry>>({});
  const [comments, setComments] = useState<Record<string, string>>({});
  const [tombstones, setTombstones] = useState<Set<string>>(() => new Set());

  const setDraft = useCallback((path: string, input: string, value?: unknown) => {
    setDrafts((prev) => ({ ...prev, [path]: { input, value } }));
  }, []);

  const clearDraft = useCallback((path: string) => {
    setDrafts((prev) => {
      if (!(path in prev)) return prev;
      const next = { ...prev };
      delete next[path];
      return next;
    });
  }, []);

  const setComment = useCallback((path: string, comment: string) => {
    setComments((prev) => ({ ...prev, [path]: comment }));
  }, []);

  const clearComment = useCallback((path: string) => {
    setComments((prev) => {
      if (!(path in prev)) return prev;
      const next = { ...prev };
      delete next[path];
      return next;
    });
  }, []);

  const stageTombstone = useCallback((path: string) => {
    setTombstones((prev) => {
      if (prev.has(path)) return prev;
      const next = new Set(prev);
      next.add(path);
      return next;
    });
    // Tombstoning supersedes any draft for the same path.
    setDrafts((prev) => {
      if (!(path in prev)) return prev;
      const next = { ...prev };
      delete next[path];
      return next;
    });
  }, []);

  const unstageTombstone = useCallback((path: string) => {
    setTombstones((prev) => {
      if (!prev.has(path)) return prev;
      const next = new Set(prev);
      next.delete(path);
      return next;
    });
  }, []);

  const discardPaths = useCallback((paths: readonly string[]) => {
    const discard = new Set(paths);
    if (discard.size === 0) return;
    setDrafts((prev) => {
      const next: Record<string, DraftEntry> = {};
      let changed = false;
      for (const [k, v] of Object.entries(prev)) {
        if (discard.has(k)) {
          changed = true;
        } else {
          next[k] = v;
        }
      }
      return changed ? next : prev;
    });
    setComments((prev) => {
      const next: Record<string, string> = {};
      let changed = false;
      for (const [k, v] of Object.entries(prev)) {
        if (discard.has(k)) {
          changed = true;
        } else {
          next[k] = v;
        }
      }
      return changed ? next : prev;
    });
    setTombstones((prev) => {
      let changed = false;
      const next = new Set<string>();
      for (const k of prev) {
        if (discard.has(k)) {
          changed = true;
        } else {
          next.add(k);
        }
      }
      return changed ? next : prev;
    });
  }, []);

  const discardSection = useCallback((prefix: string) => {
    const hasPrefix = (k: string) => k === prefix || k.startsWith(`${prefix}.`);
    setDrafts((prev) => {
      const next: Record<string, DraftEntry> = {};
      let changed = false;
      for (const [k, v] of Object.entries(prev)) {
        if (hasPrefix(k)) {
          changed = true;
        } else {
          next[k] = v;
        }
      }
      return changed ? next : prev;
    });
    setComments((prev) => {
      const next: Record<string, string> = {};
      let changed = false;
      for (const [k, v] of Object.entries(prev)) {
        if (hasPrefix(k)) {
          changed = true;
        } else {
          next[k] = v;
        }
      }
      return changed ? next : prev;
    });
    setTombstones((prev) => {
      let changed = false;
      const next = new Set<string>();
      for (const k of prev) {
        if (hasPrefix(k)) {
          changed = true;
        } else {
          next.add(k);
        }
      }
      return changed ? next : prev;
    });
  }, []);

  const discardAll = useCallback(() => {
    setDrafts({});
    setComments({});
    setTombstones(new Set());
  }, []);

  const saveAll = useCallback(async (): Promise<PatchResponse> => {
    const ops: PatchOp[] = [];
    const draftedPaths = new Set<string>();
    for (const [path, entry] of Object.entries(drafts)) {
      draftedPaths.add(path);
      const op: PatchOp = { op: 'replace', path, value: entry.value };
      const c = comments[path];
      if (c && c.length > 0) op.comment = c;
      ops.push(op);
    }
    for (const [path, comment] of Object.entries(comments)) {
      if (draftedPaths.has(path) || tombstones.has(path)) continue;
      ops.push({ op: 'comment', path, comment });
    }
    for (const path of tombstones) {
      if (draftedPaths.has(path)) continue;
      ops.push({ op: 'remove', path });
    }
    if (ops.length === 0) {
      return { saved: true, results: [], warnings: [] };
    }
    try {
      const resp = await patchConfig(ops);
      // Successful save: clear everything that just landed.
      setDrafts({});
      setComments({});
      setTombstones(new Set());
      return resp;
    } catch (e) {
      // Surface ApiError unchanged so the banner can display field-bound errors.
      if (e instanceof ApiError) throw e;
      throw e;
    }
  }, [drafts, comments, tombstones]);

  const value = useMemo<ConfigDraftCtx>(
    () => ({
      drafts,
      comments,
      tombstones,
      setDraft,
      clearDraft,
      setComment,
      clearComment,
      stageTombstone,
      unstageTombstone,
      discardPaths,
      discardSection,
      discardAll,
      saveAll,
    }),
    [
      drafts,
      comments,
      tombstones,
      setDraft,
      clearDraft,
      setComment,
      clearComment,
      stageTombstone,
      unstageTombstone,
      discardPaths,
      discardSection,
      discardAll,
      saveAll,
    ],
  );

  return <Context.Provider value={value}>{children}</Context.Provider>;
}

export function useConfigDraft(): ConfigDraftCtx {
  const ctx = useContext(Context);
  if (!ctx) {
    throw new Error('useConfigDraft must be used inside <ConfigDraftProvider>');
  }
  return ctx;
}

/** Count of pending changes — drafts + comments + tombstones, deduplicated by path. */
export function useConfigDirtyCount(): number {
  const { drafts, comments, tombstones } = useConfigDraft();
  const seen = new Set<string>();
  for (const path of Object.keys(drafts)) seen.add(path);
  for (const path of Object.keys(comments)) seen.add(path);
  for (const path of tombstones) seen.add(path);
  return seen.size;
}

/** Aggregate top-level section keys (`channels`, `memory`, ...) that have
 *  any pending edits. Used by the banner to show which sections to revisit. */
export function useConfigDirtySections(): string[] {
  const { drafts, comments, tombstones } = useConfigDraft();
  const seen = new Set<string>();
  const collect = (path: string) => {
    const top = path.split('.', 1)[0];
    if (top) seen.add(top);
  };
  for (const path of Object.keys(drafts)) collect(path);
  for (const path of Object.keys(comments)) collect(path);
  for (const path of tombstones) collect(path);
  return [...seen].sort();
}
