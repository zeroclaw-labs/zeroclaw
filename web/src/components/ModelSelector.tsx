import { useState, useEffect, useRef, useCallback } from 'react';
import { ChevronDown, Cpu } from 'lucide-react';
import { getStatus, getConfig, putConfig } from '@/lib/api';

interface ModelRoute {
  provider: string;
  model: string;
}

/** Color dot per provider for quick visual identification. */
const PROVIDER_COLORS: Record<string, string> = {
  openai: '#74aa9c',
  anthropic: '#c19a6b',
  google: '#4285f4',
  groq: '#f55036',
  ollama: '#999999',
  openrouter: '#6366f1',
  together: '#ff6f00',
  mistral: '#ff7000',
  deepseek: '#5b6ef5',
};

function providerDotColor(provider: string): string {
  const key = provider.toLowerCase();
  return PROVIDER_COLORS[key] ?? 'var(--pc-text-muted)';
}

/**
 * Parse [[model_routes]] entries from a TOML config string.
 *
 * This is intentionally a minimal regex-based parser rather than a full TOML
 * library — it only needs to pull provider/model strings from each
 * [[model_routes]] block.
 */
function parseModelRoutes(toml: string): ModelRoute[] {
  const routes: ModelRoute[] = [];
  // Split into [[model_routes]] blocks
  const blocks = toml.split(/\[\[model_routes\]\]/);
  for (let i = 1; i < blocks.length; i++) {
    const block = blocks[i]!;
    // Stop at next top-level section (single or double bracket that isn't model_routes)
    const nextSection = new RegExp('^\\[(?!\\[model_routes\\])', 'm');
    const sectionEnd = block.search(nextSection);
    const content = sectionEnd === -1 ? block : block.slice(0, sectionEnd);

    const providerMatch = content.match(/^\s*provider\s*=\s*"([^"]+)"/m);
    const modelMatch = content.match(/^\s*model\s*=\s*"([^"]+)"/m);
    if (providerMatch && modelMatch) {
      routes.push({ provider: providerMatch[1]!, model: modelMatch[1]! });
    }
  }
  return routes;
}

/**
 * Update the default_provider and default_model in a TOML config string.
 * Creates the keys under [ai] if they don't already exist.
 */
function updateDefaultModel(toml: string, provider: string, model: string): string {
  let updated = toml;

  // Replace or insert default_provider
  if (/^\s*default_provider\s*=/m.test(updated)) {
    updated = updated.replace(
      /^(\s*default_provider\s*=\s*).*$/m,
      `$1"${provider}"`,
    );
  } else {
    // Insert after [ai] section header if it exists, otherwise append
    const aiMatch = updated.match(/^\[ai\]\s*$/m);
    if (aiMatch && aiMatch.index !== undefined) {
      const insertPos = aiMatch.index + aiMatch[0].length;
      updated = `${updated.slice(0, insertPos)}\ndefault_provider = "${provider}"${updated.slice(insertPos)}`;
    } else {
      updated += `\n[ai]\ndefault_provider = "${provider}"\n`;
    }
  }

  // Replace or insert default_model
  if (/^\s*default_model\s*=/m.test(updated)) {
    updated = updated.replace(
      /^(\s*default_model\s*=\s*).*$/m,
      `$1"${model}"`,
    );
  } else {
    const providerLine = updated.match(/^\s*default_provider\s*=.*$/m);
    if (providerLine && providerLine.index !== undefined) {
      const insertPos = providerLine.index + providerLine[0].length;
      updated = `${updated.slice(0, insertPos)}\ndefault_model = "${model}"${updated.slice(insertPos)}`;
    }
  }

  return updated;
}

export default function ModelSelector() {
  const [currentProvider, setCurrentProvider] = useState<string | null>(null);
  const [currentModel, setCurrentModel] = useState<string>('');
  const [routes, setRoutes] = useState<ModelRoute[]>([]);
  const [open, setOpen] = useState(false);
  const [switching, setSwitching] = useState(false);
  const dropdownRef = useRef<HTMLDivElement>(null);

  // Fetch current status + available routes
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const [status, configToml] = await Promise.all([getStatus(), getConfig()]);
        if (cancelled) return;
        setCurrentProvider(status.provider);
        setCurrentModel(status.model);
        setRoutes(parseModelRoutes(configToml));
      } catch {
        // Non-critical — selector just stays empty
      }
    })();
    return () => { cancelled = true; };
  }, []);

  // Close dropdown on outside click
  useEffect(() => {
    const handleClick = (e: MouseEvent) => {
      if (dropdownRef.current && !dropdownRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    if (open) {
      document.addEventListener('mousedown', handleClick);
    }
    return () => document.removeEventListener('mousedown', handleClick);
  }, [open]);

  const handleSelect = useCallback(
    async (route: ModelRoute) => {
      if (route.provider === currentProvider && route.model === currentModel) {
        setOpen(false);
        return;
      }
      setSwitching(true);
      setOpen(false);
      try {
        const configToml = await getConfig();
        const updated = updateDefaultModel(configToml, route.provider, route.model);
        await putConfig(updated);
        setCurrentProvider(route.provider);
        setCurrentModel(route.model);
      } catch {
        // Failed to update — keep current selection
      } finally {
        setSwitching(false);
      }
    },
    [currentProvider, currentModel],
  );

  const displayLabel = currentModel
    ? `${currentProvider ?? 'unknown'}/${currentModel}`
    : 'Loading...';

  return (
    <div ref={dropdownRef} className="relative inline-block">
      {/* Trigger */}
      <button
        type="button"
        onClick={() => setOpen((prev) => !prev)}
        disabled={switching}
        className="flex items-center gap-2 rounded-xl px-3 py-1.5 text-xs font-medium transition-all border"
        style={{
          background: 'var(--pc-bg-elevated)',
          borderColor: 'var(--pc-border)',
          color: 'var(--pc-text-muted)',
          opacity: switching ? 0.6 : 1,
        }}
        onMouseEnter={(e) => {
          e.currentTarget.style.borderColor = 'var(--pc-accent-dim)';
          e.currentTarget.style.color = 'var(--pc-text-primary)';
        }}
        onMouseLeave={(e) => {
          e.currentTarget.style.borderColor = 'var(--pc-border)';
          e.currentTarget.style.color = 'var(--pc-text-muted)';
        }}
      >
        {currentProvider && (
          <span
            className="w-2 h-2 rounded-full shrink-0"
            style={{ background: providerDotColor(currentProvider) }}
          />
        )}
        <Cpu className="h-3.5 w-3.5 shrink-0" />
        <span className="truncate max-w-[180px]">{displayLabel}</span>
        <ChevronDown className="h-3 w-3 shrink-0 transition-transform" style={{ transform: open ? 'rotate(180deg)' : undefined }} />
      </button>

      {/* Dropdown */}
      {open && routes.length > 0 && (
        <div
          className="absolute top-full left-0 mt-1 rounded-xl border shadow-lg z-50 min-w-[220px] py-1 animate-fade-in"
          style={{
            background: 'var(--pc-bg-elevated)',
            borderColor: 'var(--pc-border)',
          }}
        >
          {routes.map((route) => {
            const isActive = route.provider === currentProvider && route.model === currentModel;
            return (
              <button
                key={`${route.provider}/${route.model}`}
                type="button"
                onClick={() => handleSelect(route)}
                className="flex items-center gap-2 w-full px-3 py-2 text-xs text-left transition-all"
                style={{
                  color: isActive ? 'var(--pc-accent-light)' : 'var(--pc-text-muted)',
                  background: isActive ? 'var(--pc-accent-glow)' : 'transparent',
                }}
                onMouseEnter={(e) => {
                  if (!isActive) {
                    e.currentTarget.style.background = 'var(--pc-hover)';
                    e.currentTarget.style.color = 'var(--pc-text-secondary)';
                  }
                }}
                onMouseLeave={(e) => {
                  if (!isActive) {
                    e.currentTarget.style.background = 'transparent';
                    e.currentTarget.style.color = 'var(--pc-text-muted)';
                  }
                }}
              >
                <span
                  className="w-2 h-2 rounded-full shrink-0"
                  style={{ background: providerDotColor(route.provider) }}
                />
                <span className="truncate">
                  {route.provider}/{route.model}
                </span>
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
