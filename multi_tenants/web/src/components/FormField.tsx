interface Props {
  label: string;
  type?: string;
  value: string;
  onChange: (val: string) => void;
  placeholder?: string;
  required?: boolean;
}

export default function FormField({ label, type = 'text', value, onChange, placeholder, required }: Props) {
  return (
    <div className="mb-3">
      <label className="block text-sm font-medium text-text-secondary mb-1">{label}</label>
      <input
        type={type}
        value={value}
        onChange={e => onChange(e.target.value)}
        placeholder={placeholder}
        required={required}
        className="w-full px-3 py-2 bg-bg-input border border-border-default rounded-lg text-sm text-text-primary placeholder-text-muted focus:outline-none focus:ring-2 focus:ring-accent-blue focus:border-transparent transition-colors"
      />
    </div>
  );
}
