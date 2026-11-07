import { useEffect, useState } from 'react';
import { getMapKeys, getProviderRegistry } from './api';

/** Resolve profile boundaries from configured keys, preserving dotted aliases. */
export function useModelProfileAliases(reference: string | undefined): string[] {
  const family = reference?.split('.', 1)[0];
  const [resolved, setResolved] = useState<{ reference: string | undefined; aliases: string[] }>();
  useEffect(() => {
    if (!family) return;
    let active = true;
    let request = 0;
    const load = async () => {
      const registry = await getProviderRegistry();
      // Canonical references are opaque exact keys, even when their prefix
      // happens to name a legacy family/alias that also exists.
      if (reference && Object.prototype.hasOwnProperty.call(registry.models, reference)) return { keys: [] };
      return getMapKeys(`providers.models.${family}`);
    };
    const refresh = () => {
      const current = ++request;
      setResolved(undefined);
      void load().then(
        ({ keys }) => { if (active && current === request) setResolved({ reference, aliases: keys }); },
        () => { if (active && current === request) setResolved({ reference, aliases: [] }); },
      );
    };
    refresh();
    window.addEventListener('zeroclaw-config-mutated', refresh);
    return () => { active = false; window.removeEventListener('zeroclaw-config-mutated', refresh); };
  }, [family, reference]);
  return resolved && resolved.reference === reference ? resolved.aliases : [];
}
