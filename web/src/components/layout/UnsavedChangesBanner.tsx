// Top-of-page banner shown whenever the cross-section ConfigDraftStore
// has pending edits. Lists the affected top-level section keys, offers
// Save-all / Discard-all. Hides itself when there are no drafts.
//
// Section help and labels come from the gateway's section info; the
// banner falls back to a humanized key if the section hasn't been
// fetched yet.

import { useEffect, useState } from 'react';
import { Save, X } from 'lucide-react';
import { ApiError, getSections, type SectionInfo, type ValidationWarning } from '@/lib/api';
import {
  useConfigDirtyCount,
  useConfigDirtySections,
  useConfigDraft,
} from '@/lib/draftStore';

function humanize(key: string): string {
  if (!key) return '';
  const spaced = key.replace(/[-_.]/g, ' ');
  return spaced.charAt(0).toUpperCase() + spaced.slice(1);
}

export default function UnsavedChangesBanner() {
  const dirtyCount = useConfigDirtyCount();
  const dirtySections = useConfigDirtySections();
  const { saveAll, discardAll } = useConfigDraft();

  const [labels, setLabels] = useState<Record<string, string>>({});
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [warnings, setWarnings] = useState<ValidationWarning[]>([]);

  // Load section labels once so "channels" renders as "Channels", etc.
  useEffect(() => {
    let cancelled = false;
    getSections()
      .then((r) => {
        if (cancelled) return;
        const map: Record<string, string> = {};
        for (const s of r.sections) {
          map[s.key] = s.label;
          // Dotted keys (`providers.models`) — also key the parent so a
          // dirty path under `providers.models.anthropic.x` maps to the
          // dotted label.
          const first = s.key.split('.')[0];
          if (first && !map[first]) map[first] = s.label;
        }
        setLabels(map);
      })
      .catch(() => {
        // Best-effort; humanized fallback is fine.
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (dirtyCount === 0) return null;

  const labelFor = (key: string) => labels[key] ?? humanize(key);
  const sectionList = dirtySections.map(labelFor).join(', ');

  const onSave = async () => {
    setSaving(true);
    setError(null);
    setWarnings([]);
    try {
      const resp = await saveAll();
      setWarnings(resp.warnings ?? []);
    } catch (e) {
      if (e instanceof ApiError) {
        const env = e.envelope as { code?: string; message?: string; path?: string };
        const label = env.path ? ` (${env.path})` : '';
        setError(`[${env.code ?? 'error'}] ${env.message ?? 'save failed'}${label}`);
      } else {
        setError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      setSaving(false);
    }
  };

  return (
    <div
      className="border-b px-4 py-2 flex flex-col gap-2"
      style={{
        background: 'rgba(245, 158, 11, 0.08)',
        borderColor: 'rgba(245, 158, 11, 0.25)',
      }}
    >
      <div className="flex items-center justify-between gap-3 flex-wrap">
        <div className="text-sm" style={{ color: 'var(--pc-text-primary)' }}>
          <span style={{ color: 'var(--color-status-warning, #f59e0b)', fontWeight: 600 }}>
            {dirtyCount} unsaved {dirtyCount === 1 ? 'change' : 'changes'}
          </span>
          {sectionList && (
            <span style={{ color: 'var(--pc-text-secondary)' }}> — in {sectionList}</span>
          )}
        </div>
        <div className="flex items-center gap-2">
          <button
            type="button"
            onClick={() => {
              discardAll();
              setError(null);
              setWarnings([]);
            }}
            disabled={saving}
            className="btn-secondary inline-flex items-center gap-1 text-xs px-2.5 py-1"
          >
            <X className="h-3.5 w-3.5" />
            Discard all
          </button>
          <button
            type="button"
            onClick={() => void onSave()}
            disabled={saving}
            className="btn-electric inline-flex items-center gap-1 text-xs px-2.5 py-1"
          >
            <Save className="h-3.5 w-3.5" />
            {saving ? 'Saving…' : 'Save all'}
          </button>
        </div>
      </div>
      {error && (
        <p className="text-xs" style={{ color: 'var(--color-status-error, #f87171)' }}>
          {error}
        </p>
      )}
      {warnings.length > 0 && (
        <ul className="text-xs flex flex-col gap-0.5" style={{ color: 'var(--pc-text-secondary)' }}>
          {warnings.map((w, i) => (
            <li key={`${w.path}-${i}`}>
              ⚠ {w.path}: {w.message}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
