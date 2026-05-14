import { useEffect, useState } from 'react';
import { AlertTriangle, Check, X, ShieldCheck } from 'lucide-react';
import type { ApprovalDecision, PendingApproval } from '@/types/api';
import { t } from '@/lib/i18n';

interface ApprovalBannerProps {
  pending: PendingApproval;
  onRespond: (decision: ApprovalDecision) => void;
}

export default function ApprovalBanner({ pending, onRespond }: ApprovalBannerProps) {
  const [now, setNow] = useState(() => Date.now());

  useEffect(() => {
    const id = setInterval(() => setNow(Date.now()), 1000);
    return () => clearInterval(id);
  }, []);

  const elapsedMs = now - pending.receivedAt;
  const remainingSec = Math.max(0, Math.ceil(pending.timeoutSecs - elapsedMs / 1000));

  return (
    <div
      role="alert"
      aria-live="assertive"
      aria-labelledby="approval-banner-title"
      className="border-b px-4 py-3 animate-fade-in"
      style={{
        background: 'var(--color-status-warning-alpha-08, rgba(245, 158, 11, 0.08))',
        borderColor: 'var(--color-status-warning-alpha-20, rgba(245, 158, 11, 0.2))',
      }}
    >
      <div className="max-w-4xl mx-auto flex flex-col gap-2">
        <div className="flex items-start gap-3">
          <AlertTriangle
            className="h-5 w-5 shrink-0 mt-0.5"
            style={{ color: 'var(--color-status-warning, #f59e0b)' }}
          />
          <div className="flex-1 min-w-0">
            <div className="flex items-center justify-between gap-2 flex-wrap">
              <p
                id="approval-banner-title"
                className="text-sm font-semibold"
                style={{ color: 'var(--pc-text-primary)' }}
              >
                {t('agent.approval_title')}
              </p>
              <span
                className="text-xs font-mono"
                style={{ color: 'var(--pc-text-muted)' }}
                aria-hidden="true"
              >
                {t('agent.approval_timeout_in')}: {remainingSec}s
              </span>
            </div>
            <p className="text-xs mt-1" style={{ color: 'var(--pc-text-secondary)' }}>
              <span style={{ color: 'var(--pc-text-muted)' }}>{t('agent.approval_tool')}:</span>{' '}
              <span className="font-mono">{pending.toolName}</span>
            </p>
            {pending.argumentsSummary && (
              <>
                <p
                  className="text-xs mt-1"
                  style={{ color: 'var(--pc-text-muted)' }}
                  id="approval-banner-args-label"
                >
                  {t('agent.approval_arguments')}:
                </p>
                <pre
                  className="text-xs mt-1 whitespace-pre-wrap break-words leading-relaxed p-2 rounded-lg max-h-40 overflow-auto"
                  style={{ background: 'var(--pc-bg-surface)', color: 'var(--pc-text-secondary)' }}
                  aria-labelledby="approval-banner-args-label"
                >
                  {pending.argumentsSummary}
                </pre>
              </>
            )}
          </div>
        </div>

        <div className="flex items-center gap-2 justify-end">
          <button
            type="button"
            onClick={() => onRespond('deny')}
            className="btn-secondary flex items-center gap-1.5 text-xs"
            style={{ padding: '0.35rem 0.85rem', borderRadius: '0.5rem' }}
          >
            <X className="h-3.5 w-3.5" />
            {t('agent.approval_deny')}
          </button>
          <button
            type="button"
            onClick={() => onRespond('approve')}
            className="btn-electric flex items-center gap-1.5 text-xs"
            style={{ padding: '0.35rem 0.85rem', borderRadius: '0.5rem', color: 'white' }}
          >
            <Check className="h-3.5 w-3.5" />
            {t('agent.approval_approve')}
          </button>
          <button
            type="button"
            onClick={() => onRespond('always')}
            className="btn-secondary flex items-center gap-1.5 text-xs"
            style={{ padding: '0.35rem 0.85rem', borderRadius: '0.5rem' }}
            title={t('agent.approval_always_hint')}
          >
            <ShieldCheck className="h-3.5 w-3.5" />
            {t('agent.approval_always')}
          </button>
        </div>
      </div>
    </div>
  );
}
