import { useEffect, useRef, useState } from 'react';
import CodeMirror from '@uiw/react-codemirror';
import { githubLight } from '@uiw/codemirror-theme-github';
import { oneDark } from '@codemirror/theme-one-dark';
import { useTheme } from '@/hooks/useTheme';
import { graphDraft, type Sop } from '@/lib/sops';
import { t } from '@/lib/i18n';
import { isSopDraft } from '@/lib/sopDraft';

export default function SopSourceEditor({
  draft,
  storageKey,
  onApply,
  onDirty,
}: {
  draft: Sop;
  storageKey: string;
  onApply: (draft: Sop) => void;
  onDirty: (dirty: boolean) => void;
}) {
  const { theme } = useTheme();
  const canonical = JSON.stringify(draft, null, 2);
  const [source, setSource] = useState(() => {
    try {
      return sessionStorage.getItem(storageKey) ?? canonical;
    } catch {
      return canonical;
    }
  });
  const [dirty, setDirty] = useState(source !== canonical);
  const [error, setError] = useState('');
  const [checking, setChecking] = useState(false);
  const latest = useRef(canonical);
  latest.current = canonical;
  useEffect(() => {
    if (!dirty) setSource(canonical);
  }, [canonical, dirty]);
  useEffect(() => {
    onDirty(dirty);
  }, [dirty, onDirty]);
  useEffect(() => {
    try {
      if (dirty) sessionStorage.setItem(storageKey, source);
      else sessionStorage.removeItem(storageKey);
    } catch {
      /* Optional draft recovery. */
    }
  }, [source, dirty, storageKey]);
  const apply = async () => {
    setChecking(true);
    setError('');
    const original = latest.current;
    try {
      const value: unknown = JSON.parse(source);
      if (!isSopDraft(value)) throw new Error(t('workspace.invalid_sop'));
      const next = { ...draft, ...value } as Sop;
      // The gateway deserializer and graph validator own the SOP contract.
      const graph = await graphDraft(next);
      if (latest.current !== original)
        throw new Error(t('workspace.source_conflict'));
      const errors = graph.diagnostics.filter(
        (item) => item.severity === 'error',
      );
      if (errors.length)
        throw new Error(errors.map((item) => item.message).join('\n'));
      onApply(next);
      setDirty(false);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setChecking(false);
    }
  };
  return (
    <div className="flex min-h-0 flex-1 flex-col">
      <p className="px-4 py-3 text-xs text-pc-text-muted">
        {t('workspace.source_hint')}
      </p>
      <div className="min-h-0 flex-1 overflow-auto">
        <CodeMirror
          value={source}
          theme={theme === 'light' ? githubLight : oneDark}
          height="100%"
          aria-label={t('workspace.sop_definition')}
          editable={!checking}
          onChange={(value) => {
            setSource(value);
            setDirty(value !== latest.current);
          }}
          basicSetup={{ lineNumbers: true, foldGutter: true }}
        />
      </div>
      {error && (
        <p
          role="alert"
          className="whitespace-pre-wrap px-4 py-2 text-xs text-status-error"
        >
          {error}
        </p>
      )}
      <div className="flex items-center justify-end gap-3 border-t border-pc-border p-3">
        <button
          type="button"
          disabled={!dirty || checking}
          className="text-xs text-pc-text-muted disabled:opacity-40"
          onClick={() => {
            setSource(canonical);
            setDirty(false);
            setError('');
          }}
        >
          {t('workspace.reset_source')}
        </button>
        <button
          type="button"
          disabled={!dirty || checking}
          className="btn-primary px-3 py-2 text-xs disabled:opacity-40"
          onClick={() => void apply()}
        >
          {t(checking ? 'common.loading' : 'workspace.apply_source')}
        </button>
      </div>
    </div>
  );
}
