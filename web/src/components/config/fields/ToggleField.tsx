import type { FieldProps } from '../types';

export default function ToggleField({ field, value, onChange }: FieldProps) {
  const isOn = Boolean(value);

  return (
    <div className="flex items-center gap-3">
      <button
        type="button"
        role="switch"
        aria-checked={isOn}
        aria-label={field.label}
        onClick={() => onChange(!isOn)}
        className={`relative inline-flex h-6 w-11 items-center rounded-full transition-colors ${
          isOn ? 'bg-blue-600' : 'bg-gray-700'
        }`}
      >
        <span
          className={`inline-block h-4 w-4 transform rounded-full bg-white transition-transform ${
            isOn ? 'translate-x-6' : 'translate-x-1'
          }`}
        />
      </button>
      <span className="text-sm text-gray-400">{isOn ? 'Enabled' : 'Disabled'}</span>
    </div>
  );
}
