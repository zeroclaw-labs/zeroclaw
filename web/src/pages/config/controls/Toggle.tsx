interface ToggleProps {
  value: boolean;
  onChange: (v: boolean) => void;
  disabled?: boolean;
}

export default function Toggle({ value, onChange, disabled }: ToggleProps) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={value}
      disabled={disabled}
      onClick={() => onChange(!value)}
      className="relative inline-flex h-6 w-11 items-center rounded-full transition-colors duration-200 focus:outline-none"
      style={{
        background: value ? 'var(--pc-accent)' : 'var(--pc-bg-input)',
        border: '1px solid',
        borderColor: value ? 'var(--pc-accent)' : 'var(--pc-border)',
        opacity: disabled ? 0.4 : 1,
        cursor: disabled ? 'not-allowed' : 'pointer',
      }}
    >
      <span
        className="inline-block h-4 w-4 rounded-full bg-white transition-transform duration-200"
        style={{ transform: value ? 'translateX(22px)' : 'translateX(4px)' }}
      />
    </button>
  );
}
