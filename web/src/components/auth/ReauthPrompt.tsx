import { useEffect, useId, useRef, useState } from 'react';
import { Button } from '@/components/ui/Button';
import { useAuth } from '@/hooks/useAuth';
import { providerAlias } from '@/lib/auth';
import { t } from '@/lib/i18n';

/**
 * Re-authentication for a signed-in session: a banner two minutes before the
 * session expires, and a modal once it has. Both sign in again in a separate
 * window, so the page underneath (and anything typed into it) stays. The same
 * account resumes in place; a different account reloads the page.
 */
export function ReauthPrompt() {
  const {
    session,
    expiryWarning,
    reauthRequired,
    reauthPending,
    reauthenticate,
    continueReauthHere,
    checkSession,
    signOut,
  } = useAuth();
  const [blocked, setBlocked] = useState(false);

  // A fresh session (or none) starts over.
  useEffect(() => {
    setBlocked(false);
  }, [session]);

  if (!session) return null;
  const provider = providerAlias(session.provider);
  const start = () => setBlocked(reauthenticate() === 'blocked');

  if (reauthRequired) {
    return (
      <ReauthDialog
        provider={provider}
        pending={reauthPending}
        blocked={blocked}
        onStart={start}
        onContinueHere={continueReauthHere}
        onCheck={checkSession}
        onSignOut={() => {
          void signOut().catch(() => {
            // The session is already unusable; the next request signs out.
          });
        }}
      />
    );
  }

  if (!expiryWarning) return null;
  return (
    <div
      role="status"
      className="fixed bottom-4 left-1/2 -translate-x-1/2 z-[200] flex items-center gap-3 rounded-[var(--radius-lg)] border border-pc-border bg-pc-elevated px-4 py-2 text-sm text-pc-text shadow-[var(--pc-shadow-md)]"
    >
      <span>{t('auth.reauth.expiring')}</span>
      <Button size="sm" onClick={start}>
        {t('auth.reauth.sign_in_again')}
      </Button>
      {blocked && (
        <Button size="sm" variant="ghost" onClick={continueReauthHere}>
          {t('auth.reauth.continue_here')}
        </Button>
      )}
    </div>
  );
}

function ReauthDialog({
  provider,
  pending,
  blocked,
  onStart,
  onContinueHere,
  onCheck,
  onSignOut,
}: {
  provider: string;
  pending: boolean;
  blocked: boolean;
  onStart: () => void;
  onContinueHere: () => void;
  onCheck: () => void;
  onSignOut: () => void;
}) {
  const panelRef = useRef<HTMLDivElement>(null);
  const titleId = useId();
  const descId = useId();

  // Focus the primary action (the last button) on open. There is no Escape:
  // the page behind can't be used until the user signs in again or signs out.
  useEffect(() => {
    const buttons = panelRef.current?.querySelectorAll<HTMLButtonElement>('button');
    buttons?.[buttons.length - 1]?.focus();
  }, []);

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-labelledby={titleId}
      aria-describedby={descId}
      className="fixed inset-0 z-[10000] flex items-center justify-center"
    >
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />
      <div
        ref={panelRef}
        className="relative w-full max-w-sm mx-4 rounded-[var(--radius-xl)] border border-pc-border bg-pc-base shadow-[var(--pc-shadow-md)] animate-fade-in"
      >
        <div className="px-6 pt-5 pb-4 flex flex-col gap-2">
          <h2 id={titleId} className="text-sm font-semibold text-pc-text">
            {t('auth.reauth.title')}
          </h2>
          <p id={descId} className="text-xs leading-relaxed text-pc-text-muted">
            {t('auth.reauth.body').replace('{provider}', provider)}
          </p>
          {pending && !blocked && (
            <p role="status" className="text-xs leading-relaxed text-pc-text-muted">
              {t('auth.reauth.waiting')}
            </p>
          )}
          {blocked && (
            <p role="alert" className="text-xs leading-relaxed text-status-error">
              {t('auth.reauth.popup_blocked')}
            </p>
          )}
        </div>
        <div className="flex flex-wrap items-center justify-end gap-2 px-6 py-4 border-t border-pc-border">
          <Button variant="ghost" onClick={onSignOut}>
            {t('auth.sign_out')}
          </Button>
          {pending && !blocked && (
            <Button variant="ghost" onClick={onCheck}>
              {t('auth.reauth.check_again')}
            </Button>
          )}
          {blocked && (
            <Button variant="ghost" onClick={onContinueHere}>
              {t('auth.reauth.continue_here')}
            </Button>
          )}
          <Button onClick={onStart}>{t('auth.reauth.sign_in_again')}</Button>
        </div>
      </div>
    </div>
  );
}
