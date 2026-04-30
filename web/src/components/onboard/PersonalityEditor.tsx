// Tabbed text editor for the per-workspace personality markdown files
// (SOUL.md, IDENTITY.md, USER.md, AGENTS.md, TOOLS.md, HEARTBEAT.md,
// BOOTSTRAP.md, MEMORY.md). The runtime injects these into the system
// prompt at request time — this component is the dashboard's authoring
// surface for them.
//
// `agent` is reserved for #5890 (multi-agent workspaces). Today it is
// passed through to the API client and ignored by the gateway.

import { useCallback, useEffect, useRef, useState } from 'react';
import { markdown } from '@codemirror/lang-markdown';
import { oneDark } from '@codemirror/theme-one-dark';
import CodeMirror from '@uiw/react-codemirror';
import ReactMarkdown from 'react-markdown';
import rehypeHighlight from 'rehype-highlight';
import remarkGfm from 'remark-gfm';
import {
  ApiError,
  PersonalityConflictError,
  getPersonalityFile,
  getPersonalityIndex,
  getPersonalityTemplates,
  putPersonalityFile,
  type PersonalityIndex,
  type PersonalityIndexEntry,
} from '../../lib/api';

interface BufferState {
  loaded: string;
  draft: string;
  loadedMtimeMs: number | null;
  exists: boolean;
  truncated: boolean;
}

interface Props {
  agent?: string;
}

export default function PersonalityEditor({ agent }: Props) {
  const [index, setIndex] = useState<PersonalityIndex | null>(null);
  const [active, setActive] = useState<string | null>(null);
  const [buffers, setBuffers] = useState<Record<string, BufferState>>({});
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [seeding, setSeeding] = useState(false);
  // null while the user hasn't picked a starting point yet. Auto-set to
  // 'blank' on load when at least one file already exists — the user
  // clearly isn't starting fresh, so the picker would be noise.
  const [pick, setPick] = useState<'default' | 'blank' | null>(null);
  const [conflict, setConflict] = useState<{
    filename: string;
    currentContent: string;
    currentMtimeMs: number | null;
  } | null>(null);

  // Edit ↔ Preview toggle for the active tab.
  const [preview, setPreview] = useState(false);

  const loadIndex = useCallback(async () => {
    try {
      const resp = await getPersonalityIndex(agent);
      setIndex(resp);
      setActive((prev) => prev ?? resp.files[0]?.filename ?? null);
      // If any file already exists, skip the starter-template picker —
      // the user has already authored at least one and shouldn't have
      // to dismiss a "fresh start" prompt every time they revisit.
      setPick((prev) => prev ?? (resp.files.some((f) => f.exists) ? 'blank' : null));
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [agent]);

  // Cached templates response — fetched on first need (bulk seed or
  // per-tab insert) and reused thereafter. Keeps clicks instant after
  // the first one and avoids hitting the gateway on every tab switch.
  const templatesCache = useRef<Map<string, string> | null>(null);

  const fetchTemplates = useCallback(async (): Promise<Map<string, string>> => {
    if (templatesCache.current) return templatesCache.current;
    const resp = await getPersonalityTemplates({}, 'default', agent);
    const map = new Map(resp.files.map((f) => [f.filename, f.content]));
    templatesCache.current = map;
    return map;
  }, [agent]);

  const seedDefaultTemplates = useCallback(async () => {
    if (!index) return;
    setSeeding(true);
    setError(null);
    try {
      const byFilename = await fetchTemplates();
      // Only seed files that don't already exist on disk — never
      // silently overwrite existing user content. Existing files get
      // lazy-loaded by tab activation as before.
      const seeded: Record<string, BufferState> = {};
      for (const entry of index.files) {
        if (entry.exists) continue;
        const template = byFilename.get(entry.filename);
        if (template === undefined) continue;
        seeded[entry.filename] = {
          loaded: '',
          draft: template,
          loadedMtimeMs: null,
          exists: false,
          truncated: false,
        };
      }
      setBuffers((prev) => ({ ...seeded, ...prev }));
      setPick('default');
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSeeding(false);
    }
  }, [fetchTemplates, index]);

  const insertTemplateIntoActive = useCallback(async () => {
    if (!active) return;
    setError(null);
    try {
      const byFilename = await fetchTemplates();
      const template = byFilename.get(active);
      if (template === undefined) {
        setError(`No template available for ${active}.`);
        return;
      }
      setBuffers((prev) => {
        const existing = prev[active];
        return {
          ...prev,
          [active]: {
            loaded: existing?.loaded ?? '',
            draft: template,
            loadedMtimeMs: existing?.loadedMtimeMs ?? null,
            exists: existing?.exists ?? false,
            truncated: existing?.truncated ?? false,
          },
        };
      });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [active, fetchTemplates]);

  useEffect(() => {
    void loadIndex();
  }, [loadIndex]);


  const loadFile = useCallback(
    async (filename: string) => {
      try {
        const file = await getPersonalityFile(filename, agent);
        setBuffers((prev) => ({
          ...prev,
          [filename]: {
            loaded: file.content,
            draft: file.content,
            loadedMtimeMs: file.mtime_ms,
            exists: file.exists,
            truncated: file.truncated,
          },
        }));
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      }
    },
    [agent],
  );

  // Lazy-load each tab's content the first time it's activated.
  useEffect(() => {
    if (!active) return;
    if (buffers[active]) return;
    void loadFile(active);
  }, [active, buffers, loadFile]);

  // Warn before navigating away when any buffer has unsaved changes.
  useEffect(() => {
    const dirty = Object.values(buffers).some((b) => b.draft !== b.loaded);
    if (!dirty) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', handler);
    return () => window.removeEventListener('beforeunload', handler);
  }, [buffers]);

  const activeBuf = active ? buffers[active] : undefined;
  const maxChars = index?.max_chars ?? 20_000;
  const charCount = activeBuf?.draft.length ?? 0;
  const overLimit = charCount > maxChars;
  const dirty = activeBuf ? activeBuf.draft !== activeBuf.loaded : false;

  const onSave = async () => {
    if (!active || !activeBuf) return;
    setSaving(true);
    setError(null);
    try {
      const result = await putPersonalityFile(
        active,
        activeBuf.draft,
        activeBuf.loadedMtimeMs,
        agent,
      );
      setBuffers((prev) => ({
        ...prev,
        [active]: {
          ...prev[active]!,
          loaded: activeBuf.draft,
          loadedMtimeMs: result.mtime_ms,
          exists: true,
        },
      }));
      // Refresh index so the "exists" / size dots update.
      void loadIndex();
    } catch (e) {
      if (e instanceof PersonalityConflictError) {
        setConflict({
          filename: e.conflict.filename,
          currentContent: e.conflict.current_content,
          currentMtimeMs: e.conflict.current_mtime_ms,
        });
      } else if (e instanceof ApiError) {
        setError(`[${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      setSaving(false);
    }
  };

  const resolveTakeTheirs = () => {
    if (!conflict || !active) return;
    setBuffers((prev) => ({
      ...prev,
      [active]: {
        ...prev[active]!,
        loaded: conflict.currentContent,
        draft: conflict.currentContent,
        loadedMtimeMs: conflict.currentMtimeMs,
        exists: true,
      },
    }));
    setConflict(null);
  };

  const resolveKeepMine = () => {
    if (!conflict || !active) return;
    // Adopt the disk's mtime so the next PUT passes the guard.
    setBuffers((prev) => ({
      ...prev,
      [active]: {
        ...prev[active]!,
        loadedMtimeMs: conflict.currentMtimeMs,
      },
    }));
    setConflict(null);
  };

  if (!index) {
    return (
      <div
        className="rounded-xl border p-6 text-sm"
        style={{
          borderColor: 'var(--pc-border)',
          background: 'var(--pc-bg-surface)',
          color: 'var(--pc-text-muted)',
        }}
      >
        {error ? `Couldn't load personality files: ${error}` : 'Loading…'}
      </div>
    );
  }

  if (pick === null) {
    return (
      <div className="flex flex-col gap-4">
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          These markdown files shape your agent's voice and context. Pick a
          starting point — you can edit each file before saving.
        </p>
        <div className="grid gap-3 md:grid-cols-2">
          <button
            type="button"
            disabled={seeding}
            onClick={() => void seedDefaultTemplates()}
            className="text-left rounded-xl border p-4 transition-colors hover:border-[var(--pc-accent)]"
            style={{
              borderColor: 'var(--pc-border)',
              background: 'var(--pc-bg-surface)',
            }}
          >
            <div className="font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>
              {seeding ? 'Loading templates…' : 'Use the default templates'}
            </div>
            <div className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              Pre-fills every file with a sensible starter (identity, tone,
              boundaries, session ritual). Nothing is saved until you click
              Save in each tab. Existing files are left untouched.
            </div>
          </button>
          <button
            type="button"
            onClick={() => setPick('blank')}
            className="text-left rounded-xl border p-4 transition-colors hover:border-[var(--pc-accent)]"
            style={{
              borderColor: 'var(--pc-border)',
              background: 'var(--pc-bg-surface)',
            }}
          >
            <div className="font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>
              Start blank
            </div>
            <div className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              Open the editor with empty buffers and write each file
              from scratch.
            </div>
          </button>
        </div>
        {error && (
          <div
            className="rounded-lg border p-3 text-sm"
            style={{
              background: 'rgba(239, 68, 68, 0.08)',
              borderColor: 'rgba(239, 68, 68, 0.2)',
              color: '#f87171',
            }}
          >
            {error}
          </div>
        )}
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
        These markdown files shape your agent's voice and context. The
        runtime reads them at every request, so changes take effect on the
        next message — no daemon reload needed.
      </p>

      {/* Tab strip */}
      <div
        className="flex flex-wrap gap-1 border-b"
        style={{ borderColor: 'var(--pc-border)' }}
      >
        {index.files.map((f) => (
          <PersonalityTab
            key={f.filename}
            entry={f}
            active={f.filename === active}
            dirty={
              !!buffers[f.filename] &&
              buffers[f.filename]!.draft !== buffers[f.filename]!.loaded
            }
            onSelect={() => setActive(f.filename)}
          />
        ))}
      </div>

      {/* Editor */}
      {active && (
        <div className="flex flex-col gap-2">
          {/* Edit ↔ Preview segmented toggle */}
          <div
            className="inline-flex self-end rounded-lg border overflow-hidden"
            style={{ borderColor: 'var(--pc-border)' }}
          >
            <button
              type="button"
              onClick={() => setPreview(false)}
              className="text-xs px-3 py-1 transition-colors"
              style={{
                background: !preview ? 'var(--pc-accent-glow)' : 'transparent',
                color: !preview ? 'var(--pc-accent)' : 'var(--pc-text-secondary)',
                fontWeight: !preview ? 600 : 400,
              }}
            >
              Edit
            </button>
            <button
              type="button"
              onClick={() => setPreview(true)}
              className="text-xs px-3 py-1 transition-colors"
              style={{
                background: preview ? 'var(--pc-accent-glow)' : 'transparent',
                color: preview ? 'var(--pc-accent)' : 'var(--pc-text-secondary)',
                fontWeight: preview ? 600 : 400,
              }}
            >
              Preview
            </button>
          </div>

          {preview ? (
            <div
              className="prose prose-invert max-w-none rounded-md border px-4 py-3 text-sm overflow-y-auto"
              style={{
                borderColor: 'var(--pc-border)',
                background: 'var(--pc-bg-base)',
                minHeight: '20rem',
              }}
            >
              {(activeBuf?.draft ?? '').trim().length > 0 ? (
                <ReactMarkdown
                  remarkPlugins={[remarkGfm]}
                  rehypePlugins={[[rehypeHighlight, { detect: true, ignoreMissing: true }]]}
                >
                  {activeBuf?.draft ?? ''}
                </ReactMarkdown>
              ) : (
                <p style={{ color: 'var(--pc-text-muted)' }}>
                  Nothing to preview yet — switch to Edit and add content, or
                  click Insert template.
                </p>
              )}
            </div>
          ) : (
            <div
              className="rounded-md border overflow-hidden"
              style={{ borderColor: 'var(--pc-border)' }}
            >
              <CodeMirror
                value={activeBuf?.draft ?? ''}
                onChange={(value) =>
                  setBuffers((prev) => ({
                    ...prev,
                    [active]: {
                      loaded: prev[active]?.loaded ?? '',
                      draft: value,
                      loadedMtimeMs: prev[active]?.loadedMtimeMs ?? null,
                      exists: prev[active]?.exists ?? false,
                      truncated: prev[active]?.truncated ?? false,
                    },
                  }))
                }
                extensions={[markdown()]}
                theme={oneDark}
                height="32rem"
                basicSetup={{
                  lineNumbers: true,
                  highlightActiveLine: true,
                  foldGutter: true,
                  bracketMatching: true,
                }}
                placeholder={`# ${active}\n\n…`}
              />
            </div>
          )}
          <div
            className="flex items-center justify-between text-xs"
            style={{ color: 'var(--pc-text-muted)' }}
          >
            <span>
              {charCount.toLocaleString()} / {maxChars.toLocaleString()} chars
              {overLimit && (
                <span style={{ color: 'var(--color-status-error)' }}>
                  {' '}— over limit, save will be rejected
                </span>
              )}
              {activeBuf?.truncated && (
                <span style={{ color: 'var(--color-status-warn)' }}>
                  {' '}— file on disk exceeds limit; saving will overwrite with truncated content
                </span>
              )}
            </span>
            <div className="flex items-center gap-2">
              <button
                type="button"
                onClick={() => {
                  // Replacing real content needs explicit confirmation;
                  // empty buffers can take the template silently.
                  const hasContent = (activeBuf?.draft ?? '').trim().length > 0;
                  if (
                    !hasContent ||
                    window.confirm(
                      `Replace the current ${active} buffer with the default template? Unsaved edits will be lost.`,
                    )
                  ) {
                    void insertTemplateIntoActive();
                  }
                }}
                className="btn-secondary text-sm px-3 py-1.5"
                title="Fill this tab with the default starter template"
              >
                {(activeBuf?.draft ?? '').trim().length > 0
                  ? 'Replace with template'
                  : 'Insert template'}
              </button>
              <button
                type="button"
                disabled={!dirty || saving || overLimit}
                onClick={() => void onSave()}
                className="btn-electric text-sm px-4 py-1.5"
              >
                {saving ? 'Saving…' : 'Save'}
              </button>
            </div>
          </div>
        </div>
      )}

      {error && (
        <div
          className="rounded-lg border p-3 text-sm"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {error}
        </div>
      )}

      {conflict && (
        <div
          className="rounded-lg border p-4 text-sm flex flex-col gap-3"
          style={{
            background: 'rgba(245, 158, 11, 0.08)',
            borderColor: 'rgba(245, 158, 11, 0.3)',
          }}
        >
          <div style={{ color: 'var(--pc-text-primary)' }}>
            <strong>{conflict.filename}</strong> changed on disk while you
            were editing. Pick how to resolve:
          </div>
          <div className="flex gap-2 flex-wrap">
            <button
              type="button"
              onClick={resolveTakeTheirs}
              className="btn-secondary text-sm px-3 py-1.5"
              title="Replace your buffer with the on-disk content"
            >
              Take theirs (discard my edits)
            </button>
            <button
              type="button"
              onClick={resolveKeepMine}
              className="btn-secondary text-sm px-3 py-1.5"
              title="Keep your edits and overwrite disk on next save"
            >
              Keep mine (overwrite on next save)
            </button>
          </div>
          <details>
            <summary
              className="cursor-pointer text-xs"
              style={{ color: 'var(--pc-text-muted)' }}
            >
              Show on-disk content
            </summary>
            <pre
              className="mt-2 p-2 text-xs rounded font-mono whitespace-pre-wrap break-all"
              style={{
                background: 'var(--pc-bg-base)',
                color: 'var(--pc-text-secondary)',
                maxHeight: 200,
                overflow: 'auto',
              }}
            >
              {conflict.currentContent || '(empty)'}
            </pre>
          </details>
        </div>
      )}
    </div>
  );
}

interface TabProps {
  entry: PersonalityIndexEntry;
  active: boolean;
  dirty: boolean;
  onSelect: () => void;
}

function PersonalityTab({ entry, active, dirty, onSelect }: TabProps) {
  return (
    <button
      type="button"
      onClick={onSelect}
      className="text-sm px-3 py-2 inline-flex items-center gap-2 transition-colors"
      style={{
        background: active ? 'var(--pc-accent-glow)' : 'transparent',
        color: active ? 'var(--pc-accent)' : 'var(--pc-text-primary)',
        fontWeight: active ? 600 : 400,
        borderBottom: active
          ? '2px solid var(--pc-accent)'
          : '2px solid transparent',
        marginBottom: -1,
      }}
    >
      <span
        className="h-1.5 w-1.5 rounded-full"
        style={{
          background: entry.exists
            ? 'var(--color-status-success)'
            : 'var(--pc-border)',
        }}
        title={entry.exists ? 'Saved on disk' : 'Not yet created'}
      />
      <span>{entry.filename}</span>
      {dirty && (
        <span
          className="h-1.5 w-1.5 rounded-full"
          style={{ background: 'var(--color-status-warn)' }}
          title="Unsaved changes"
        />
      )}
    </button>
  );
}
