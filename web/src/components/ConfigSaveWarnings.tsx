import type { ValidationWarning } from '@/lib/api';
import { t } from '@/lib/i18n';
import { validationWarningMessage } from '@/lib/validationWarnings';

interface ConfigSaveWarningsProps {
  warnings: ValidationWarning[];
  onDismiss?: () => void;
  compact?: boolean;
}

export default function ConfigSaveWarnings({
  warnings,
  onDismiss,
  compact = false,
}: ConfigSaveWarningsProps) {
  if (warnings.length === 0) return null;

  const list = (
    <ul className="flex flex-col gap-0.5">
      {warnings.map((warning, index) => (
        <li key={`${warning.code}-${warning.path}-${index}`}>
          ⚠ {warning.path}: {validationWarningMessage(warning)}
        </li>
      ))}
    </ul>
  );

  return (
    <div
      role="status"
      aria-live="polite"
      className="min-w-0 max-w-full text-xs text-pc-text-secondary"
      style={{ overflowWrap: 'anywhere' }}
    >
      {compact ? (
        <details>
          <summary className="cursor-pointer">
            {warnings.length} {t(warnings.length === 1 ? 'config_warnings.warning' : 'config_warnings.warnings')}
          </summary>
          {list}
        </details>
      ) : (
        <>
          <p className="font-semibold">{t('config_warnings.saved_with_warnings')}</p>
          {list}
        </>
      )}
      {onDismiss && (
        <button
          type="button"
          onClick={onDismiss}
          aria-label={t('config_warnings.dismiss')}
          className="underline"
        >
          {t('config_warnings.dismiss')}
        </button>
      )}
    </div>
  );
}
