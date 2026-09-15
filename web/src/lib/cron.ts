export const CRON_DEFAULT_EXPRESSION = '0 9 * * 1-5';

export const CRON_FIELD_DEFINITIONS = [
  { key: 'minute', labelKey: 'cron.field_minute', min: 0, max: 59 },
  { key: 'hour', labelKey: 'cron.field_hour', min: 0, max: 23 },
  { key: 'day', labelKey: 'cron.field_day', min: 1, max: 31 },
  { key: 'month', labelKey: 'cron.field_month', min: 1, max: 12 },
  { key: 'weekday', labelKey: 'cron.field_weekday', min: 0, max: 7 },
] as const;

const CRON_FIELD_COUNT = CRON_FIELD_DEFINITIONS.length;

function isInRange(value: string, min: number, max: number): boolean {
  if (!/^\d+$/.test(value)) return false;
  const number = Number(value);
  return Number.isInteger(number) && number >= min && number <= max;
}

function isValidRange(value: string, min: number, max: number): boolean {
  const parts = value.split('-');
  if (parts.length !== 2) return false;
  const start = parts[0];
  const end = parts[1];
  return start !== undefined && end !== undefined && isInRange(start, min, max) && isInRange(end, min, max) && Number(start) <= Number(end);
}

function isValidToken(token: string, min: number, max: number): boolean {
  if (!token) return false;

  const parts = token.split('/');
  if (parts.length > 2) return false;

  const base = parts[0];
  const step = parts[1];
  if (base === undefined) return false;
  if (step !== undefined && (!/^\d+$/.test(step) || Number(step) < 1)) {
    return false;
  }

  if (base === '*') return true;
  if (step !== undefined && !base.includes('-')) return false;
  if (base.includes('-')) return isValidRange(base, min, max);
  return isInRange(base, min, max) && step === undefined;
}

/** Return whether one standard five-field crontab field is valid. */
export function isValidCronField(value: string, fieldIndex: number): boolean {
  const definition = CRON_FIELD_DEFINITIONS[fieldIndex];
  if (!definition || value.trim() !== value || value.length === 0) return false;

  return value.split(',').every((token) =>
    isValidToken(token, definition.min, definition.max),
  );
}

/** Return one validity flag per minute/hour/day/month/weekday field. */
export function cronFieldValidity(fields: readonly string[]): boolean[] {
  return CRON_FIELD_DEFINITIONS.map((_, index) =>
    isValidCronField(fields[index] ?? '', index),
  );
}

/** Return whether an expression uses the supported five-field cron grammar. */
export function isValidCronExpression(expression: string): boolean {
  const fields = expression.trim().split(/\s+/);
  return fields.length === CRON_FIELD_COUNT && cronFieldValidity(fields).every(Boolean);
}

/** Normalize an expression into the single-space form sent to the API. */
export function normalizeCronFields(fields: readonly string[]): string {
  return fields.map((field) => field.trim().replace(/\s+/g, '')).join(' ');
}

/** Parse an external expression, falling back to the issue's valid default. */
export function splitCronExpression(expression: string): string[] {
  const fields = expression.trim().split(/\s+/);
  return fields.length === CRON_FIELD_COUNT && fields.every(Boolean)
    ? fields
    : CRON_DEFAULT_EXPRESSION.split(' ');
}
