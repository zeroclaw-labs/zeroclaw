import { useState, useEffect } from 'react';
import { Info, ExternalLink } from 'lucide-react';
import { isTauri, getAppVersion } from '../lib/tauri';
import { basePath } from '../lib/basePath';
import { t } from '@/lib/i18n';

export default function About() {
  const [version, setVersion] = useState<string>('');

  useEffect(() => {
    if (isTauri()) {
      getAppVersion().then(setVersion);
    }
  }, []);

  return (
    <div className="p-6 space-y-6 animate-fade-in">
      <div className="flex items-center gap-3">
        <Info className="h-6 w-6" style={{ color: 'var(--pc-accent)' }} />
        <h1 className="text-xl font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
          {t('about.title') || 'About'}
        </h1>
      </div>

      <div className="card p-8 text-center max-w-md mx-auto">
        <img
          src={`${basePath}/_app/zeroclaw-trans.png`}
          alt="ZeroClaw"
          className="h-24 w-24 rounded-2xl object-cover mx-auto mb-4"
          onError={(e) => { e.currentTarget.style.display = 'none'; }}
        />
        <h2 className="text-2xl font-bold text-gradient-blue mb-1">ZeroClaw</h2>
        {version && (
          <p className="text-sm mb-4" style={{ color: 'var(--pc-text-muted)' }}>
            Version {version}
          </p>
        )}
        <p className="text-sm mb-6" style={{ color: 'var(--pc-text-secondary)' }}>
          AI Hardware Agent — Desktop Automation &amp; Multi-Node Orchestration
        </p>

        <div className="space-y-2">
          {[
            { label: 'Documentation', href: 'https://docs.zeroclaw.ai' },
            { label: 'GitHub', href: 'https://github.com/zeroclawlabs/zeroclaw' },
            { label: 'Support', href: 'https://github.com/zeroclawlabs/zeroclaw/issues' },
          ].map(({ label, href }) => (
            <a
              key={label}
              href={href}
              target="_blank"
              rel="noopener noreferrer"
              className="flex items-center justify-center gap-2 text-sm py-2 rounded-xl hover:bg-[var(--pc-hover)] transition-colors"
              style={{ color: 'var(--pc-accent)' }}
            >
              {label}
              <ExternalLink className="h-3.5 w-3.5" />
            </a>
          ))}
        </div>
      </div>
    </div>
  );
}
