import { createContext, useContext, useCallback, useRef } from 'react';
import type { AttachmentReference } from '@/types/api';

/**
 * In-memory draft store that survives component unmounts but not page reloads.
 * Keyed by an arbitrary string (e.g. route path or conversation id).
 */

export interface DraftContextType {
  getDraft: (key: string) => string;
  setDraft: (key: string, value: string) => void;
  clearDraft: (key: string) => void;
  getAttachments: (key: string) => AttachmentReference[];
  setAttachments: (key: string, value: AttachmentReference[]) => void;
}

const EMPTY_ATTACHMENTS: AttachmentReference[] = [];

export const DraftContext = createContext<DraftContextType>({
  getDraft: () => '',
  setDraft: () => {},
  clearDraft: () => {},
  getAttachments: () => EMPTY_ATTACHMENTS,
  setAttachments: () => {},
});

export function useDraftStore(): DraftContextType {
  const store = useRef<Map<string, { text: string; attachments: AttachmentReference[] }>>(new Map());

  const getDraft = useCallback((key: string): string => {
    return store.current.get(key)?.text ?? '';
  }, []);

  const setDraft = useCallback((key: string, value: string): void => {
    store.current.set(key, { text: value, attachments: store.current.get(key)?.attachments ?? EMPTY_ATTACHMENTS });
  }, []);

  const clearDraft = useCallback((key: string): void => {
    store.current.delete(key);
  }, []);

  const getAttachments = useCallback((key: string): AttachmentReference[] => {
    return store.current.get(key)?.attachments ?? EMPTY_ATTACHMENTS;
  }, []);

  const setAttachments = useCallback((key: string, attachments: AttachmentReference[]): void => {
    store.current.set(key, { text: store.current.get(key)?.text ?? '', attachments });
  }, []);

  return { getDraft, setDraft, clearDraft, getAttachments, setAttachments };
}

export function useDraft(key: string) {
  const { getDraft, setDraft, clearDraft, getAttachments, setAttachments } = useContext(DraftContext);
  const updateAttachments = useCallback((update: (previous: AttachmentReference[]) => AttachmentReference[]) => {
    const next = update(getAttachments(key));
    setAttachments(key, next);
    return next;
  }, [key, getAttachments, setAttachments]);
  return {
    draft: getDraft(key),
    saveDraft: (value: string) => setDraft(key, value),
    clearDraft: () => clearDraft(key),
    attachments: getAttachments(key),
    updateAttachments,
  };
}
