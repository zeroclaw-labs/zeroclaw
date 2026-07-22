/**
 * Direct mode — raw markdown editors for SOUL.md / IDENTITY.md / USER.md /
 * HEARTBEAT.md with mtime-guarded saves and 409 conflict resolution
 * (keep mine / take theirs, with a side-by-side comparison).
 */

import { useCallback, useEffect, useState } from 'react';
import { Loader2, Save } from 'lucide-react';
import MarkdownEditor from '@/components/MarkdownEditor';
import {
  PersonalityConflictError,
  getPersonalityFile,
  putPersonalityFile,
} from '@/lib/api';
import { ErrorNote, S } from './studioUi';

const FILES = ['SOUL.md', 'IDENTITY.md', 'USER.md', 'HEARTBEAT.md'] as const;
type Filename = (typeof FILES)[number];

interface BufferState {
  loaded: string;
  draft: string;
  loadedMtimeMs: number | null;
  exists: boolean;
  truncated: boolean;
}

interface Conflict {
  filename: Filename;
  currentContent: string;
  currentMtimeMs: number | null;
}

interface Props {
  agent: string;
}

export default function DirectMode({ agent }: Props) {
  const [active, setActive] = useState<Filename>('SOUL.md');
  const [buffers, setBuffers] = useState<Partial<Record<Filename, BufferState>>>({});
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [conflict, setConflict] = useState<Conflict | null>(null);

  const loadFile = useCallback(
    async (filename: Filename) => {
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

  // Reset buffers when the agent changes; lazy-load the active tab.
  useEffect(() => {
    setBuffers({});
    setConflict(null);
    setError(null);
  }, [agent]);

  useEffect(() => {
    if (buffers[active]) return;
    void loadFile(active);
  }, [active, buffers, loadFile]);

  // Warn before navigating away with unsaved edits.
  useEffect(() => {
    const dirty = Object.values(buffers).some((b) => b && b.draft !== b.loaded);
    if (!dirty) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
      e.returnValue = '';
    };
    window.addEventListener('beforeunload', handler);
    return () => window.removeEventListener('beforeunload', handler);
  }, [buffers]);

  const buf = buffers[active];
  const dirty = buf ? buf.draft !== buf.loaded : false;

  const onSave = async () => {
    if (!buf) return;
    setSaving(true);
    setError(null);
    try {
      const result = await putPersonalityFile(active, buf.draft, buf.loadedMtimeMs, agent);
      setBuffers((prev) => ({
        ...prev,
        [active]: {
          ...prev[active]!,
          loaded: buf.draft,
          loadedMtimeMs: result.mtime_ms,
          exists: true,
        },
      }));
    } catch (e) {
      if (e instanceof PersonalityConflictError) {
        setConflict({
          filename: active,
          currentContent: e.conflict.current_content,
          currentMtimeMs: e.conflict.current_mtime_ms,
        });
      } else {
        setError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      setSaving(false);
    }
  };

  /** Keep my draft: adopt the disk mtime so the next save wins. */
  const resolveKeepMine = () => {
    if (!conflict) return;
    setBuffers((prev) => ({
      ...prev,
      [conflict.filename]: {
        ...prev[conflict.filename]!,
        loadedMtimeMs: conflict.currentMtimeMs,
      },
    }));
    setConflict(null);
  };

  /** Take theirs: replace my draft with what's on disk. */
  const resolveTakeTheirs = () => {
    if (!conflict) return;
    setBuffers((prev) => ({
      ...prev,
      [conflict.filename]: {
        ...prev[conflict.filename]!,
        loaded: conflict.currentContent,
        draft: conflict.currentContent,
        loadedMtimeMs: conflict.currentMtimeMs,
        exists: true,
      },
    }));
    setConflict(null);
  };

  return (
    <div className="flex flex-col gap-3">
      {/* File tabs */}
      <div className="flex flex-wrap gap-1 border-b" style={{ borderColor: S.border }}>
        {FILES.map((f) => {
          const b = buffers[f];
          const fileDirty = !!b && b.draft !== b.loaded;
          const isActive = f === active;
          return (
            <button
              key={f}
              type="button"
              onClick={() => setActive(f)}
              className="inline-flex items-center gap-2 px-3 py-2 font-mono text-xs transition-colors"
              style={{
                color: isActive ? S.accent : S.muted,
                fontWeight: isActive ? 600 : 400,
                borderBottom: isActive
                  ? `2px solid ${S.accent}`
                  : '2px solid transparent',
                marginBottom: -1,
              }}
            >
              {f}
              {fileDirty && (
                <span
                  className="h-1.5 w-1.5 rounded-full"
                  style={{ background: S.accent }}
                  title="Unsaved changes"
                />
              )}
            </button>
          );
        })}
      </div>

      {buf ? (
        <MarkdownEditor
          value={buf.draft}
          onChange={(value) =>
            setBuffers((prev) => ({
              ...prev,
              [active]: { ...prev[active]!, draft: value },
            }))
          }
          height="30rem"
          placeholder={`# ${active}\n\n…`}
        />
      ) : (
        <div
          className="flex h-48 items-center justify-center rounded-xl border text-sm"
          style={{ borderColor: S.border, color: S.faint, background: S.surface }}
        >
          Loading {active}…
        </div>
      )}

      <div className="flex items-center justify-between text-xs" style={{ color: S.faint }}>
        <span>
          {buf ? `${buf.draft.length.toLocaleString()} chars` : ''}
          {buf?.truncated && (
            <span style={{ color: '#fbbf24' }}> — truncated on read; saving writes only what you see</span>
          )}
          {buf && !buf.exists && <span> — not created yet</span>}
        </span>
        <button
          type="button"
          disabled={!dirty || saving}
          onClick={() => void onSave()}
          className="inline-flex items-center gap-2 rounded-lg px-4 py-2 text-sm font-semibold transition-opacity disabled:opacity-50"
          style={{ background: S.accent, color: '#000' }}
        >
          {saving ? <Loader2 size={14} className="animate-spin" /> : <Save size={14} />}
          {saving ? 'Saving…' : `Save ${active}`}
        </button>
      </div>

      {error && <ErrorNote>{error}</ErrorNote>}

      {conflict && (
        <div
          className="flex flex-col gap-3 rounded-xl border p-4 text-sm"
          style={{
            background: 'rgba(245, 158, 11, 0.08)',
            borderColor: 'rgba(245, 158, 11, 0.3)',
          }}
        >
          <div style={{ color: S.text }}>
            <strong className="font-mono">{conflict.filename}</strong> changed on
            disk while you were editing. Compare the two versions and choose.
          </div>
          <div className="grid gap-3 md:grid-cols-2">
            <ConflictPane label="Mine (in editor)" text={buffers[conflict.filename]?.draft ?? ''} />
            <ConflictPane label="Theirs (on disk)" text={conflict.currentContent} />
          </div>
          <div className="flex flex-wrap gap-2">
            <button
              type="button"
              onClick={resolveKeepMine}
              className="rounded-lg px-3 py-1.5 text-sm font-semibold"
              style={{ background: S.accent, color: '#000' }}
              title="Keep my draft; the next save overwrites the disk version"
            >
              Keep mine
            </button>
            <button
              type="button"
              onClick={resolveTakeTheirs}
              className="rounded-lg border px-3 py-1.5 text-sm"
              style={{ borderColor: S.border, color: S.text }}
              title="Discard my draft and load the disk version"
            >
              Take theirs
            </button>
          </div>
        </div>
      )}
    </div>
  );
}

function ConflictPane({ label, text }: { label: string; text: string }) {
  return (
    <div className="flex min-w-0 flex-col gap-1">
      <span className="text-[10px] font-semibold uppercase tracking-[0.12em]" style={{ color: S.faint }}>
        {label}
      </span>
      <pre
        className="max-h-52 overflow-auto whitespace-pre-wrap break-words rounded-lg border p-2.5 font-mono text-xs leading-relaxed"
        style={{ background: S.bg, borderColor: S.border, color: S.muted }}
      >
        {text || '(empty)'}
      </pre>
    </div>
  );
}
