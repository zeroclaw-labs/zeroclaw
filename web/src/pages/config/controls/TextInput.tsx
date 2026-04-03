import { useState } from 'react';
import { Eye, EyeOff } from 'lucide-react';

interface TextInputProps {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  disabled?: boolean;
  masked?: boolean;
}

export default function TextInput({ value, onChange, placeholder, disabled, masked }: TextInputProps) {
  const [revealed, setRevealed] = useState(false);
  const isMaskedValue = value === '***MASKED***';
  const showAsPassword = masked && !revealed;

  return (
    <div className="relative flex items-center">
      <input
        type={showAsPassword ? 'password' : 'text'}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        placeholder={placeholder}
        disabled={disabled}
        className="input-electric text-sm px-3 py-1.5 w-52 font-mono"
        style={isMaskedValue ? { color: 'var(--pc-text-muted)' } : undefined}
      />
      {masked && (
        <button
          type="button"
          onClick={() => setRevealed(!revealed)}
          className="absolute right-2 p-0.5"
          style={{ color: 'var(--pc-text-muted)' }}
        >
          {revealed ? <EyeOff className="h-3.5 w-3.5" /> : <Eye className="h-3.5 w-3.5" />}
        </button>
      )}
    </div>
  );
}
