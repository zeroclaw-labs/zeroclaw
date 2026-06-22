import { useEffect, useState } from 'react';
import { AlertTriangle, Check, X, ShieldCheck } from 'lucide-react';
import type { ApprovalDecision, PendingApproval } from '@/types/api';
import { Button } from '@/components/ui';
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
      className="border-b border-status-warning/20 bg-status-warning/[0.08] px-4 py-3 animate-fade-in"
    >
      <div className="max-w-4xl mx-auto flex flex-col gap-2">
        <div className="flex items-start gap-3">
          <AlertTriangle className="h-5 w-5 shrink-0 mt-0.5 text-status-warning" />
          <div className="flex-1 min-w-0">
            <div className="flex items-center justify-between gap-2 flex-wrap">
              <p
                id="approval-banner-title"
                className="text-sm font-semibold text-pc-text"
              >
                {t('agent.approval_title')}
              </p>
              <span
                className="text-xs font-mono text-pc-text-muted"
                aria-hidden="true"
              >
                {t('agent.approval_timeout_in')}: {remainingSec}s
              </span>
            </div>
            <p className="text-xs mt-1 text-pc-text-secondary">
              <span className="text-pc-text-muted">{t('agent.approval_tool')}:</span>{' '}
              <span className="font-mono">{pending.toolName}</span>
            </p>
            {pending.argumentsSummary && (
              <>
                <p
                  className="text-xs mt-1 text-pc-text-muted"
                  id="approval-banner-args-label"
                >
                  {t('agent.approval_arguments')}:
                </p>
                <pre
                  className="text-xs mt-1 whitespace-pre-wrap break-words leading-relaxed p-2 rounded-[var(--radius-md)] max-h-40 overflow-auto bg-pc-code text-pc-text-secondary border border-pc-border"
                  aria-labelledby="approval-banner-args-label"
                >
                  {pending.argumentsSummary}
                </pre>
              </>
            )}
          </div>
        </div>

        <div className="flex items-center gap-2 justify-end">
          <Button
            size="sm"
            variant="danger"
            onClick={() => onRespond('deny')}
          >
            <X className="h-3.5 w-3.5" />
            {t('agent.approval_deny')}
          </Button>
          <Button
            size="sm"
            variant="ghost"
            onClick={() => onRespond('always')}
            title={t('agent.approval_always_hint')}
          >
            <ShieldCheck className="h-3.5 w-3.5" />
            {t('agent.approval_always')}
          </Button>
          <Button
            size="sm"
            variant="primary"
            onClick={() => onRespond('approve')}
          >
            <Check className="h-3.5 w-3.5" />
            {t('agent.approval_approve')}
          </Button>
        </div>
      </div>
    </div>
  );
}
