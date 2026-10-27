import { useId } from 'react';
import { useLayoutPreference, type LayoutMode } from '@/contexts/LayoutPreference';
import { t } from '@/lib/i18n';

export default function LayoutChoice() {
  const { layout, setLayout } = useLayoutPreference();
  const id = useId();
  return (
    <fieldset className="space-y-3">
      <legend className="text-sm font-medium">{t('layout.title')}</legend>
      <p className="text-xs text-pc-text-muted">{t('layout.hint')}</p>
      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        {(['classic', 'focused'] as LayoutMode[]).map((mode) => (
          <label key={mode} className={`flex cursor-pointer items-start gap-3 rounded-lg border p-3 ${layout === mode ? 'border-pc-accent bg-pc-accent/5' : 'border-pc-border'}`}>
            <input type="radio" name={id} value={mode} checked={layout === mode} onChange={() => setLayout(mode)} className="mt-1 accent-pc-accent" />
            <span><span className="block text-sm font-medium">{t(`layout.${mode}`)}</span><span className="mt-1 block text-xs text-pc-text-muted">{t(`layout.${mode}_hint`)}</span></span>
          </label>
        ))}
      </div>
    </fieldset>
  );
}
