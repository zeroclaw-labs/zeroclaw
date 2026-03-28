import { useEffect, useMemo, useState } from 'react';
import { X, Settings, Sun, Moon, Monitor, Laptop, Check, Type, CaseSensitive, Palette } from 'lucide-react';
import { useTheme } from '@/hooks/useTheme';
import { t } from '@/lib/i18n';
import type { AccentColor, UiFont, MonoFont, ThemeMode } from '@/contexts/ThemeContextDef';
import { uiFontStacks, monoFontStacks } from '@/contexts/ThemeContextDef';
import { colorThemes } from '@/contexts/colorThemes';

const themeOptions: { value: ThemeMode; icon: typeof Sun; labelKey: string }[] = [
  { value: 'system', icon: Laptop, labelKey: 'theme.system' },
  { value: 'dark', icon: Moon, labelKey: 'theme.dark' },
  { value: 'light', icon: Sun, labelKey: 'theme.light' },
  { value: 'oled', icon: Monitor, labelKey: 'theme.oled' },
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

function SectionTitle({ children }: { children: React.ReactNode }) {
  return (
    <div
      className="text-[10px] uppercase tracking-wider mb-2 mt-5 first:mt-0"
      style={{ color: 'var(--pc-text-faint)', fontWeight: 600 }}
    >
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
      onClick={onClick}
      className="flex flex-col gap-1.5 p-2 rounded-xl border transition-all text-left group"
      style={{
        borderColor: active ? 'var(--pc-accent)' : 'var(--pc-border)',
        background: active ? 'var(--pc-accent-glow)' : 'transparent',
        boxShadow: active ? '0 0 12px var(--pc-accent-glow)' : 'none',
        minWidth: '110px',
      }}
      aria-pressed={active}
    >
      {/* Mini terminal */}
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
        {active && <Check size={10} style={{ color: 'var(--pc-accent)' }} />}
        <span
          className="text-[10px] font-medium truncate"
          style={{ color: active ? 'var(--pc-accent-light)' : 'var(--pc-text-muted)' }}
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

  const tabs: { id: TabId; label: string; icon: typeof Palette }[] = useMemo(() => [
    { id: 'appearance', label: t('settings.tab.appearance'), icon: Settings },
    { id: 'themes', label: 'Themes', icon: Palette },
    { id: 'typography', label: t('settings.tab.typography'), icon: Type },
  ], []);

  // Group themes by scheme for the themes tab
  const darkThemes = useMemo(() => colorThemes.filter(ct => ct.scheme === 'dark'), []);
  const lightThemes = useMemo(() => colorThemes.filter(ct => ct.scheme === 'light'), []);

  useEffect(() => {
    if (!open) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
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
      <div className="absolute inset-0" style={{ background: 'rgba(0,0,0,0.6)', backdropFilter: 'blur(8px)' }} />
      <div
        className="relative w-full max-w-2xl mx-4 rounded-3xl border shadow-2xl animate-fade-in"
        style={{ background: 'var(--pc-bg-base)', borderColor: 'var(--pc-border)' }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div
          className="flex items-center justify-between px-6 py-4 border-b"
          style={{ borderColor: 'var(--pc-border)' }}
        >
          <div className="flex items-center gap-2.5">
            <Settings size={18} style={{ color: 'var(--pc-accent-light)' }} />
            <h2 className="text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>{t('settings.title')}</h2>
          </div>
          <button
            onClick={onClose}
            className="h-8 w-8 rounded-xl flex items-center justify-center transition-colors"
            style={{ color: 'var(--pc-text-muted)', background: 'transparent', border: 'none', cursor: 'pointer' }}
            onMouseEnter={(e) => { e.currentTarget.style.color = 'var(--pc-text-primary)'; e.currentTarget.style.background = 'var(--pc-hover)'; }}
            onMouseLeave={(e) => { e.currentTarget.style.color = 'var(--pc-text-muted)'; e.currentTarget.style.background = 'transparent'; }}
            aria-label="Close"
          >
            <X size={16} />
          </button>
        </div>

        {/* Body */}
        <div className="px-6 py-4 max-h-[65vh] overflow-y-auto">
          {/* Tabs */}
          <div className="flex gap-2 mb-4">
            {tabs.map(tTab => (
              <button
                key={tTab.id}
                onClick={() => setTab(tTab.id)}
                className="flex-1 rounded-xl border px-3 py-2 text-xs font-medium transition-colors flex items-center justify-center gap-1.5"
                style={tab === tTab.id
                  ? { borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }
                  : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)', background: 'transparent' }
                }
                onMouseEnter={(e) => { if (tab !== tTab.id) e.currentTarget.style.background = 'var(--pc-hover)'; }}
                onMouseLeave={(e) => { if (tab !== tTab.id) e.currentTarget.style.background = 'transparent'; }}
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
                <div className="text-xs mb-2" style={{ color: 'var(--pc-text-secondary)' }}>{t('theme.mode')}</div>
                <div className="flex gap-1.5">
                  {themeOptions.map(opt => {
                    const Icon = opt.icon;
                    const active = theme === opt.value;
                    return (
                      <button
                        key={opt.value}
                        onClick={() => setTheme(opt.value)}
                        aria-pressed={active}
                        className="flex-1 flex flex-col items-center gap-1 py-2 rounded-xl border text-xs transition-all"
                        style={active
                          ? { borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }
                          : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)', background: 'transparent' }
                        }
                        onMouseEnter={(e) => { if (!active) e.currentTarget.style.background = 'var(--pc-hover)'; }}
                        onMouseLeave={(e) => { if (!active) e.currentTarget.style.background = 'transparent'; }}
                      >
                        <Icon size={16} />
                        <span>{t(opt.labelKey)}</span>
                      </button>
                    );
                  })}
                </div>
              </div>

              {/* Accent Color */}
              <div className="mb-4">
                <div className="text-xs mb-2" style={{ color: 'var(--pc-text-secondary)' }}>{t('theme.accent')}</div>
                <div className="flex gap-2">
                  {accentOptions.map(opt => (
                    <button
                      key={opt.value}
                      onClick={() => setAccent(opt.value)}
                      className="relative h-7 w-7 rounded-full transition-all flex items-center justify-center"
                      style={{
                        backgroundColor: opt.color,
                        border: accent === opt.value ? `2px solid ${opt.color}` : '2px solid transparent',
                        boxShadow: accent === opt.value ? `0 0 8px ${opt.color}40` : 'none',
                      }}
                      aria-pressed={accent === opt.value}
                      aria-label={`${opt.value} accent`}
                    >
                      {accent === opt.value && <Check size={14} style={{ color: 'white' }} />}
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
              <div className="grid grid-cols-3 sm:grid-cols-4 gap-2 mb-4">
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
              <div className="grid grid-cols-3 sm:grid-cols-4 gap-2 mb-4">
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
              <div
                className="rounded-2xl border p-3 mt-2"
                style={{ background: 'var(--pc-bg-surface)', borderColor: 'var(--pc-border)' }}
              >
                <div className="flex items-center gap-2">
                  <Palette size={14} style={{ color: 'var(--pc-accent)' }} />
                  <span className="text-xs font-medium" style={{ color: 'var(--pc-text-primary)' }}>
                    {colorThemes.find(ct => ct.id === colorTheme)?.name ?? 'Default Dark'}
                  </span>
                  <span
                    className="text-[10px] px-1.5 py-0.5 rounded-full"
                    style={{ background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }}
                  >
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
                <div className="flex items-center gap-2 text-xs mb-2" style={{ color: 'var(--pc-text-secondary)' }}>
                  <Type size={14} />
                  {t('settings.fontUi')}
                </div>
                <div className="flex flex-wrap gap-1.5">
                  {uiFontOptions.map(opt => (
                    <button
                      key={opt.value}
                      onClick={() => setUiFont(opt.value)}
                      className="flex items-center gap-2 px-3 py-2 rounded-xl border text-xs transition-all"
                      style={uiFont === opt.value
                        ? { borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }
                        : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)', background: 'transparent' }
                      }
                      onMouseEnter={(e) => { if (uiFont !== opt.value) e.currentTarget.style.background = 'var(--pc-hover)'; }}
                      onMouseLeave={(e) => { if (uiFont !== opt.value) e.currentTarget.style.background = 'transparent'; }}
                    >
                      <span style={{ fontSize: '14px', fontFamily: uiFontStacks[opt.value] }}>{opt.sample}</span>
                      <span style={{ fontSize: '11px', color: 'var(--pc-text-faint)' }}>{opt.label}</span>
                    </button>
                  ))}
                </div>
              </div>

              {/* Mono Font */}
              <div className="mb-4">
                <div className="flex items-center gap-2 text-xs mb-2" style={{ color: 'var(--pc-text-secondary)' }}>
                  <CaseSensitive size={14} />
                  {t('settings.fontMono')}
                </div>
                <div className="flex flex-wrap gap-1.5">
                  {monoFontOptions.map(opt => (
                    <button
                      key={opt.value}
                      onClick={() => setMonoFont(opt.value)}
                      className="flex items-center gap-2 px-3 py-2 rounded-xl border text-xs transition-all"
                      style={monoFont === opt.value
                        ? { borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }
                        : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)', background: 'transparent' }
                      }
                      onMouseEnter={(e) => { if (monoFont !== opt.value) e.currentTarget.style.background = 'var(--pc-hover)'; }}
                      onMouseLeave={(e) => { if (monoFont !== opt.value) e.currentTarget.style.background = 'transparent'; }}
                    >
                      <span style={{ fontSize: '14px', fontFamily: monoFontStacks[opt.value] }}>{opt.sample}</span>
                      <span style={{ fontSize: '11px', color: 'var(--pc-text-faint)' }}>{opt.label}</span>
                    </button>
                  ))}
                </div>
              </div>

              {/* UI Font Size */}
              <div className="mb-4">
                <div className="text-xs mb-2" style={{ color: 'var(--pc-text-secondary)' }}>{t('settings.fontSize')}</div>
                <div className="flex gap-1.5 flex-wrap">
                  {uiSizes.map(size => (
                    <button
                      key={size}
                      onClick={() => setUiFontSize(size)}
                      className="px-3 py-1.5 rounded-lg border text-xs transition-all"
                      style={uiFontSize === size
                        ? { borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }
                        : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)', background: 'transparent' }
                      }
                      onMouseEnter={(e) => { if (uiFontSize !== size) e.currentTarget.style.background = 'var(--pc-hover)'; }}
                      onMouseLeave={(e) => { if (uiFontSize !== size) e.currentTarget.style.background = 'transparent'; }}
                    >
                      {size}px
                    </button>
                  ))}
                </div>
              </div>

              {/* Mono Font Size */}
              <div className="mb-4">
                <div className="text-xs mb-2" style={{ color: 'var(--pc-text-secondary)' }}>{t('settings.fontMonoSize')}</div>
                <div className="flex gap-1.5 flex-wrap">
                  {monoSizes.map(size => (
                    <button
                      key={size}
                      onClick={() => setMonoFontSize(size)}
                      className="px-3 py-1.5 rounded-lg border text-xs transition-all"
                      style={monoFontSize === size
                        ? { borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }
                        : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)', background: 'transparent' }
                      }
                      onMouseEnter={(e) => { if (monoFontSize !== size) e.currentTarget.style.background = 'var(--pc-hover)'; }}
                      onMouseLeave={(e) => { if (monoFontSize !== size) e.currentTarget.style.background = 'transparent'; }}
                    >
                      {size}px
                    </button>
                  ))}
                </div>
              </div>

              {/* Preview */}
              <div
                className="rounded-2xl border p-3"
                style={{ background: 'var(--pc-bg-surface)', borderColor: 'var(--pc-border)' }}
              >
                <div
                  className="text-[11px] uppercase tracking-wide mb-2"
                  style={{ color: 'var(--pc-text-faint)' }}
                >
                  {t('settings.preview')}
                </div>
                <div
                  className="text-sm mb-2"
                  style={{ color: 'var(--pc-text-primary)', fontFamily: 'var(--pc-font-ui)', fontSize: 'var(--pc-font-size)' }}
                >
                  {t('settings.previewText')}
                </div>
                <div
                  className="rounded-xl border p-2 text-[13px]"
                  style={{ fontFamily: 'var(--pc-font-mono)', fontSize: 'var(--pc-font-size-mono)', color: 'var(--pc-text-primary)', borderColor: 'var(--pc-border)', background: 'var(--pc-bg-code)' }}
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
