import { useEffect, useState } from 'react';
import { getMapKeys } from './api';

/** Resolve profile boundaries from configured keys, preserving dotted aliases. */
export function useModelProfileAliases(reference: string | undefined): string[] {
  const family = reference?.split('.', 1)[0];
  const [resolved, setResolved] = useState<{ family: string; aliases: string[] }>();
  useEffect(() => {
    if (!family) return;
    let active = true;
    void getMapKeys(`providers.models.${family}`).then(
      ({ keys }) => { if (active) setResolved({ family, aliases: keys }); },
      () => { if (active) setResolved({ family, aliases: [] }); },
    );
    return () => { active = false; };
  }, [family]);
  return resolved && resolved.family === family ? resolved.aliases : [];
}
