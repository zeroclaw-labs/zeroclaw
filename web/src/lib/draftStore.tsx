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
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from 'react';
import {
  ApiError,
  patchConfig,
  saveProviderRegistry,
  type PatchOp,
  type PatchResponse,
  type ProviderRegistryDocument,
} from './api';
import { t } from './i18n';

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
  providerRegistryDraft?: ProviderRegistryDraft;
};

/**
 * The provider registry is saved as one compare-and-swap document rather than
 * as dotted config properties. Keep its expected snapshot and the editor's
 * raw JSON together so the draft survives the registry section unmounting.
 */
export type ProviderRegistryDraft = {
  expected: ProviderRegistryDocument;
  sections: Record<string, string>;
  dirty: boolean;
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
  setProviderRegistryDraft: (draft: ProviderRegistryDraft) => void;
  clearProviderRegistryDraft: () => void;
  /** Commit every staged draft + tombstone as a single PATCH and clear on success. */
  saveAll: () => Promise<PatchResponse>;
};

const Context = createContext<ConfigDraftCtx | null>(null);

export function ConfigDraftProvider({ children }: { children: ReactNode }) {
  const [drafts, setDrafts] = useState<Record<string, DraftEntry>>({});
  const [comments, setComments] = useState<Record<string, string>>({});
  const [tombstones, setTombstones] = useState<Set<string>>(() => new Set());
  const [providerRegistryDraft, setProviderRegistryDraftState] =
    useState<ProviderRegistryDraft>();

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
    setProviderRegistryDraftState(undefined);
  }, []);

  const setProviderRegistryDraft = useCallback((draft: ProviderRegistryDraft) => {
    setProviderRegistryDraftState(draft);
  }, []);

  const clearProviderRegistryDraft = useCallback(() => {
    setProviderRegistryDraftState(undefined);
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
    // Registry replacement and dotted-property PATCH use separate server
    // transactions. Refuse to partially commit a mixed Save All request;
    // save the registry from its editor first, then save the remaining draft.
    if (providerRegistryDraft?.dirty && ops.length > 0) {
      throw new Error(t('unsaved_banner.registry_save_separately'));
    }
    if (providerRegistryDraft?.dirty) {
      const registry: Record<string, unknown> = {};
      for (const [section, input] of Object.entries(providerRegistryDraft.sections)) {
        try {
          registry[section] = JSON.parse(input);
        } catch (error) {
          throw error instanceof Error ? error : new Error(String(error));
        }
      }
      await saveProviderRegistry(
        providerRegistryDraft.expected,
        registry as ProviderRegistryDocument,
      );
      setProviderRegistryDraftState(undefined);
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
  }, [drafts, comments, tombstones, providerRegistryDraft]);

  // The drafts outlive the config form, so the warning before leaving the
  // page lives here: a reload or a closed tab asks first while anything is
  // unsaved, whichever page is showing.
  const unsaved =
    Object.keys(drafts).length > 0 ||
    Object.keys(comments).length > 0 ||
    tombstones.size > 0 ||
    providerRegistryDraft?.dirty === true;
  useEffect(() => {
    if (!unsaved) return;
    const warn = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', warn);
    return () => window.removeEventListener('beforeunload', warn);
  }, [unsaved]);

  const value = useMemo<ConfigDraftCtx>(
    () => ({
      drafts,
      comments,
      tombstones,
      providerRegistryDraft,
      setDraft,
      clearDraft,
      setComment,
      clearComment,
      stageTombstone,
      unstageTombstone,
      discardPaths,
      discardSection,
      discardAll,
      setProviderRegistryDraft,
      clearProviderRegistryDraft,
      saveAll,
    }),
    [
      drafts,
      comments,
      tombstones,
      providerRegistryDraft,
      setDraft,
      clearDraft,
      setComment,
      clearComment,
      stageTombstone,
      unstageTombstone,
      discardPaths,
      discardSection,
      discardAll,
      setProviderRegistryDraft,
      clearProviderRegistryDraft,
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

/** Count of pending changes — field drafts plus atomic registry drafts. */
export function useConfigDirtyCount(): number {
  const { drafts, comments, tombstones, providerRegistryDraft } = useConfigDraft();
  const seen = new Set<string>();
  for (const path of Object.keys(drafts)) seen.add(path);
  for (const path of Object.keys(comments)) seen.add(path);
  for (const path of tombstones) seen.add(path);
  if (providerRegistryDraft?.dirty) seen.add('providers.registry');
  return seen.size;
}

/** Aggregate top-level section keys (`channels`, `memory`, ...) that have
 *  any pending edits. Used by the banner to show which sections to revisit. */
export function useConfigDirtySections(): string[] {
  const { drafts, comments, tombstones, providerRegistryDraft } = useConfigDraft();
  const seen = new Set<string>();
  const collect = (path: string) => {
    const top = path.split('.', 1)[0];
    if (top) seen.add(top);
  };
  for (const path of Object.keys(drafts)) collect(path);
  for (const path of Object.keys(comments)) collect(path);
  for (const path of tombstones) collect(path);
  if (providerRegistryDraft?.dirty) collect('providers.registry');
  return [...seen].sort();
}
