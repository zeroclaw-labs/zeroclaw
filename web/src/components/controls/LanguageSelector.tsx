import { Check, ChevronDown } from 'lucide-react';
import { useEffect, useRef, useState } from 'react';
import {
  getLanguageOption,
  getLocaleDirection,
  LANGUAGE_OPTIONS,
  type Locale,
} from '@/lib/i18n';

interface LanguageSelectorProps {
  locale: Locale;
  onChange: (locale: Locale) => void;
  ariaLabel: string;
  title?: string;
  align?: 'left' | 'right';
  buttonClassName?: string;
  menuClassName?: string;
}

export default function LanguageSelector({
  locale,
  onChange,
  ariaLabel,
  title,
  align = 'right',
  buttonClassName = '',
  menuClassName = '',
}: LanguageSelectorProps) {
  const [open, setOpen] = useState(false);
  const containerRef = useRef<HTMLDivElement | null>(null);
  const activeLanguage = getLanguageOption(locale);
  const localeDirection = getLocaleDirection(locale);

  useEffect(() => {
    const handlePointerDown = (event: MouseEvent) => {
      if (!containerRef.current?.contains(event.target as Node)) {
        setOpen(false);
      }
    };

    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        setOpen(false);
      }
    };

    window.addEventListener('mousedown', handlePointerDown);
    window.addEventListener('keydown', handleKeyDown);
    return () => {
      window.removeEventListener('mousedown', handlePointerDown);
      window.removeEventListener('keydown', handleKeyDown);
    };
  }, []);

  const alignmentClass = align === 'left' ? 'left-0' : 'right-0';

  return (
    <div ref={containerRef} className="relative" dir={localeDirection}>
      <button
        type="button"
        data-testid="locale-select"
        aria-label={ariaLabel}
        aria-haspopup="listbox"
        aria-expanded={open}
        title={title}
        onClick={() => setOpen((current) => !current)}
        className={buttonClassName}
      >
        <span
          aria-hidden="true"
          data-testid="locale-flag"
          className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-[#0f2450] text-sm shadow-inner shadow-black/20"
        >
          {activeLanguage.flag}
        </span>
        <span dir={localeDirection} className="min-w-0 truncate text-start">{activeLanguage.label}</span>
        <ChevronDown className={`h-4 w-4 shrink-0 transition ${open ? 'rotate-180' : ''}`} />
      </button>

      {open ? (
        <div
          className={`absolute z-50 mt-2 w-72 max-w-[min(18rem,calc(100vw-2rem))] overflow-hidden rounded-2xl border border-[#2b4f97] bg-[#071228]/96 shadow-[0_20px_60px_rgba(0,0,0,0.45)] backdrop-blur-xl ${alignmentClass} ${menuClassName}`}
        >
          <div
            role="listbox"
            aria-label={ariaLabel}
            data-testid="locale-menu"
            className="max-h-80 overflow-y-auto p-2"
          >
            {LANGUAGE_OPTIONS.map((option) => {
              const selected = option.value === locale;
              return (
                <button
                  key={option.value}
                  type="button"
                  role="option"
                  aria-selected={selected}
                  data-testid={`locale-option-${option.value}`}
                  onClick={() => {
                    onChange(option.value);
                    setOpen(false);
                  }}
                  className={`flex w-full items-center gap-3 rounded-xl px-3 py-2 text-sm transition text-start ${
                    selected
                      ? 'bg-[#13305f] text-white shadow-[0_0_0_1px_rgba(79,131,255,0.24)]'
                      : 'text-[#c4d8ff] hover:bg-[#0d2147] hover:text-white'
                  }`}
                >
                  <span
                    aria-hidden="true"
                    className="flex h-7 w-7 shrink-0 items-center justify-center rounded-full bg-[#0f2450] text-base shadow-inner shadow-black/20"
                  >
                    {option.flag}
                  </span>
                  <span dir={option.direction} className="min-w-0 flex-1 truncate text-start">
                    {option.label}
                  </span>
                  {selected ? <Check className="h-4 w-4 shrink-0 text-[#8cc2ff]" /> : null}
                </button>
              );
            })}
          </div>
        </div>
      ) : null}
    </div>
  );
}
