// Drill-in editor for a single skill-bundle: list the skills in the
// bundle's resolved directory, scaffold new ones (strict-spec: name +
// description required), open a selected skill in a CodeMirror SKILL.md
// editor, and archive a skill on demand.
//
// Lazy-load pattern mirrors PersonalityEditor — list endpoint hydrates
// the picker, individual SKILL.md content is fetched on selection.

import { useCallback, useEffect, useState } from 'react';
import { markdown } from '@codemirror/lang-markdown';
import { oneDark } from '@codemirror/theme-one-dark';
import { githubLight } from '@uiw/codemirror-theme-github';
import CodeMirror from '@uiw/react-codemirror';
import { useTheme } from '@/hooks/useTheme';
import {
  createSkill,
  deleteSkill,
  listSkillsInBundle,
  readSkill,
  writeSkill,
  type SkillEntry,
  type SkillFrontmatter,
} from '../../lib/api';

interface Props {
  bundle: string;
}

interface EditorBuffer {
  loaded: { frontmatter: SkillFrontmatter; body: string };
  draft: { frontmatter: SkillFrontmatter; body: string };
}

export default function SkillsBundleEditor({ bundle }: Props) {
  // Match the SKILL.md editor to the active console scheme — a dark CodeMirror
  // theme inside a light palette is the light-mode bug we're fixing.
  // `resolvedTheme` is 'dark' | 'light' | 'oled'; only 'light' is a light scheme.
  const { resolvedTheme } = useTheme();
  const cmTheme = resolvedTheme === 'light' ? githubLight : oneDark;

  const [skills, setSkills] = useState<SkillEntry[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [buffer, setBuffer] = useState<EditorBuffer | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [creating, setCreating] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);
  const [newName, setNewName] = useState('');
  const [newDescription, setNewDescription] = useState('');

  const loadList = useCallback(async () => {
    try {
      const resp = await listSkillsInBundle(bundle);
      setSkills(resp.skills);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [bundle]);

  useEffect(() => {
    void loadList();
  }, [loadList]);

  const loadActive = useCallback(async () => {
    if (!active) return;
    setError(null);
    setConfirmDelete(false);
    try {
      const doc = await readSkill(bundle, active);
      const loaded = { frontmatter: doc.frontmatter, body: doc.body };
      setBuffer({ loaded, draft: structuredClone(loaded) });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
      setBuffer(null);
    }
  }, [active, bundle]);

  useEffect(() => {
    void loadActive();
  }, [loadActive]);

  const dirty =
    buffer !== null && JSON.stringify(buffer.draft) !== JSON.stringify(buffer.loaded);

  const onSave = async () => {
    if (!active || !buffer) return;
    setBusy(true);
    setError(null);
    try {
      await writeSkill(bundle, active, buffer.draft);
      setBuffer({ loaded: structuredClone(buffer.draft), draft: buffer.draft });
      void loadList();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onCreate = async () => {
    const name = newName.trim();
    const description = newDescription.trim();
    if (!name) {
      setError('Name is required.');
      return;
    }
    if (!description) {
      setError('Description is required.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await createSkill(bundle, {
        name,
        frontmatter: {
          name,
          description,
          version: '0.1.0',
        },
        body: '',
      });
      setNewName('');
      setNewDescription('');
      setCreating(false);
      await loadList();
      setActive(name);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const onDelete = async () => {
    if (!active) return;
    setBusy(true);
    setError(null);
    try {
      await deleteSkill(bundle, active, false);
      setBuffer(null);
      setActive(null);
      setConfirmDelete(false);
      await loadList();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  };

  const setFrontmatterField = (field: keyof SkillFrontmatter, value: string) => {
    setBuffer((prev) =>
      prev
        ? {
            ...prev,
            draft: {
              ...prev.draft,
              frontmatter: {
                ...prev.draft.frontmatter,
                [field]: value || (field === 'name' || field === 'description' ? '' : null),
              },
            },
          }
        : prev,
    );
  };

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
        Skills in this bundle live under its configured directory. Each is a folder
        with a canonical <code>SKILL.md</code> (frontmatter + body) plus optional
        <code> scripts/</code>, <code>references/</code>, and <code>assets/</code> subdirs.
      </p>

      {/* Skill picker strip */}
      <div className="flex flex-wrap gap-2 items-center">
        {skills.map((s) => (
          <button
            key={s.name}
            type="button"
            onClick={() => setActive(s.name)}
            className="text-xs px-3 py-1.5 rounded-lg border transition-colors"
            style={{
              borderColor: 'var(--pc-border)',
              background: s.name === active ? 'var(--pc-accent-glow)' : 'transparent',
              color: s.name === active ? 'var(--pc-accent)' : 'var(--pc-text-secondary)',
              fontWeight: s.name === active ? 600 : 400,
            }}
          >
            {s.name}
          </button>
        ))}
        {!creating && (
          <button
            type="button"
            onClick={() => {
              setCreating(true);
              setError(null);
            }}
            className="text-xs px-3 py-1.5 rounded-lg border-dashed border"
            style={{ borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)' }}
          >
            + New skill
          </button>
        )}
        {skills.length === 0 && !creating && (
          <span className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
            (no skills installed)
          </span>
        )}
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

      {/* Create form */}
      {creating && (
        <div
          className="rounded-xl border p-4 flex flex-col gap-3"
          style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
        >
          <div className="flex flex-col gap-1">
            <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              Name (lowercase + hyphens)
            </label>
            <input
              type="text"
              value={newName}
              onChange={(e) => setNewName(e.target.value)}
              placeholder="my-skill"
              className="rounded-md border bg-transparent px-3 py-1.5 text-sm"
              style={{ borderColor: 'var(--pc-border)' }}
            />
          </div>
          <div className="flex flex-col gap-1">
            <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
              Description (what it does, when to use it — third person)
            </label>
            <textarea
              value={newDescription}
              onChange={(e) => setNewDescription(e.target.value)}
              rows={3}
              placeholder="Reviews pull requests for correctness, security, and style. Use when..."
              className="rounded-md border bg-transparent px-3 py-1.5 text-sm font-mono"
              style={{ borderColor: 'var(--pc-border)' }}
            />
          </div>
          <div className="flex gap-2">
            <button
              type="button"
              disabled={busy}
              onClick={() => void onCreate()}
              className="btn-primary text-sm"
            >
              Create skill
            </button>
            <button
              type="button"
              onClick={() => {
                setCreating(false);
                setNewName('');
                setNewDescription('');
                setError(null);
              }}
              className="btn-secondary text-sm"
            >
              Cancel
            </button>
          </div>
        </div>
      )}

      {/* Editor */}
      {active && buffer && (
        <div className="flex flex-col gap-3">
          <FrontmatterForm
            value={buffer.draft.frontmatter}
            onChange={setFrontmatterField}
          />
          <div
            className="rounded-xl border overflow-hidden"
            style={{ borderColor: 'var(--pc-border)' }}
          >
            <CodeMirror
              value={buffer.draft.body}
              height="320px"
              theme={cmTheme}
              extensions={[markdown()]}
              onChange={(v) =>
                setBuffer((prev) =>
                  prev ? { ...prev, draft: { ...prev.draft, body: v } } : prev,
                )
              }
            />
          </div>
          <div className="flex items-center gap-2">
            <button
              type="button"
              disabled={!dirty || busy}
              onClick={() => void onSave()}
              className="btn-primary text-sm"
            >
              {busy ? 'Saving…' : 'Save'}
            </button>
            <button
              type="button"
              disabled={!dirty || busy}
              onClick={() =>
                setBuffer((prev) =>
                  prev ? { ...prev, draft: structuredClone(prev.loaded) } : prev,
                )
              }
              className="btn-secondary text-sm"
            >
              Discard
            </button>
            <div className="flex-1" />
            {!confirmDelete ? (
              <button
                type="button"
                onClick={() => setConfirmDelete(true)}
                className="btn-secondary text-sm"
                style={{ color: '#f87171' }}
              >
                Archive skill
              </button>
            ) : (
              <>
                <span className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                  Move to <code>shared/skills/_deleted/</code>?
                </span>
                <button
                  type="button"
                  disabled={busy}
                  onClick={() => void onDelete()}
                  className="btn-primary text-sm"
                  style={{ background: '#dc2626' }}
                >
                  Confirm archive
                </button>
                <button
                  type="button"
                  onClick={() => setConfirmDelete(false)}
                  className="btn-secondary text-sm"
                >
                  Cancel
                </button>
              </>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

interface FrontmatterFormProps {
  value: SkillFrontmatter;
  onChange: (field: keyof SkillFrontmatter, value: string) => void;
}

function FrontmatterForm({ value, onChange }: FrontmatterFormProps) {
  return (
    <div
      className="rounded-xl border p-4 grid gap-3 md:grid-cols-2"
      style={{ borderColor: 'var(--pc-border)', background: 'var(--pc-bg-surface)' }}
    >
      <Field label="Name (required)" value={value.name} onChange={(v) => onChange('name', v)} />
      <Field
        label="Version"
        value={value.version ?? ''}
        onChange={(v) => onChange('version', v)}
        placeholder="0.1.0"
      />
      <div className="md:col-span-2 flex flex-col gap-1">
        <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
          Description (required) — what it does and when to use it
        </label>
        <textarea
          value={value.description}
          onChange={(e) => onChange('description', e.target.value)}
          rows={2}
          className="rounded-md border bg-transparent px-3 py-1.5 text-sm font-mono"
          style={{ borderColor: 'var(--pc-border)' }}
        />
      </div>
      <Field
        label="License (SPDX)"
        value={value.license ?? ''}
        onChange={(v) => onChange('license', v)}
        placeholder="MIT"
      />
      <Field
        label="Author"
        value={value.author ?? ''}
        onChange={(v) => onChange('author', v)}
      />
      <Field
        label="Category"
        value={value.category ?? ''}
        onChange={(v) => onChange('category', v)}
        placeholder="coding, ops, …"
      />
    </div>
  );
}

interface FieldProps {
  label: string;
  value: string;
  onChange: (value: string) => void;
  placeholder?: string;
}

function Field({ label, value, onChange, placeholder }: FieldProps) {
  return (
    <div className="flex flex-col gap-1">
      <label className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
        {label}
      </label>
      <input
        type="text"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        className="rounded-md border bg-transparent px-3 py-1.5 text-sm"
        style={{ borderColor: 'var(--pc-border)' }}
      />
    </div>
  );
}
