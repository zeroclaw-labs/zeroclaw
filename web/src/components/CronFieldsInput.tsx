import cronstrue from 'cronstrue';
import { AlertCircle, CheckCircle } from 'lucide-react';
import { useEffect, useMemo, useState } from 'react';

import { t } from '@/lib/i18n';
import {
  CRON_DEFAULT_EXPRESSION,
  CRON_FIELD_DEFINITIONS,
  cronFieldValidity,
  normalizeCronFields,
  splitCronExpression,
} from '@/lib/cron';

interface CronFieldsInputProps {
  value: string;
  onChange: (expression: string) => void;
  onValidityChange?: (valid: boolean) => void;
}

export default function CronFieldsInput({
  value,
  onChange,
  onValidityChange,
}: CronFieldsInputProps) {
  const [fields, setFields] = useState(() => splitCronExpression(value));
  const fieldValidity = useMemo(() => cronFieldValidity(fields), [fields]);
  const isValid = fieldValidity.every(Boolean);
  const expression = normalizeCronFields(fields);
  const readback = useMemo(() => {
    if (!isValid) return null;
    try {
      return cronstrue.toString(expression, { throwExceptionOnParseError: true });
    } catch {
      return null;
    }
  }, [expression, isValid]);

  useEffect(() => {
    const nextFields = splitCronExpression(value);
    setFields((current) =>
      current.join(' ') === nextFields.join(' ') ? current : nextFields,
    );
  }, [value]);

  useEffect(() => {
    onValidityChange?.(isValid && readback !== null);
  }, [isValid, onValidityChange, readback]);

  const updateField = (index: number, nextValue: string) => {
    const nextFields = [...fields];
    nextFields[index] = nextValue.replace(/\s+/g, '');
    setFields(nextFields);
    onChange(normalizeCronFields(nextFields));
  };

  return (
    <div>
      <div className="grid grid-cols-2 gap-3 sm:grid-cols-5">
        {CRON_FIELD_DEFINITIONS.map((definition, index) => {
          const fieldId = `cron-schedule-${definition.key}`;
          const invalid = !fieldValidity[index];
          return (
            <div key={definition.key}>
              <label
                htmlFor={fieldId}
                className="mb-1.5 block text-[11px] font-medium uppercase tracking-wider text-pc-text-faint"
              >
                {t(definition.labelKey)}
              </label>
              <input
                id={fieldId}
                type="text"
                value={fields[index] ?? ''}
                onChange={(event) => updateField(index, event.target.value)}
                placeholder={t('cron.field_placeholder')}
                autoCorrect="off"
                autoCapitalize="off"
                spellCheck={false}
                aria-invalid={invalid}
                aria-describedby={`${fieldId}-hint${invalid ? ` ${fieldId}-error` : ''}`}
                className={[
                  'w-full rounded-[var(--radius-md)] border bg-pc-input px-3 py-2.5 text-sm font-mono text-pc-text placeholder:text-pc-text-faint transition-colors focus:outline-none focus:ring-2 focus:ring-[var(--pc-focus)]/30',
                  invalid
                    ? 'border-status-error focus:border-status-error'
                    : 'border-pc-border focus:border-pc-border-strong',
                ].join(' ')}
              />
              <p id={`${fieldId}-hint`} className="mt-1 text-xs text-pc-text-faint">
                {t('cron.field_hint')}
              </p>
              {invalid && (
                <p id={`${fieldId}-error`} className="mt-1 text-xs text-status-error">
                  {t('cron.field_invalid')}
                </p>
              )}
            </div>
          );
        })}
      </div>

      <div className="mt-4 rounded-[var(--radius-md)] border border-pc-border bg-pc-elevated p-3">
        <div className="flex flex-wrap items-center justify-between gap-2">
          <span className="text-xs font-medium uppercase tracking-wider text-pc-text-faint">
            {t('cron.schedule_assembled')}
          </span>
          <code className="rounded bg-pc-code px-2 py-1 text-sm text-pc-text-secondary">
            {expression || CRON_DEFAULT_EXPRESSION}
          </code>
        </div>
        <div className="mt-3 flex items-start gap-2 text-sm">
          {isValid && readback !== null ? (
            <CheckCircle className="mt-0.5 h-4 w-4 shrink-0 text-status-success" aria-hidden="true" />
          ) : (
            <AlertCircle className="mt-0.5 h-4 w-4 shrink-0 text-status-error" aria-hidden="true" />
          )}
          <div>
            <div className="font-medium text-pc-text-secondary">{t('cron.schedule_readback')}</div>
            <div aria-live="polite" role="status" className={isValid && readback !== null ? 'text-status-success' : 'text-status-error'}>
              {isValid && readback !== null ? readback : t('cron.schedule_invalid')}
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
