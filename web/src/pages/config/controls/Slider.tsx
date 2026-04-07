interface SliderProps {
  value: number;
  onChange: (v: number) => void;
  min: number;
  max: number;
  step?: number;
  disabled?: boolean;
}

export default function Slider({ value, onChange, min, max, step = 1, disabled }: SliderProps) {
  return (
    <div className="flex items-center gap-3">
      <input
        type="range"
        min={min}
        max={max}
        step={step}
        value={value}
        disabled={disabled}
        onChange={(e) => onChange(Number(e.target.value))}
        className="w-32 h-1.5 rounded-full appearance-none cursor-pointer"
        style={{
          background: `linear-gradient(to right, var(--pc-accent) 0%, var(--pc-accent) ${((value - min) / (max - min)) * 100}%, var(--pc-bg-input) ${((value - min) / (max - min)) * 100}%, var(--pc-bg-input) 100%)`,
          opacity: disabled ? 0.4 : 1,
        }}
      />
      <span className="text-xs font-mono min-w-[3ch] text-right" style={{ color: 'var(--pc-text-secondary)' }}>
        {step < 1 ? value.toFixed(1) : value}
      </span>
    </div>
  );
}
