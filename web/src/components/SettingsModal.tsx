import { useEffect, useMemo, useRef, useState } from 'react';
import { X, Settings, Sun, Moon, Monitor, Laptop, Check, Type, CaseSensitive, Palette } from 'lucide-react';
import { useTheme } from '@/hooks/useTheme';
import { t } from '@/lib/i18n';
import type { AccentColor, UiFont, MonoFont, ThemeMode } from '@/contexts/ThemeContext';
import { uiFontStacks, monoFontStacks } from '@/contexts/ThemeContext';
import { colorThemes } from '@/contexts/colorThemes';

const themeOptions: { value: ThemeMode; icon: typeof Sun; labelKey: string; previewBg: string; previewFg: string }[] = [
  { value: 'system', icon: Laptop, labelKey: 'theme.system', previewBg: 'linear-gradient(135deg, #1e1e24 50%, #f4f4f5 50%)', previewFg: '#d4d4d8' },
  { value: 'dark', icon: Moon, labelKey: 'theme.dark', previewBg: '#1e1e24', previewFg: '#d4d4d8' },
  { value: 'light', icon: Sun, labelKey: 'theme.light', previewBg: '#f4f4f5', previewFg: '#18181b' },
  { value: 'oled', icon: Monitor, labelKey: 'theme.oled', previewBg: '#000000', previewFg: '#d4d4d8' },
];

const accentOptions: { value: AccentColor; color: string }[] = [
  { value: 'cyan', color: '#22d3ee' },
  { value: 'violet', color: '#8b5cf6' },
  { value: 'emerald', color: '#10b981' },
  { value: 'amber', color: '#f59e0b' },
  { value: 'rose', color: '#f43f5e' },
  { value: 'blue', color: '#3b82f6' },
];

const uiFontOptions: { value: UiFont; label: string; sample: string }[] = [
  { value: 'system', label: 'System', sample: 'Segoe/UI' },
  { value: 'inter', label: 'Inter', sample: 'Inter' },
  { value: 'segoe', label: 'Segoe UI', sample: 'Segoe' },
  { value: 'sf', label: 'SF Pro', sample: 'SF' },
];

const monoFontOptions: { value: MonoFont; label: string; sample: string }[] = [
  { value: 'jetbrains', label: 'JetBrains Mono', sample: 'JetBrains' },
  { value: 'fira', label: 'Fira Code', sample: 'Fira' },
  { value: 'cascadia', label: 'Cascadia Code', sample: 'Cascadia' },
  { value: 'system-mono', label: 'System mono', sample: 'System' },
];

const uiSizes = [14, 15, 16, 17, 18];
const monoSizes = [13, 14, 15, 16, 17];

// Shared selectable-chip classes. Hover is pure CSS (no JS handlers): the
// inactive state lifts to `--pc-hover` on hover; the active state is an
// accent-tinted token surface. Both carry a strong focus-visible ring.
const chipBase =
  'border transition-colors duration-150 focus-visible:outline-none ' +
  'focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] ' +
  'focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base cursor-pointer';
const chipInactive =
  'border-pc-border text-pc-text-muted bg-transparent ' +
  'hover:bg-[var(--pc-hover)] hover:text-pc-text';
const chipActive =
  'border-pc-accent-dim bg-pc-accent/10 text-pc-accent-light';

function chip(active: boolean, extra = '') {
  return [chipBase, active ? chipActive : chipInactive, extra].filter(Boolean).join(' ');
}

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <div className="text-[10px] uppercase tracking-wider font-semibold mb-2 mt-5 first:mt-0 text-pc-text-faint">
      {children}
    </div>
  );
}

/** Mini terminal preview card for a color theme. */
function ThemePreviewCard({
  theme,
  active,
  onClick,
}: {
  theme: typeof colorThemes[number];
  active: boolean;
  onClick: () => void;
}) {
  const [bg, c1, c2, c3, text] = theme.preview;
  return (
    <button
      type="button"
      onClick={onClick}
      className={[
        'flex flex-col gap-1.5 p-2 rounded-[var(--radius-lg)] border text-left group',
        'min-w-0 w-full', // let the card shrink with the grid track on narrow screens
        'transition-colors duration-150 cursor-pointer',
        'focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]',
        'focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base',
        active
          ? 'border-pc-accent bg-pc-accent/10'
          : 'border-pc-border hover:bg-[var(--pc-hover)] hover:border-pc-border-strong',
      ].join(' ')}
      aria-pressed={active}
    >
      {/* Mini terminal — keeps the theme's literal preview colors (it is a preview). */}
      <div
        className="w-full rounded-lg overflow-hidden"
        style={{ background: bg, border: `1px solid ${theme.scheme === 'dark' ? 'rgba(255,255,255,0.08)' : 'rgba(0,0,0,0.08)'}` }}
      >
        {/* Title bar dots */}
        <div className="flex gap-1 px-2 py-1.5">
          <span className="w-[6px] h-[6px] rounded-full" style={{ background: '#ff5f57' }} />
          <span className="w-[6px] h-[6px] rounded-full" style={{ background: '#febc2e' }} />
          <span className="w-[6px] h-[6px] rounded-full" style={{ background: '#28c840' }} />
        </div>
        {/* Fake code lines */}
        <div className="px-2 pb-2 flex flex-col gap-[3px]">
          <div className="flex gap-1 items-center">
            <span className="h-[3px] rounded-full" style={{ background: c1, width: '30%' }} />
            <span className="h-[3px] rounded-full" style={{ background: text, width: '20%', opacity: 0.4 }} />
          </div>
          <div className="flex gap-1 items-center">
            <span className="h-[3px] rounded-full" style={{ background: text, width: '15%', opacity: 0.3 }} />
            <span className="h-[3px] rounded-full" style={{ background: c2, width: '25%' }} />
            <span className="h-[3px] rounded-full" style={{ background: c3, width: '18%' }} />
          </div>
          <div className="flex gap-1 items-center">
            <span className="h-[3px] rounded-full" style={{ background: c3, width: '22%' }} />
            <span className="h-[3px] rounded-full" style={{ background: text, width: '28%', opacity: 0.3 }} />
          </div>
        </div>
      </div>
      {/* Label */}
      <div className="flex items-center gap-1 px-0.5">
        {active && <Check size={10} className="text-pc-accent" />}
        <span
          className={[
            'text-[10px] font-medium truncate',
            active ? 'text-pc-accent-light' : 'text-pc-text-muted',
          ].join(' ')}
        >
          {theme.name}
        </span>
      </div>
    </button>
  );
}

interface Props {
  open: boolean;
  onClose: () => void;
}

export function SettingsModal({ open, onClose }: Props) {
  const {
    theme, accent, colorTheme, uiFont, monoFont, uiFontSize, monoFontSize,
    setTheme, setAccent, setColorTheme, setUiFont, setMonoFont, setUiFontSize, setMonoFontSize,
  } = useTheme();

  type TabId = 'appearance' | 'themes' | 'typography';
  const [tab, setTab] = useState<TabId>('appearance');

  const panelRef = useRef<HTMLDivElement>(null);

  const tabs: { id: TabId; label: string; icon: typeof Palette }[] = useMemo(() => [
    { id: 'appearance', label: t('settings.tab.appearance'), icon: Settings },
    { id: 'themes', label: 'Themes', icon: Palette },
    { id: 'typography', label: t('settings.tab.typography'), icon: Type },
  ], []);

  // Group themes by scheme for the themes tab
  const darkThemes = useMemo(() => colorThemes.filter(ct => ct.scheme === 'dark'), []);
  const lightThemes = useMemo(() => colorThemes.filter(ct => ct.scheme === 'light'), []);

  // Focus management: focus the first control on open, restore focus to the
  // trigger on close.
  useEffect(() => {
    if (!open) return;
    const previouslyFocused = document.activeElement as HTMLElement | null;
    const panel = panelRef.current;
    const firstFocusable = panel?.querySelector<HTMLElement>(
      'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
    );
    firstFocusable?.focus();
    return () => previouslyFocused?.focus?.();
  }, [open]);

  // Esc closes; Tab is trapped within the modal panel.
  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        onClose();
        return;
      }
      if (e.key !== 'Tab') return;
      const panel = panelRef.current;
      if (!panel) return;
      const focusable = Array.from(
        panel.querySelectorAll<HTMLElement>(
          'a[href], button:not([disabled]), input:not([disabled]), [tabindex]:not([tabindex="-1"])',
        ),
      ).filter((el) => el.offsetParent !== null || el === document.activeElement);
      const first = focusable[0];
      const last = focusable[focusable.length - 1];
      if (!first || !last) return;
      const active = document.activeElement;
      if (e.shiftKey && active === first) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && active === last) {
        e.preventDefault();
        first.focus();
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, [open, onClose]);

  if (!open) return null;

  return (
    <div
      role="dialog"
      aria-modal="true"
      aria-label={t('settings.title')}
      className="fixed inset-0 z-50 flex items-center justify-center"
      onClick={onClose}
    >
      <div className="absolute inset-0 bg-pc-base/70 backdrop-blur-sm" />
      <div
        ref={panelRef}
        className="relative flex flex-col w-full max-w-2xl mx-4 max-h-[90vh] rounded-[var(--radius-xl)] border border-pc-border bg-pc-base shadow-[var(--pc-shadow-md)] animate-fade-in"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex-shrink-0 flex items-center justify-between px-6 py-4 border-b border-pc-border">
          <div className="flex items-center gap-2.5">
            <Settings size={18} className="text-pc-accent-light" />
            <h2 className="text-sm font-semibold text-pc-text">{t('settings.title')}</h2>
          </div>
          <button
            type="button"
            onClick={onClose}
            aria-label="Close"
            className="h-11 w-11 -mr-2 rounded-[var(--radius-md)] flex items-center justify-center text-pc-text-muted transition-colors hover:bg-[var(--pc-hover)] hover:text-pc-text focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="flex-1 min-h-0 px-6 py-4 overflow-y-auto">
          {/* Tabs */}
          <div className="flex gap-2 mb-4">
            {tabs.map(tTab => (
              <button
                key={tTab.id}
                type="button"
                onClick={() => setTab(tTab.id)}
                className={chip(
                  tab === tTab.id,
                  'flex-1 rounded-[var(--radius-md)] px-3 py-2 text-xs font-medium flex items-center justify-center gap-1.5',
                )}
                aria-pressed={tab === tTab.id}
              >
                <tTab.icon size={13} />
                {tTab.label}
              </button>
            ))}
          </div>

          {/* Appearance Tab */}
          {tab === 'appearance' && (
            <>
              <SectionTitle>{t('settings.appearance')}</SectionTitle>

              {/* Theme Mode */}
              <div className="mb-3">
                <div className="text-xs mb-2 text-pc-text-secondary">{t('theme.mode')}</div>
                <div className="flex gap-1.5">
                  {themeOptions.map(opt => {
                    const Icon = opt.icon;
                    const active = theme === opt.value;
                    return (
                      <button
                        key={opt.value}
                        type="button"
                        onClick={() => setTheme(opt.value)}
                        aria-pressed={active}
                        className={chip(
                          active,
                          'flex-1 flex flex-col items-center gap-1.5 py-2 rounded-[var(--radius-md)] text-xs',
                        )}
                      >
                        {/* Theme preview swatch — keeps its literal colors (it previews a mode). */}
                        <div
                          className="w-8 h-5 rounded-md border"
                          style={{
                            background: opt.previewBg,
                            borderColor: opt.value === 'light' ? 'rgba(0,0,0,0.12)' : 'rgba(255,255,255,0.12)',
                          }}
                        >
                          <div className="flex items-center justify-center h-full">
                            <Icon size={10} style={{ color: opt.previewFg }} />
                          </div>
                        </div>
                        <span>{t(opt.labelKey)}</span>
                      </button>
                    );
                  })}
                </div>
              </div>

              {/* Accent Color */}
              <div className="mb-4">
                <div className="text-xs mb-2 text-pc-text-secondary">{t('theme.accent')}</div>
                {/* flex-wrap so swatches never overflow the modal on a phone; each
                    button carries a ≥44px hit area (min-h/min-w + padding) while the
                    visible swatch stays 28px. */}
                <div className="flex flex-wrap gap-1">
                  {accentOptions.map(opt => (
                    <button
                      key={opt.value}
                      type="button"
                      onClick={() => setAccent(opt.value)}
                      className="relative flex min-h-[44px] min-w-[44px] items-center justify-center rounded-full p-2 cursor-pointer focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)] focus-visible:ring-offset-2 focus-visible:ring-offset-pc-base"
                      aria-pressed={accent === opt.value}
                      aria-label={`${opt.value} accent`}
                    >
                      <span
                        className="flex h-7 w-7 items-center justify-center rounded-full transition-all"
                        style={{
                          backgroundColor: opt.color,
                          border: accent === opt.value ? `2px solid ${opt.color}` : '2px solid transparent',
                          boxShadow: accent === opt.value ? `0 0 8px ${opt.color}40` : 'none',
                        }}
                      >
                        {accent === opt.value && <Check size={14} style={{ color: 'white' }} />}
                      </span>
                    </button>
                  ))}
                </div>
              </div>
            </>
          )}

          {/* Themes Tab */}
          {tab === 'themes' && (
            <>
              <SectionTitle>Dark Themes</SectionTitle>
              <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-2 mb-4">
                {darkThemes.map(ct => (
                  <ThemePreviewCard
                    key={ct.id}
                    theme={ct}
                    active={colorTheme === ct.id}
                    onClick={() => setColorTheme(ct.id)}
                  />
                ))}
              </div>

              <SectionTitle>Light Themes</SectionTitle>
              <div className="grid grid-cols-2 sm:grid-cols-3 md:grid-cols-4 gap-2 mb-4">
                {lightThemes.map(ct => (
                  <ThemePreviewCard
                    key={ct.id}
                    theme={ct}
                    active={colorTheme === ct.id}
                    onClick={() => setColorTheme(ct.id)}
                  />
                ))}
              </div>

              {/* Active theme info */}
              <div className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface p-3 mt-2">
                <div className="flex items-center gap-2">
                  <Palette size={14} className="text-pc-accent" />
                  <span className="text-xs font-medium text-pc-text">
                    {colorThemes.find(ct => ct.id === colorTheme)?.name ?? 'Default Dark'}
                  </span>
                  <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-pc-accent/10 text-pc-accent-light">
                    Active
                  </span>
                </div>
              </div>
            </>
          )}

          {/* Typography Tab */}
          {tab === 'typography' && (
            <>
              <SectionTitle>{t('settings.typography')}</SectionTitle>

              {/* UI Font */}
              <div className="mb-4">
                <div className="flex items-center gap-2 text-xs mb-2 text-pc-text-secondary">
                  <Type size={14} />
                  {t('settings.fontUi')}
                </div>
                <div className="flex flex-wrap gap-1.5">
                  {uiFontOptions.map(opt => (
                    <button
                      key={opt.value}
                      type="button"
                      onClick={() => setUiFont(opt.value)}
                      className={chip(
                        uiFont === opt.value,
                        'flex items-center gap-2 px-3 py-2 rounded-[var(--radius-md)] text-xs',
                      )}
                      aria-pressed={uiFont === opt.value}
                    >
                      <span style={{ fontSize: '14px', fontFamily: uiFontStacks[opt.value] }}>{opt.sample}</span>
                      <span className="text-pc-text-faint" style={{ fontSize: '11px' }}>{opt.label}</span>
                    </button>
                  ))}
                </div>
              </div>

              {/* Mono Font */}
              <div className="mb-4">
                <div className="flex items-center gap-2 text-xs mb-2 text-pc-text-secondary">
                  <CaseSensitive size={14} />
                  {t('settings.fontMono')}
                </div>
                <div className="flex flex-wrap gap-1.5">
                  {monoFontOptions.map(opt => (
                    <button
                      key={opt.value}
                      type="button"
                      onClick={() => setMonoFont(opt.value)}
                      className={chip(
                        monoFont === opt.value,
                        'flex items-center gap-2 px-3 py-2 rounded-[var(--radius-md)] text-xs',
                      )}
                      aria-pressed={monoFont === opt.value}
                    >
                      <span style={{ fontSize: '14px', fontFamily: monoFontStacks[opt.value] }}>{opt.sample}</span>
                      <span className="text-pc-text-faint" style={{ fontSize: '11px' }}>{opt.label}</span>
                    </button>
                  ))}
                </div>
              </div>

              {/* UI Font Size */}
              <div className="mb-4">
                <div className="text-xs mb-2 text-pc-text-secondary">{t('settings.fontSize')}</div>
                <div className="flex gap-1.5 flex-wrap">
                  {uiSizes.map(size => (
                    <button
                      key={size}
                      type="button"
                      onClick={() => setUiFontSize(size)}
                      className={chip(
                        uiFontSize === size,
                        'px-3 py-1.5 rounded-[var(--radius-md)] text-xs',
                      )}
                      aria-pressed={uiFontSize === size}
                    >
                      {size}px
                    </button>
                  ))}
                </div>
              </div>

              {/* Mono Font Size */}
              <div className="mb-4">
                <div className="text-xs mb-2 text-pc-text-secondary">{t('settings.fontMonoSize')}</div>
                <div className="flex gap-1.5 flex-wrap">
                  {monoSizes.map(size => (
                    <button
                      key={size}
                      type="button"
                      onClick={() => setMonoFontSize(size)}
                      className={chip(
                        monoFontSize === size,
                        'px-3 py-1.5 rounded-[var(--radius-md)] text-xs',
                      )}
                      aria-pressed={monoFontSize === size}
                    >
                      {size}px
                    </button>
                  ))}
                </div>
              </div>

              {/* Preview */}
              <div className="rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface p-3">
                <div className="text-[11px] uppercase tracking-wide mb-2 text-pc-text-faint">
                  {t('settings.preview')}
                </div>
                <div
                  className="text-sm mb-2 text-pc-text"
                  style={{ fontFamily: 'var(--pc-font-ui)', fontSize: 'var(--pc-font-size)' }}
                >
                  {t('settings.previewText')}
                </div>
                <div
                  className="rounded-[var(--radius-md)] border border-pc-border bg-pc-code p-2 text-[13px] text-pc-text"
                  style={{ fontFamily: 'var(--pc-font-mono)', fontSize: 'var(--pc-font-size-mono)' }}
                >
                  const hello = 'ZeroClaw'; // typography preview
                </div>
              </div>
            </>
          )}
        </div>
      </div>
    </div>
  );
}
