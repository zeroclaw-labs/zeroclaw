import { useState, useEffect } from 'react';
import { Eye, EyeOff } from 'lucide-react';

interface TextInputProps {
  value: string;
  onChange: (v: string) => void;
  placeholder?: string;
  disabled?: boolean;
  masked?: boolean;
  /** When true, onChange fires only on blur/Enter, not on every keystroke. */
  commitOnBlur?: boolean;
}

export default function TextInput({ value, onChange, placeholder, disabled, masked, commitOnBlur }: TextInputProps) {
  const [revealed, setRevealed] = useState(false);
  const [draft, setDraft] = useState(value);
  const [focused, setFocused] = useState(false);

  // Sync draft from parent when not focused (external config changes)
  useEffect(() => {
    if (!focused) setDraft(value);
  }, [value, focused]);

  const isMaskedValue = value === '***MASKED***';
  const showAsPassword = masked && !revealed;

  const handleChange = (raw: string) => {
    if (commitOnBlur) {
      setDraft(raw);
    } else {
      onChange(raw);
    }
  };

  const handleBlur = () => {
    setFocused(false);
    if (commitOnBlur) onChange(draft);
  };

  const handleKeyDown = (e: React.KeyboardEvent<HTMLInputElement>) => {
    if (commitOnBlur && e.key === 'Enter') {
      onChange(draft);
      (e.target as HTMLInputElement).blur();
    }
  };

  return (
    <div className="relative flex items-center">
      <input
        type={showAsPassword ? 'password' : 'text'}
        value={commitOnBlur ? draft : value}
        onChange={(e) => handleChange(e.target.value)}
        onFocus={() => setFocused(true)}
        onBlur={handleBlur}
        onKeyDown={commitOnBlur ? handleKeyDown : undefined}
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
