import { useEffect, useMemo, useState } from 'react';
import { X, Settings, Sun, Moon, Monitor, Laptop, Check, Type, CaseSensitive } from 'lucide-react';
import { useTheme } from '@/hooks/useTheme';
import { t } from '@/lib/i18n';
import type { ThemeName, AccentColor, UiFont, MonoFont } from '@/contexts/ThemeContextDef';
import { uiFontStacks, monoFontStacks } from '@/contexts/ThemeContextDef';

const themeOptions: { value: ThemeName; icon: typeof Sun; labelKey: string }[] = [
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

interface Props {
  open: boolean;
  onClose: () => void;
}

export function SettingsModal({ open, onClose }: Props) {
  const {
    theme, accent, uiFont, monoFont, uiFontSize, monoFontSize,
    setTheme, setAccent, setUiFont, setMonoFont, setUiFontSize, setMonoFontSize,
  } = useTheme();

  type TabId = 'appearance' | 'typography';
  const [tab, setTab] = useState<TabId>('appearance');

  const tabs: { id: TabId; label: string }[] = useMemo(() => [
    { id: 'appearance', label: t('settings.tab.appearance') },
    { id: 'typography', label: t('settings.tab.typography') },
  ], []);

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
        className="relative w-full max-w-xl mx-4 rounded-3xl border shadow-2xl animate-fade-in"
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
        <div className="px-6 py-4 max-h-[60vh] overflow-y-auto">
          {/* Tabs */}
          <div className="flex gap-2 mb-4">
            {tabs.map(tTab => (
              <button
                key={tTab.id}
                onClick={() => setTab(tTab.id)}
                className="flex-1 rounded-xl border px-3 py-2 text-xs font-medium transition-colors"
                style={tab === tTab.id
                  ? { borderColor: 'var(--pc-accent-dim)', background: 'var(--pc-accent-glow)', color: 'var(--pc-accent-light)' }
                  : { borderColor: 'var(--pc-border)', color: 'var(--pc-text-muted)', background: 'transparent' }
                }
                onMouseEnter={(e) => { if (tab !== tTab.id) e.currentTarget.style.background = 'var(--pc-hover)'; }}
                onMouseLeave={(e) => { if (tab !== tTab.id) e.currentTarget.style.background = 'transparent'; }}
              >
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
