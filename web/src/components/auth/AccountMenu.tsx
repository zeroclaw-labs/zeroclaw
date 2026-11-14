import { useEffect, useRef, useState } from 'react';
import { ChevronDown, LogOut, UserRound } from 'lucide-react';
import { Button } from '@/components/ui';
import { providerAlias, type OidcSession } from '@/lib/auth';
import { t } from '@/lib/i18n';

/**
 * Header menu for a signed-in account: who is signed in, with which provider,
 * and sign-out. Sign-out ends the ZeroClaw session only; the provider's own
 * session is untouched, and the menu says so.
 */
export function AccountMenu({
  session,
  onSignOut,
}: {
  session: OidcSession;
  onSignOut: () => Promise<void>;
}) {
  const [open, setOpen] = useState(false);
  const [signingOut, setSigningOut] = useState(false);
  const [error, setError] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const provider = providerAlias(session.provider);
  const name = session.display_name || session.principal_id;

  useEffect(() => {
    if (!open) return;
    const handler = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setOpen(false);
    };
    document.addEventListener('mousedown', handler);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('mousedown', handler);
      document.removeEventListener('keydown', onKey);
    };
  }, [open]);

  const signOut = async () => {
    setSigningOut(true);
    setError(false);
    try {
      await onSignOut();
    } catch {
      setError(true);
      setSigningOut(false);
    }
  };

  return (
    <div ref={ref} className="relative" style={{ zIndex: 9999 }}>
      <Button
        variant="ghost"
        onClick={() => setOpen((v) => !v)}
        aria-expanded={open}
        aria-haspopup="true"
        aria-label={t('auth.account.menu')}
        className="h-9 px-3 text-xs gap-1.5 max-w-[14rem]"
      >
        <UserRound className="h-[20px] w-[20px] shrink-0" />
        <span className="hidden sm:inline truncate">{name}</span>
        <ChevronDown
          className="h-3 w-3 shrink-0"
          style={{ transform: open ? 'rotate(180deg)' : undefined, transition: 'transform 0.15s' }}
        />
      </Button>

      {open && (
        <div
          className="absolute right-0 top-full mt-1 w-72 rounded-xl border shadow-lg p-3 text-xs"
          style={{
            background: 'var(--pc-bg-elevated)',
            borderColor: 'var(--pc-border)',
            color: 'var(--pc-text-secondary)',
          }}
        >
          <div className="mb-3">
            <div className="text-sm font-semibold truncate" style={{ color: 'var(--pc-text-primary)' }}>
              {name}
            </div>
            <div className="mt-0.5" style={{ color: 'var(--pc-text-muted)' }}>
              {t('auth.account.signed_in_with').replace('{provider}', provider)}
              {session.admin && <> · {t('auth.account.admin')}</>}
            </div>
          </div>
          <Button
            variant="ghost"
            onClick={() => void signOut()}
            disabled={signingOut}
            className="w-full justify-start h-9 px-3 gap-1.5 hover:text-status-error hover:border-status-error/25 hover:bg-status-error/10"
          >
            <LogOut className="h-[18px] w-[18px] shrink-0" />
            {t('auth.sign_out')}
          </Button>
          <p className="mt-2 leading-relaxed" style={{ color: 'var(--pc-text-muted)' }}>
            {t('auth.account.sign_out_note').replace('{provider}', provider)}
          </p>
          {error && (
            <p role="alert" className="mt-2 leading-relaxed" style={{ color: 'var(--color-status-error)' }}>
              {t('auth.sign_out_failed')}
            </p>
          )}
        </div>
      )}
    </div>
  );
}
