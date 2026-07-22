/**
 * ModelPicker — full-catalog model selector for the Brain step.
 *
 * Renders the daemon's live model catalog (e.g. all ~330 OpenRouter models)
 * as a searchable, scrollable list grouped by vendor prefix, while still
 * allowing any free-text model id. Falls back to a plain input when no
 * catalog is available.
 */
import { useMemo, useState } from 'react';
import { TextInput } from './ui';

interface Props {
  models: string[];
  /** true when the list came from a live source (not a static stub) */
  live: boolean;
  value: string;
  onChange: (model: string) => void;
}

const ACCENT = '#D97757';
const IVORY = '#FAF9F5';

function groupByVendor(models: string[]): Array<[string, string[]]> {
  const groups = new Map<string, string[]>();
  for (const m of models) {
    const slash = m.indexOf('/');
    const vendor = slash > 0 ? m.slice(0, slash) : 'other';
    const list = groups.get(vendor);
    if (list) list.push(m);
    else groups.set(vendor, [m]);
  }
  return [...groups.entries()].sort((a, b) => a[0].localeCompare(b[0]));
}

export default function ModelPicker({ models, live, value, onChange }: Props) {
  const [filter, setFilter] = useState('');

  const filtered = useMemo(() => {
    const q = filter.trim().toLowerCase();
    if (!q) return models;
    return models.filter((m) => m.toLowerCase().includes(q));
  }, [models, filter]);

  const grouped = useMemo(() => groupByVendor(filtered), [filtered]);
  const hasVendors = useMemo(() => models.some((m) => m.includes('/')), [models]);

  if (models.length === 0) {
    return (
      <TextInput
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder="model id"
      />
    );
  }

  return (
    <div className="space-y-2">
      <TextInput
        value={value}
        onChange={(e) => {
          onChange(e.target.value);
          setFilter(e.target.value);
        }}
        placeholder="Search or type any model id…"
      />
      <div
        className="rounded-lg overflow-y-auto"
        style={{ maxHeight: 240, background: '#0d0d0d', border: '1px solid #222' }}
      >
        {grouped.length === 0 && (
          <div className="px-3 py-3 text-[12px]" style={{ color: '#666' }}>
            No catalog match — the id above will be used as-is.
          </div>
        )}
        {grouped.map(([vendor, list]) => (
          <div key={vendor}>
            {hasVendors && (
              <div
                className="px-3 pt-2.5 pb-1 text-[10px] tracking-[0.2em] uppercase sticky top-0"
                style={{ color: '#777', background: '#0d0d0dee' }}
              >
                {vendor} · {list.length}
              </div>
            )}
            {list.map((m) => {
              const selected = m === value;
              return (
                <button
                  key={m}
                  type="button"
                  onClick={() => onChange(m)}
                  className="w-full text-left px-3 py-1.5 text-[13px] transition-colors"
                  style={{
                    color: selected ? '#000' : IVORY,
                    background: selected ? ACCENT : 'transparent',
                  }}
                  onMouseEnter={(e) => {
                    if (!selected) e.currentTarget.style.background = '#1a1a1a';
                  }}
                  onMouseLeave={(e) => {
                    if (!selected) e.currentTarget.style.background = 'transparent';
                  }}
                >
                  {m}
                </button>
              );
            })}
          </div>
        ))}
      </div>
      <p className="text-[11px]" style={{ color: '#555' }}>
        {filtered.length} of {models.length} models{live ? ' · live catalog' : ''}
      </p>
    </div>
  );
}
