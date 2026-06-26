import { useState } from 'react';
import { Power } from 'lucide-react';
import { ApiError, ConfigApiCodes, patchConfig } from '@/lib/api';
import { t } from '@/lib/i18n';

export interface EntityEnabledToggleProps {
  /** Dotted prefix of the entity (`agents.clamps`, `channels.discord.clamps`, …).
   *  The toggle writes to `<prefix>.enabled`. */
  prefix: string;
  enabled: boolean;
  /** Fired after a successful flip so parents can refresh their entry state. */
  onChange: (next: boolean) => void;
}

/**
 * Pill toggle for the entity-gate `enabled` bool, hoisted out of the field
 * list onto whatever surface represents the entity (page header, card).
 * One-click flip via patchConfig — no Save round-trip.
 */
export default function EntityEnabledToggle({
  prefix,
  enabled,
  onChange,
}: EntityEnabledToggleProps) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // The drift guard (409 `config_changed_externally`) fires when the on-disk
  // config has diverged from the daemon's in-memory state on this path, e.g. a
  // hand-edit, or a daemon left running across an external config write. Track
  // it separately from generic errors so the operator gets an explicit
  // "overwrite" affordance instead of a cryptic "save failed" that silently
  // reverts the pill (the silent revert is exactly what made a stale daemon
  // look like a broken toggle).
  const [driftConflict, setDriftConflict] = useState(false);

  const apply = async (next: boolean, overrideDrift: boolean) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    setDriftConflict(false);
    try {
      await patchConfig(
        [{ op: 'replace', path: `${prefix}.enabled`, value: next }],
        overrideDrift ? { overrideDrift: true } : undefined,
      );
      onChange(next);
    } catch (e) {
      if (
        e instanceof ApiError &&
        e.envelope.code === ConfigApiCodes.configChangedExternally
      ) {
        setDriftConflict(true);
      } else {
        setError(e instanceof Error ? e.message : String(e));
      }
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="inline-flex flex-col items-end gap-1">
      <div className="inline-flex items-center gap-2">
        <button
          type="button"
          onClick={() => void apply(!enabled, false)}
          disabled={busy}
          aria-pressed={enabled}
          aria-label={enabled ? t('entity_toggle.disable') : t('entity_toggle.enable')}
          className="inline-flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs font-medium transition-colors disabled:opacity-50"
          style={{
            background: enabled
              ? 'var(--color-status-success-alpha-08)'
              : 'var(--pc-bg-elevated)',
            color: enabled
              ? 'var(--color-status-success)'
              : 'var(--pc-text-muted)',
            border: '1px solid',
            borderColor: enabled
              ? 'var(--color-status-success-alpha-20)'
              : 'var(--pc-border)',
          }}
        >
          <Power className="h-3.5 w-3.5" />
          {enabled ? t('entity_toggle.enabled') : t('entity_toggle.disabled')}
        </button>
        {error && (
          <span
            className="text-[11px]"
            style={{ color: 'var(--color-status-error)' }}
            title={error}
          >
            {t('entity_toggle.save_failed')}
          </span>
        )}
      </div>
      {driftConflict && (
        <div
          className="inline-flex items-center gap-2 text-[11px]"
          style={{ color: 'var(--color-status-warning)' }}
        >
          <span>{t('entity_toggle.drift_conflict')}</span>
          <button
            type="button"
            onClick={() => void apply(!enabled, true)}
            disabled={busy}
            className="underline disabled:opacity-50"
            style={{ color: 'var(--pc-text-link)' }}
          >
            {t('entity_toggle.overwrite')}
          </button>
        </div>
      )}
    </div>
  );
}
