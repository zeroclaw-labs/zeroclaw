import { createElement } from 'react';
import { RefreshCw } from 'lucide-react';
import type { CatalogLoadWarning } from '../lib/toolCatalog';
import { t } from '../lib/i18n';

export function catalogWarningLabel(warning: CatalogLoadWarning): string {
  const prefix =
    warning.source === 'agent'
      ? t('tool_picker.partial_load_agent_prefix')
      : t('tool_picker.partial_load_cli_prefix');
  return `${prefix}${warning.message}`;
}

export function ToolCatalogWarningPanel({
  warnings,
  onRetry,
  retryDisabled,
}: {
  warnings: CatalogLoadWarning[];
  onRetry: () => void;
  retryDisabled: boolean;
}) {
  return createElement(
    'div',
    {
      // The panel is inserted asynchronously after the catalog settles, so it
      // must be announced to assistive tech. `status`/`polite` (not `alert`)
      // because a partial load is recoverable and should not interrupt.
      role: 'status',
      'aria-live': 'polite',
      className:
        'rounded-[var(--radius-md)] border border-status-warning/25 bg-status-warning/10 px-3 py-2 text-xs text-status-warning',
    },
    createElement(
      'div',
      { className: 'flex items-start justify-between gap-3' },
      createElement(
        'div',
        { className: 'min-w-0' },
        createElement('p', { className: 'font-medium' }, t('tool_picker.partial_load')),
        createElement(
          'ul',
          { className: 'mt-1 space-y-0.5' },
          warnings.map((warning) =>
            createElement('li', { key: warning.source }, catalogWarningLabel(warning)),
          ),
        ),
      ),
      createElement(
        'button',
        {
          type: 'button',
          onClick: onRetry,
          disabled: retryDisabled,
          title: t('tool_picker.retry_catalog'),
          'aria-label': t('tool_picker.retry_catalog'),
          className:
            'inline-flex h-8 w-8 flex-shrink-0 items-center justify-center rounded-[var(--radius-md)] border border-status-warning/30 text-status-warning transition-colors hover:bg-status-warning/10 focus:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]/40 disabled:opacity-40 disabled:cursor-not-allowed',
        },
        createElement(RefreshCw, { className: 'h-3.5 w-3.5' }),
      ),
    ),
  );
}
