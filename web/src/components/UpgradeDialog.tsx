import { useEffect, useId, useRef } from 'react';
import { Button } from '@/components/ui/Button';
import { t } from '@/lib/i18n';
import type { VersionCheckResponse } from '@/lib/api';

export interface UpgradeDialogProps {
  /** Whether the dialog is mounted/visible. */
  open: boolean;
  /** Latest version-check result, or null while the first check is in flight. */
  info: VersionCheckResponse | null;
  /** Whether a check is currently running. */
  loading: boolean;
  /** Close the dialog (Esc, backdrop, or Close button). */
  onClose: () => void;
}

/**
 * Phase 1 (read-only) upgrade dialog: shows the current vs. latest version and
 * release notes, with a link to the GitHub release. No upgrade action yet —
 * that lands in Phase 2 behind `gateway.allow_self_upgrade`.
 *
 * Mirrors the modal conventions in `ConfirmDialog`/`SettingsModal` (token
 * classes, focus trap, Esc + backdrop close).
 */
export function UpgradeDialog({ open, info, loading, onClose }: UpgradeDialogProps) {
  const panelRef = useRef<HTMLDivElement>(null);
  const titleId = useId();

  useEffect(() => {
    if (!open) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    const buttons = panelRef.current?.querySelectorAll<HTMLButtonElement>('button');
    buttons?.[buttons.length - 1]?.focus();
    return () => previouslyFocused?.focus?.();
  }, [open]);

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.preventDefault();
        onClose();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!open) return null;

  const hasError = info?.error != null && info.error !== '';
  const isNewer = info?.is_newer === true;
  const published =
    info?.published_at != null
      ? new Date(info.published_at).toLocaleDateString()
      : null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      className="fixed inset-0 z-50 flex items-center justify-center"
      onClick={onClose}
    >
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />
      <div
        ref={panelRef}
        className="relative w-full max-w-md mx-4 rounded-[var(--radius-xl)] border border-pc-border bg-pc-base shadow-[var(--pc-shadow-md)] animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Body */}
        <div className="px-6 pt-5 pb-4 flex flex-col gap-3">
          <h2 id={titleId} className="text-sm font-semibold text-pc-text">
            {t('upgrade.title')}
          </h2>

          {loading && info == null ? (
            <div className="text-xs text-pc-text-muted">{t('upgrade.checking')}</div>
          ) : hasError ? (
            <div className="text-xs text-pc-text-muted">
              <div className="text-pc-text">{t('upgrade.check_failed')}</div>
              <div className="mt-1 font-mono break-words">{info?.error}</div>
            </div>
          ) : (
            <>
              <dl className="text-xs grid grid-cols-[auto_1fr] gap-x-4 gap-y-1">
                <dt className="text-pc-text-muted">{t('upgrade.current')}</dt>
                <dd className="text-pc-text font-mono">{info?.current_version}</dd>
                <dt className="text-pc-text-muted">{t('upgrade.latest')}</dt>
                <dd className="text-pc-text font-mono">
                  {info?.latest_version ?? '—'}
                  {published && (
                    <span className="text-pc-text-muted"> · {published}</span>
                  )}
                </dd>
              </dl>

              {!isNewer && (
                <div className="text-xs text-pc-text-muted">
                  {t('upgrade.up_to_date')}
                </div>
              )}

              {isNewer && info?.release_notes && (
                <div className="flex flex-col gap-1">
                  <div className="text-xs font-medium text-pc-text">
                    {t('upgrade.notes')}
                  </div>
                  <div className="max-h-60 overflow-auto rounded-[var(--radius-md)] border border-pc-border bg-pc-surface px-3 py-2 text-xs leading-relaxed text-pc-text-muted whitespace-pre-wrap">
                    {info.release_notes}
                  </div>
                </div>
              )}
            </>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-2 px-6 py-4 border-t border-pc-border">
          {info?.release_url && (
            <a
              href={info.release_url}
              target="_blank"
              rel="noopener noreferrer"
              className="text-xs text-pc-accent hover:underline"
            >
              {t('upgrade.open_release')}
            </a>
          )}
          <Button variant="ghost" onClick={onClose}>
            {t('upgrade.close')}
          </Button>
        </div>
      </div>
    </div>
  );
}

export default UpgradeDialog;
