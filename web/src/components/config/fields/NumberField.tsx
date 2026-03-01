import type { FieldProps } from '../types';

export default function NumberField({ field, value, onChange }: FieldProps) {
  const numValue = value === undefined || value === null || value === '' ? '' : Number(value);

  return (
    <input
      type="number"
      value={numValue}
      onChange={(e) => {
        const raw = e.target.value;
        if (raw === '') {
          onChange(undefined);
          return;
        }
        const n = Number(raw);
        if (!isNaN(n)) {
          onChange(n);
        }
      }}
      onBlur={(e) => {
        if (field.step !== undefined && field.step < 1) {
          return;
        }
        const raw = e.target.value;
        if (raw === '') {
          return;
        }
        const n = Number(raw);
        if (!isNaN(n)) {
          onChange(Math.floor(n));
        }
      }}
      min={field.min}
      max={field.max}
      step={field.step ?? 1}
      placeholder={field.description ?? ''}
      className="w-full bg-gray-800 border border-gray-700 rounded-lg px-3 py-2 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500"
    />
  );
}
