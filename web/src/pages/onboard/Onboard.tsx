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
  ApiError,
  createMapKey,
  getCatalog,
  getCatalogModels,
  getProp,
  listProps,
  patchConfig,
  type CatalogProvider,
  type ConfigApiError,
  type ListResponseEntry,
  type PatchOp,
} from '../../lib/api';

/** Schema path for the onboard-state completion list — single source of truth. */
const COMPLETED_SECTIONS_PATH = 'onboard-state.completed-sections';

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

/**
 * Map the gateway's wire-form kind to a renderer key. Single source of truth
 * — secrets are always 'secret' regardless of underlying kind, then dispatch
 * by the declared `kind` (no value-sniffing). 'enum' falls back to 'select'
 * when variants are present, otherwise 'text'.
 */
function rendererFor(entry: ListResponseEntry): 'bool' | 'array' | 'secret' | 'select' | 'number' | 'text' {
  if (entry.is_secret) return 'secret';
  switch (entry.kind) {
    case 'bool':
      return 'bool';
    case 'string-array':
      return 'array';
    case 'integer':
    case 'float':
      return 'number';
    case 'enum':
      return entry.enum_variants && entry.enum_variants.length > 0 ? 'select' : 'text';
    default:
      return 'text';
  }
}

function fieldLabel(entry: ListResponseEntry): string {
  // Trim the section prefix, swap dashes/dots → spaces, title-case.
  const tail = entry.path.split('.').slice(-2).join(' ');
  return tail.replace(/[-_]/g, ' ');
}

function parseInput(entry: ListResponseEntry, raw: string): unknown {
  switch (rendererFor(entry)) {
    case 'bool':
      return raw === 'true';
    case 'array':
      // Newline-delimited input → JSON array.
      return raw
        .split('\n')
        .map((s) => s.trim())
        .filter(Boolean);
    case 'number': {
      const n = Number(raw);
      return Number.isNaN(n) ? raw : n;
    }
    default:
      return raw;
  }
}

function defaultInputValue(entry: ListResponseEntry): string {
  // Values from /api/config/list come stringified per the current gateway
  // contract — show them as-is, except for the <unset> sentinel.
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
  const [catalog, setCatalog] = useState<CatalogProvider[]>([]);
  const [providerToAdd, setProviderToAdd] = useState('');
  const [adding, setAdding] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    // Fetch schema + completion state + provider catalog in parallel.
    // Catalog backs the "+ Add provider" picker — same source as the CLI
    // wizard (zeroclaw_providers::list_providers via /api/onboard/catalog).
    Promise.all([
      listProps(),
      getProp(COMPLETED_SECTIONS_PATH).catch(() => ({ value: '[]' })),
      getCatalog().catch(() => ({ providers: [] })),
    ])
      .then(([resp, completedResp, catalogResp]) => {
        if (cancelled) return;
        setCatalog(catalogResp.providers);
        const grouped = groupBySection(resp.entries);
        setSections(grouped);
        // Seed draft with current display values from the schema.
        const seed: Record<string, string> = {};
        for (const g of grouped) {
          for (const f of g.entries) {
            seed[f.path] = defaultInputValue(f);
          }
        }
        setDraft(seed);
        // Hydrate completion state from the gateway. The value comes back as
        // a stringified JSON array (per the current /list display contract);
        // tolerate both array and string forms.
        const v = completedResp.value;
        let names: string[] = [];
        if (Array.isArray(v)) {
          names = v.filter((x): x is string => typeof x === 'string');
        } else if (typeof v === 'string' && v.length > 0 && v !== '<unset>') {
          try {
            const parsed = JSON.parse(v);
            if (Array.isArray(parsed)) {
              names = parsed.filter((x): x is string => typeof x === 'string');
            }
          } catch {
            // Single-name fallback (CLI display sometimes emits comma-separated).
            names = v.split(',').map((s) => s.trim()).filter(Boolean);
          }
        }
        setCompleted(new Set(names));
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

      // Persist completion to the gateway so re-opening the dashboard
      // resumes where the user left off. Single source of truth: the
      // gateway's onboard_state.completed_sections list, same place
      // `zeroclaw onboard` already writes.
      const newCompleted = new Set(completed).add(active.name);
      const completedArr = Array.from(newCompleted);
      try {
        await patchConfig([
          {
            op: 'replace',
            path: COMPLETED_SECTIONS_PATH,
            value: completedArr,
          },
        ]);
      } catch (persistErr) {
        // Don't fail the whole save flow on a completion-marker write
        // failure — the user's section data is already persisted. Surface
        // the issue so the dashboard log catches it but proceed.
        // eslint-disable-next-line no-console
        console.warn('Failed to persist onboard completion marker:', persistErr);
      }
      setCompleted(newCompleted);

      // Move to the next incomplete section.
      const next = sections.findIndex((s, i) => i > activeIdx && !newCompleted.has(s.name));
      if (next >= 0) setActiveIdx(next);
    } catch (e) {
      // Structured ApiError thrown by apiFetch carries the parsed envelope
      // directly — no regex over the message string. If the error is bound
      // to a path, surface it inline next to that field; otherwise show
      // a top-level error.
      if (e instanceof ApiError) {
        const env = e.envelope as ConfigApiError;
        if (env.path) {
          setFieldErrors({ [env.path]: env });
        } else {
          setError(`[${env.code}] ${env.message}`);
        }
      } else {
        setError(String(e instanceof Error ? e.message : e));
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

            {/* "+ Add provider" picker on the providers section. Source is
                the same /api/onboard/catalog the CLI wizard uses. Selecting
                a provider POSTs /api/config/map-key, which instantiates a
                default-valued [providers.models.<name>] block, then re-loads
                so the new section's fields appear immediately. */}
            {active.name === 'providers' && (
              <div className="mb-6 rounded border border-gray-200 bg-gray-50 p-3">
                <div className="mb-2 text-sm font-semibold text-gray-700">Add a provider</div>
                <div className="flex items-center gap-2">
                  <select
                    value={providerToAdd}
                    onChange={(e) => setProviderToAdd(e.target.value)}
                    disabled={adding}
                    className="rounded border px-2 py-1 text-sm"
                  >
                    <option value="">— pick a provider —</option>
                    {catalog.map((p) => (
                      <option key={p.name} value={p.name}>
                        {p.display_name}
                        {p.local ? ' (local)' : ''}
                      </option>
                    ))}
                  </select>
                  <button
                    type="button"
                    disabled={!providerToAdd || adding}
                    onClick={async () => {
                      setAdding(true);
                      try {
                        await createMapKey('providers.models', providerToAdd);
                        // Lazy-fetch model catalog for the chosen provider so
                        // the model field below gets a populated dropdown.
                        await getCatalogModels(providerToAdd).catch(() => null);
                        // Reload section list — the new fields appear.
                        const refreshed = await listProps();
                        const grouped = groupBySection(refreshed.entries);
                        setSections(grouped);
                        const seed: Record<string, string> = { ...draft };
                        for (const g of grouped) {
                          for (const f of g.entries) {
                            if (!(f.path in seed)) seed[f.path] = defaultInputValue(f);
                          }
                        }
                        setDraft(seed);
                        setProviderToAdd('');
                      } catch (e) {
                        if (e instanceof ApiError) {
                          const env = e.envelope as ConfigApiError;
                          setError(`[${env.code}] ${env.message}`);
                        } else {
                          setError(String(e instanceof Error ? e.message : e));
                        }
                      } finally {
                        setAdding(false);
                      }
                    }}
                    className="rounded bg-blue-600 px-3 py-1 text-sm text-white disabled:opacity-50"
                  >
                    {adding ? 'Adding…' : 'Add'}
                  </button>
                </div>
                <p className="mt-2 text-xs text-gray-500">
                  Provider list comes from the gateway's onboard catalog (same source as
                  the CLI wizard). Pick one to instantiate{' '}
                  <code>[providers.models.&lt;name&gt;]</code> with defaults; the new
                  fields (api-key, model, etc.) appear in the form below.
                </p>
              </div>
            )}

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

                  {(() => {
                    const renderer = rendererFor(f);
                    if (renderer === 'bool') {
                      return (
                        <select
                          id={f.path}
                          value={draft[f.path] ?? 'false'}
                          onChange={(e) => setDraft((d) => ({ ...d, [f.path]: e.target.value }))}
                          className="mt-1 w-full rounded border px-2 py-1"
                        >
                          <option value="true">true</option>
                          <option value="false">false</option>
                        </select>
                      );
                    }
                    if (renderer === 'select') {
                      return (
                        <select
                          id={f.path}
                          value={draft[f.path] ?? ''}
                          onChange={(e) => setDraft((d) => ({ ...d, [f.path]: e.target.value }))}
                          className="mt-1 w-full rounded border px-2 py-1"
                        >
                          {(f.enum_variants ?? []).map((v) => (
                            <option key={v} value={v}>
                              {v}
                            </option>
                          ))}
                        </select>
                      );
                    }
                    if (renderer === 'array') {
                      return (
                        <textarea
                          id={f.path}
                          rows={4}
                          value={draft[f.path] ?? ''}
                          onChange={(e) => setDraft((d) => ({ ...d, [f.path]: e.target.value }))}
                          className="mt-1 w-full rounded border px-2 py-1 font-mono text-sm"
                          placeholder="One value per line"
                        />
                      );
                    }
                    if (renderer === 'number') {
                      return (
                        <input
                          id={f.path}
                          type="number"
                          value={draft[f.path] ?? ''}
                          onChange={(e) => setDraft((d) => ({ ...d, [f.path]: e.target.value }))}
                          className="mt-1 w-full rounded border px-2 py-1"
                        />
                      );
                    }
                    return (
                      <input
                        id={f.path}
                        type={renderer === 'secret' ? 'password' : 'text'}
                        value={draft[f.path] ?? ''}
                        onChange={(e) => setDraft((d) => ({ ...d, [f.path]: e.target.value }))}
                        className="mt-1 w-full rounded border px-2 py-1"
                        placeholder={
                          renderer === 'secret'
                            ? f.populated
                              ? 'Leave blank to keep current value'
                              : 'Enter secret value'
                            : f.type_hint
                        }
                      />
                    );
                  })()}

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
