import { Lock } from 'lucide-react';
import { Card } from '@/components/ui';
import { t } from '@/lib/i18n';
import type { PathAccess } from '@/lib/navAccess';

/**
 * Shown instead of a page the signed-in account can't use, for deep links and
 * bookmarks; the navigation doesn't offer these pages.
 */
export function NotAvailable({ access }: { access: Exclude<PathAccess, 'allowed'> }) {
  return (
    <div className="p-6">
      <Card padded={false} className="p-6 w-full max-w-lg" data-not-available={access}>
        <div className="flex items-center gap-2 mb-2">
          <Lock className="h-5 w-5 shrink-0" style={{ color: 'var(--pc-text-muted)' }} />
          <h2 className="text-lg font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
            {t('auth.not_available')}
          </h2>
        </div>
        <p className="text-sm" style={{ color: 'var(--pc-text-muted)' }}>
          {access === 'needs-pairing' ? t('auth.not_available_pairing') : t('auth.not_available_body')}
        </p>
      </Card>
    </div>
  );
}
