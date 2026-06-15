// Schema-driven editor for `[cost.rates.providers.<category>.<type>.<resource>]`.
// Reuses the same FieldForm machinery that powers /config so the
// rendered inputs always match the Configurable derive (input_per_mtok,
// output_per_mtok, cached_input_per_mtok for models; per_mchar for tts;
// per_minute for transcription). Adding a new rate field anywhere in the
// schema makes it appear here automatically.
//
// Two consumers share this widget:
//   - The "Costs" tab on `/config/providers.<category>/<type>/<alias>`. The
//     parent passes the bound upstream id (resolved from the model/voice
//     field on the alias's own config) as `fixedResource`.
//   - The "Rates" tab on `/config/cost`. No fixed resource: the widget shows
//     all configured rates and lets the operator add new ones.
//
// Per the alias mandate: this widget always renders the composite
// `<type>.<resource>` in headings, error text, and the PATCH path used by
// FieldForm — never the bare resource.

import { useEffect, useState } from 'react';
import { ChevronRight, Plus, Trash2 } from 'lucide-react';
import {
  ApiError,
  createMapKey,
  deleteMapKey,
  getMapKeys,
  getCatalogModels,
  patchConfig,
  type ModelPricing,
} from '../../lib/api';
import { configuredResourceIds } from '../../lib/configuredModels';
import FieldForm from './FieldForm';

export type CostRatesCategory = 'models' | 'tts' | 'transcription';

interface CostRatesEditorProps {
  /** Which `[cost.rates.providers.<category>]` subtree to edit. */
  category: CostRatesCategory;
  /** Provider type slot (e.g. "anthropic", "openai"). Required for embedded
   *  use; the standalone view picks one internally before mounting this. */
  providerType: string;
  /** When set, edit exactly that resource and skip the alias-list step.
   *  Used by the providers.<category>.<alias> Costs tab, which resolves the
   *  bound upstream id and passes it through. */
  fixedResource?: string;
  /** Called after each successful PATCH so the parent can refresh drift. */
  onSaved?: () => void;
}

export default function CostRatesEditor(props: CostRatesEditorProps) {
  if (props.fixedResource) {
    return <SingleResourceEditor {...props} fixedResource={props.fixedResource} />;
  }
  return <ResourceListEditor {...props} />;
}

// Composite alias-bound label used in every UI string. Mandated by the
// alias rules: a rate row's identity is `<type>.<resource>`, never just
// `<resource>`. Keeps the rates view consistent with banners, logs, and
// the [providers.<type>.<alias>] convention next door.
function composite(category: CostRatesCategory, providerType: string, resource: string) {
  return `${category}.${providerType}.${resource}`;
}

function basePathFor(category: CostRatesCategory, providerType: string) {
  return `cost.rates.providers.${category}.${providerType}`;
}

/** Apply catalog pricing to a resource's rate entry. Fetches from the API
 *  if the in-memory cache is empty, then builds and submits PATCH ops.
 *  Silent-fail on errors — pricing pre-fill is a nice-to-have. */
async function applyCatalogPricingToResource(
  basePath: string,
  resource: string,
  catalogPricing: Record<string, ModelPricing> | null,
  providerType: string,
): Promise<void> {
  let pricing: ModelPricing | undefined = catalogPricing
    ? catalogPricing[resource]
    : undefined;
  if (!pricing) {
    try {
      const resp = await getCatalogModels(providerType);
      pricing = resp.pricing?.[resource];
    } catch { /* silent fail */ }
  }
  if (!pricing) return;
  const fullPath = `${basePath}.${resource}`;
  const ops: { op: 'replace'; path: string; value: number }[] = [];
  if (pricing.prompt !== undefined) {
    const v = parseFloat(pricing.prompt);
    if (!isNaN(v)) ops.push({ op: 'replace', path: `${fullPath}.input_per_mtok`, value: v * 1_000_000 });
  }
  if (pricing.completion !== undefined) {
    const v = parseFloat(pricing.completion);
    if (!isNaN(v)) ops.push({ op: 'replace', path: `${fullPath}.output_per_mtok`, value: v * 1_000_000 });
  }
  if (pricing.input_cache_read !== undefined) {
    const v = parseFloat(pricing.input_cache_read);
    if (!isNaN(v)) ops.push({ op: 'replace', path: `${fullPath}.cached_input_per_mtok`, value: v * 1_000_000 });
  }
  if (ops.length > 0) {
    await patchConfig(ops);
  }
}

// Embedded mode — providers.<category>.<alias> "Costs" tab. The resource
// id (e.g. `claude-opus-4-7`) is fixed by the bound model/voice on the
// alias; on first visit there's no rate entry yet, so the widget shows a
// one-click "Add rates" affordance that calls createMapKey before
// handing off to FieldForm.
function SingleResourceEditor({
  category,
  providerType,
  fixedResource,
  onSaved,
}: CostRatesEditorProps & { fixedResource: string }) {
  const basePath = basePathFor(category, providerType);
  const fullPath = `${basePath}.${fixedResource}`;
  const [exists, setExists] = useState<boolean | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  const [catalogPricing, setCatalogPricing] = useState<Record<string, ModelPricing> | null>(null);

  useEffect(() => {
    let cancelled = false;
    setExists(null);
    setError(null);
    getMapKeys(basePath)
      .then((r) => {
        if (cancelled) return;
        setExists(r.keys.includes(fixedResource));
      })
      .catch((e) => {
        if (cancelled) return;
        setExists(false);
        setError(e instanceof ApiError ? e.envelope.message : String(e));
      });
    // Fetch catalog pricing in parallel — silent fail, it's a nice-to-have.
    getCatalogModels(providerType)
      .then((r) => {
        if (!cancelled && r.pricing) setCatalogPricing(r.pricing);
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [basePath, fixedResource, providerType]);

  const addRates = async () => {
    setBusy(true);
    setError(null);
    try {
      await createMapKey(basePath, fixedResource);
      // Pre-fill rates from catalog pricing.
      await applyCatalogPricingToResource(basePath, fixedResource, catalogPricing, providerType);
      setExists(true);
      setReloadKey((n) => n + 1);
      onSaved?.();
    } catch (e) {
      setError(e instanceof ApiError ? e.envelope.message : (e as Error).message);
    } finally {
      setBusy(false);
    }
  };

  if (exists === null) {
    return <InlineSpinner />;
  }

  if (!exists) {
    return (
      <div className="flex flex-col gap-3">
        <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
          No rate sheet entry yet for{' '}
          <code className="font-mono">{composite(category, providerType, fixedResource)}</code>.
          Adding one lets the orchestrator price token usage at this rate
          instead of falling back to <code>cost.usd_per_1k_input</code>.
        </p>
        {error && <ErrorBanner msg={error} />}
        <button
          type="button"
          onClick={addRates}
          disabled={busy}
          className="btn-electric flex items-center gap-2 text-sm px-3 py-2 self-start"
        >
          <Plus className="h-4 w-4" />
          {busy ? 'Adding…' : 'Add rates'}
        </button>
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
        Rate sheet for{' '}
        <code className="font-mono">{composite(category, providerType, fixedResource)}</code>{' '}
        — path <code className="font-mono">{fullPath}</code>.
      </p>
      {error && <ErrorBanner msg={error} />}
      <FieldForm
        key={`${reloadKey}-${fullPath}`}
        prefix={fullPath}
        onSaved={onSaved}
        showDelete={false}
        inlineSaveBar
      />
    </div>
  );
}

// Standalone mode — Rates tab on /config/cost. Lets the operator list
// every resource currently priced at (category, providerType) and add /
// remove / open each one. Clicking a row inlines the FieldForm under it
// so the editor stays on a single page (no nested routes).
function ResourceListEditor({
  category,
  providerType,
  onSaved,
}: CostRatesEditorProps) {
  const basePath = basePathFor(category, providerType);
  // Suggestion list comes from the matching `[providers.<category>.<type>.<alias>].model`
  // values via the `configuredModels` helper. Schema-derived; no
  // hand-typed entries.
  const [resources, setResources] = useState<string[]>([]);
  const [suggestions, setSuggestions] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [open, setOpen] = useState<string | null>(null);
  const [newResource, setNewResource] = useState('');
  const [adding, setAdding] = useState(false);
  const [reloadKey, setReloadKey] = useState(0);
  const [catalogPricing, setCatalogPricing] = useState<Record<string, ModelPricing> | null>(null);

  const reload = async () => {
    setLoading(true);
    try {
      const [keys, sugs] = await Promise.all([
        getMapKeys(basePath).then((r) => r.keys),
        configuredResourceIds(category, providerType),
      ]);
      setResources(keys);
      setSuggestions(sugs);
    } catch (e) {
      setError(e instanceof ApiError ? e.envelope.message : (e as Error).message);
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
    // Fetch catalog pricing for pre-fill — silent fail, nice-to-have.
    getCatalogModels(providerType)
      .then((r) => { if (r.pricing) setCatalogPricing(r.pricing); })
      .catch(() => {});
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [basePath]);

  /** Apply catalog pricing to a newly-created resource, if available. */
  const applyPricing = async (resource: string) => {
    await applyCatalogPricingToResource(basePath, resource, catalogPricing, providerType);
  };

  const addResource = async () => {
    const trimmed = newResource.trim();
    if (!trimmed) return;
    setAdding(true);
    setError(null);
    try {
      await createMapKey(basePath, trimmed);
      // Pre-fill rates from catalog pricing before reload.
      await applyPricing(trimmed);
      setNewResource('');
      setOpen(trimmed);
      await reload();
      onSaved?.();
    } catch (e) {
      setError(e instanceof ApiError ? e.envelope.message : (e as Error).message);
    } finally {
      setAdding(false);
    }
  };

  const removeResource = async (resource: string) => {
    setError(null);
    try {
      await deleteMapKey(basePath, resource);
      if (open === resource) setOpen(null);
      await reload();
      onSaved?.();
    } catch (e) {
      setError(e instanceof ApiError ? e.envelope.message : (e as Error).message);
    }
  };

  if (loading) return <InlineSpinner />;

  return (
    <div className="flex flex-col gap-3">
      {error && <ErrorBanner msg={error} />}

      <div
        className="surface-panel divide-y"
        style={{ borderColor: 'var(--pc-border)' }}
      >
        {resources.length === 0 ? (
          <div
            className="p-4 text-sm text-center"
            style={{ color: 'var(--pc-text-muted)' }}
          >
            No rates configured under{' '}
            <code className="font-mono">{basePath}</code>. Add one below.
          </div>
        ) : (
          resources.map((resource) => (
            <ResourceRow
              key={resource}
              category={category}
              providerType={providerType}
              resource={resource}
              basePath={basePath}
              isOpen={open === resource}
              onToggle={() => setOpen(open === resource ? null : resource)}
              onRemove={() => removeResource(resource)}
              onSaved={() => {
                setReloadKey((n) => n + 1);
                onSaved?.();
              }}
              reloadKey={reloadKey}
            />
          ))
        )}

        <div className="flex flex-col gap-2 px-4 py-3">
          <div className="flex items-center gap-2">
            <input
              type="text"
              list={`cost-rates-suggest-${category}-${providerType}`}
              value={newResource}
              onChange={(e) => setNewResource(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === 'Enter') void addResource();
              }}
              placeholder={suggestions[0] ?? ''}
              className="input-electric flex-1 px-3 py-1.5 text-sm font-mono"
            />
            <datalist id={`cost-rates-suggest-${category}-${providerType}`}>
              {suggestions
                .filter((s) => !resources.includes(s))
                .map((s) => (
                  <option key={s} value={s} />
                ))}
            </datalist>
            <button
              type="button"
              onClick={() => void addResource()}
              disabled={adding || !newResource.trim()}
              className="btn-electric text-sm px-3 py-1.5 flex items-center gap-1"
            >
              <Plus className="h-4 w-4" />
              {adding ? 'Adding…' : 'Add'}
            </button>
          </div>
          {suggestions.filter((s) => !resources.includes(s)).length > 0 && (
            <div className="flex items-center gap-1.5 flex-wrap">
              <span className="text-xs" style={{ color: 'var(--pc-text-muted)' }}>
                From <code className="font-mono">providers.{category}.{providerType}</code>:
              </span>
              {suggestions
                .filter((s) => !resources.includes(s))
                .map((s) => (
                  <button
                    key={s}
                    type="button"
                    onClick={() => setNewResource(s)}
                    className="text-xs font-mono px-2 py-0.5 rounded-md transition-opacity hover:opacity-80"
                    style={{
                      background: 'var(--pc-bg-elevated)',
                      color: 'var(--pc-text-secondary)',
                      border: '1px solid var(--pc-border)',
                    }}
                  >
                    {s}
                  </button>
                ))}
            </div>
          )}
        </div>
      </div>

      <p
        className="text-xs"
        style={{ color: 'var(--pc-text-faint)' }}
      >
        Resource id is the upstream model / voice / pipeline name as it
        appears in usage telemetry, not the local alias. Rates emit at{' '}
        <code className="font-mono">{basePath}.&lt;resource&gt;</code>.
      </p>
    </div>
  );
}

function ResourceRow({
  category,
  providerType,
  resource,
  basePath,
  isOpen,
  onToggle,
  onRemove,
  onSaved,
  reloadKey,
}: {
  category: CostRatesCategory;
  providerType: string;
  resource: string;
  basePath: string;
  isOpen: boolean;
  onToggle: () => void;
  onRemove: () => void;
  onSaved: () => void;
  reloadKey: number;
}) {
  const fullPath = `${basePath}.${resource}`;
  const [armed, setArmed] = useState(false);
  useEffect(() => {
    if (!armed) return;
    const t = setTimeout(() => setArmed(false), 3000);
    return () => clearTimeout(t);
  }, [armed]);

  return (
    <div className="flex flex-col">
      <div className="flex items-center justify-between gap-3 px-4 py-3 text-sm">
        <button
          type="button"
          onClick={onToggle}
          className="flex-1 min-w-0 flex items-center justify-between gap-3 text-left"
        >
          <div className="min-w-0">
            <span
              className="font-mono"
              style={{ color: 'var(--pc-text-primary)', fontWeight: 500 }}
            >
              {composite(category, providerType, resource)}
            </span>
            <code
              className="block text-xs mt-0.5"
              style={{ color: 'var(--pc-text-faint)' }}
            >
              {fullPath}
            </code>
          </div>
          <ChevronRight
            className="h-4 w-4 flex-shrink-0 transition-transform"
            style={{
              color: 'var(--pc-text-muted)',
              transform: isOpen ? 'rotate(90deg)' : 'none',
            }}
          />
        </button>
        <button
          type="button"
          onClick={(e) => {
            e.stopPropagation();
            if (!armed) {
              setArmed(true);
              return;
            }
            onRemove();
            setArmed(false);
          }}
          title={
            armed
              ? `Click again to delete ${composite(category, providerType, resource)}`
              : `Delete ${composite(category, providerType, resource)}`
          }
          className="btn-icon flex-shrink-0"
          style={
            armed
              ? {
                  color: 'var(--color-status-error, #f87171)',
                  borderColor: 'var(--color-status-error, #f87171)',
                }
              : undefined
          }
        >
          {armed ? (
            <span className="text-xs px-1">Confirm</span>
          ) : (
            <Trash2 className="h-4 w-4" />
          )}
        </button>
      </div>
      {isOpen && (
        <div
          className="px-4 pb-3"
          style={{ borderTop: '1px solid var(--pc-border)' }}
        >
          <FieldForm
            key={`${reloadKey}-${fullPath}`}
            prefix={fullPath}
            onSaved={onSaved}
            showDelete={false}
          />
        </div>
      )}
    </div>
  );
}

function InlineSpinner() {
  return (
    <div className="flex items-center justify-center py-8">
      <div
        className="h-6 w-6 border-2 rounded-full animate-spin"
        style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
      />
    </div>
  );
}

function ErrorBanner({ msg }: { msg: string }) {
  return (
    <div
      className="rounded-xl border p-3 text-sm"
      style={{
        background: 'var(--color-status-error-alpha-08, rgba(239,68,68,0.08))',
        borderColor: 'var(--color-status-error-alpha-20, rgba(239,68,68,0.2))',
        color: 'var(--color-status-error, #f87171)',
      }}
    >
      {msg}
    </div>
  );
}
