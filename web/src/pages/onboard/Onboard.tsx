// Schema-driven web onboarding flow (issue #6175).
//
// Walks the gateway's per-property CRUD surface to render a section-by-section
// form whose fields come from `GET /api/config/list`. Section order and
// field grouping are derived from the schema's `onboard_section` annotation
// — no TypeScript hardcoding of section names. Save submits a single
// `PATCH /api/config` per section with all the form's answers.
//
// Designed as the minimal, schema-faithful counterpart to `zeroclaw onboard`.
// Field-level error rendering binds to the structured `ConfigApiError` codes.

import { useEffect, useMemo, useState } from 'react';
import {
  listProps,
  patchConfig,
  type ConfigApiError,
  type ListResponseEntry,
  type PatchOp,
} from '../../lib/api';

type SectionGroup = {
  name: string;
  entries: ListResponseEntry[];
};

function groupBySection(entries: ListResponseEntry[]): SectionGroup[] {
  const groups = new Map<string, ListResponseEntry[]>();
  for (const e of entries) {
    if (!e.onboard_section) continue;
    const arr = groups.get(e.onboard_section) ?? [];
    arr.push(e);
    groups.set(e.onboard_section, arr);
  }
  return Array.from(groups.entries()).map(([name, entries]) => ({ name, entries }));
}

function inferInputType(entry: ListResponseEntry): 'bool' | 'array' | 'secret' | 'text' {
  if (entry.is_secret) return 'secret';
  const v = entry.value;
  if (typeof v === 'boolean') return 'bool';
  if (Array.isArray(v)) return 'array';
  if (typeof v === 'string' && (v === 'true' || v === 'false')) return 'bool';
  if (typeof v === 'string' && (v.startsWith('[') || v.startsWith('{'))) return 'array';
  return 'text';
}

function fieldLabel(entry: ListResponseEntry): string {
  // Trim the section prefix, swap dashes/dots → spaces, title-case.
  const tail = entry.path.split('.').slice(-2).join(' ');
  return tail.replace(/[-_]/g, ' ');
}

function parseInput(entry: ListResponseEntry, raw: string): unknown {
  switch (inferInputType(entry)) {
    case 'bool':
      return raw === 'true';
    case 'array':
      // Newline-delimited input → JSON array.
      return raw
        .split('\n')
        .map((s) => s.trim())
        .filter(Boolean);
    default:
      return raw;
  }
}

function defaultInputValue(entry: ListResponseEntry): string {
  const v = entry.value;
  if (typeof v === 'string') return v === '<unset>' ? '' : v;
  if (typeof v === 'boolean') return v ? 'true' : 'false';
  if (Array.isArray(v)) return v.join('\n');
  return '';
}

export default function Onboard() {
  const [sections, setSections] = useState<SectionGroup[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [activeIdx, setActiveIdx] = useState(0);
  const [draft, setDraft] = useState<Record<string, string>>({});
  const [comments, setComments] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [fieldErrors, setFieldErrors] = useState<Record<string, ConfigApiError>>({});
  const [completed, setCompleted] = useState<Set<string>>(new Set());

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    listProps()
      .then((resp) => {
        if (cancelled) return;
        const grouped = groupBySection(resp.entries);
        setSections(grouped);
        // Seed draft + comments with current display values.
        const seed: Record<string, string> = {};
        for (const g of grouped) {
          for (const f of g.entries) {
            seed[f.path] = defaultInputValue(f);
          }
        }
        setDraft(seed);
      })
      .catch((e) => !cancelled && setError(String(e)))
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, []);

  const active = sections[activeIdx];

  const handleSave = async () => {
    if (!active) return;
    setSaving(true);
    setFieldErrors({});

    const ops: PatchOp[] = [];
    for (const f of active.entries) {
      const raw = draft[f.path] ?? '';
      // For secret fields, only include when the user actually typed something.
      // Empty-string for a secret means "leave it alone."
      if (f.is_secret && raw.length === 0) continue;
      const value = parseInput(f, raw);
      const op: PatchOp = { op: 'replace', path: f.path, value };
      const c = comments[f.path];
      if (c && c.length > 0) op.comment = c;
      ops.push(op);
    }

    try {
      await patchConfig(ops);
      setCompleted((prev) => new Set(prev).add(active.name));
      // Move to the next incomplete section.
      const next = sections.findIndex((s, i) => i > activeIdx && !completed.has(s.name));
      if (next >= 0) setActiveIdx(next);
    } catch (e) {
      // Try to parse a structured error envelope.
      let parsed: ConfigApiError | null = null;
      const msg = String(e instanceof Error ? e.message : e);
      try {
        const m = msg.match(/API \d+:\s*(\{.*\})$/);
        if (m && m[1]) parsed = JSON.parse(m[1]) as ConfigApiError;
      } catch {
        /* ignore */
      }
      if (parsed && parsed.path) {
        setFieldErrors({ [parsed.path]: parsed });
      } else {
        setError(msg);
      }
    } finally {
      setSaving(false);
    }
  };

  const sectionStatus = useMemo(() => {
    return sections.map((s) => ({
      name: s.name,
      done: completed.has(s.name),
    }));
  }, [sections, completed]);

  if (loading) return <div className="p-6">Loading schema…</div>;
  if (error)
    return (
      <div className="p-6 text-red-600">
        <p>Failed to load onboarding schema:</p>
        <pre>{error}</pre>
      </div>
    );
  if (sections.length === 0)
    return (
      <div className="p-6">
        No onboard-tagged sections in the schema. Confirm `Section::from_path`
        recognizes your config layout.
      </div>
    );

  return (
    <div className="flex gap-6 p-6">
      <aside className="w-48 flex-shrink-0 border-r pr-4">
        <h2 className="mb-4 text-sm font-semibold uppercase text-gray-500">Sections</h2>
        <ul className="space-y-1">
          {sectionStatus.map((s, i) => (
            <li key={s.name}>
              <button
                onClick={() => setActiveIdx(i)}
                className={`w-full rounded px-2 py-1 text-left text-sm ${
                  i === activeIdx ? 'bg-blue-100 font-medium' : 'hover:bg-gray-100'
                } ${s.done ? 'text-gray-500' : ''}`}
              >
                {s.done ? '✓ ' : ''}
                {s.name}
              </button>
            </li>
          ))}
        </ul>
      </aside>

      <main className="flex-1">
        {active && (
          <>
            <h1 className="mb-2 text-2xl font-semibold capitalize">{active.name}</h1>
            <p className="mb-6 text-sm text-gray-600">
              Each field below is driven by the gateway's schema (
              <code>GET /api/config/list</code>). Changes save atomically via{' '}
              <code>PATCH /api/config</code>.
            </p>

            <form
              className="space-y-4"
              onSubmit={(e) => {
                e.preventDefault();
                handleSave();
              }}
            >
              {active.entries.map((f) => (
                <div key={f.path}>
                  <label className="block text-sm font-medium" htmlFor={f.path}>
                    {fieldLabel(f)}{' '}
                    {f.is_secret && (
                      <span className="text-xs text-gray-500">
                        🔒 secret {f.populated ? '(set)' : '(unset)'}
                      </span>
                    )}
                  </label>
                  <code className="block text-xs text-gray-400">{f.path}</code>

                  {inferInputType(f) === 'bool' ? (
                    <select
                      id={f.path}
                      value={draft[f.path] ?? 'false'}
                      onChange={(e) =>
                        setDraft((d) => ({ ...d, [f.path]: e.target.value }))
                      }
                      className="mt-1 w-full rounded border px-2 py-1"
                    >
                      <option value="true">true</option>
                      <option value="false">false</option>
                    </select>
                  ) : inferInputType(f) === 'array' ? (
                    <textarea
                      id={f.path}
                      rows={4}
                      value={draft[f.path] ?? ''}
                      onChange={(e) =>
                        setDraft((d) => ({ ...d, [f.path]: e.target.value }))
                      }
                      className="mt-1 w-full rounded border px-2 py-1 font-mono text-sm"
                      placeholder="One value per line"
                    />
                  ) : (
                    <input
                      id={f.path}
                      type={f.is_secret ? 'password' : 'text'}
                      value={draft[f.path] ?? ''}
                      onChange={(e) =>
                        setDraft((d) => ({ ...d, [f.path]: e.target.value }))
                      }
                      className="mt-1 w-full rounded border px-2 py-1"
                      placeholder={
                        f.is_secret
                          ? f.populated
                            ? 'Leave blank to keep current value'
                            : 'Enter secret value'
                          : ''
                      }
                    />
                  )}

                  <input
                    type="text"
                    value={comments[f.path] ?? ''}
                    onChange={(e) =>
                      setComments((c) => ({ ...c, [f.path]: e.target.value }))
                    }
                    placeholder="Optional comment (why?)"
                    className="mt-1 w-full rounded border px-2 py-1 text-xs text-gray-600"
                  />

                  {(() => {
                    const fe = fieldErrors[f.path];
                    if (!fe) return null;
                    return (
                      <p className="mt-1 text-sm text-red-600">
                        <span className="font-mono">{fe.code}</span>: {fe.message}
                      </p>
                    );
                  })()}
                </div>
              ))}

              <div className="flex items-center gap-3 pt-4">
                <button
                  type="submit"
                  disabled={saving}
                  className="rounded bg-blue-600 px-4 py-2 text-white disabled:opacity-50"
                >
                  {saving ? 'Saving…' : 'Save and continue'}
                </button>
                {activeIdx > 0 && (
                  <button
                    type="button"
                    onClick={() => setActiveIdx(activeIdx - 1)}
                    className="rounded border px-4 py-2"
                  >
                    Back
                  </button>
                )}
              </div>
            </form>
          </>
        )}
      </main>
    </div>
  );
}
