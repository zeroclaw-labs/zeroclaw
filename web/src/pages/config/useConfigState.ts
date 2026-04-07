import { useState, useEffect, useCallback } from 'react';
import { parse, stringify } from 'smol-toml';
import { getConfig, putConfig } from '@/lib/api';

export type ConfigMode = 'form' | 'advanced';

// Deep-get a value by dot-separated path
function getPath(obj: Record<string, unknown>, path: string): unknown {
  const keys = path.split('.');
  let cur: unknown = obj;
  for (const k of keys) {
    if (cur == null || typeof cur !== 'object') return undefined;
    cur = (cur as Record<string, unknown>)[k];
  }
  return cur;
}

// Immutable deep-set by dot-separated path
function setPath(obj: Record<string, unknown>, path: string, value: unknown): Record<string, unknown> {
  const keys = path.split('.');
  if (keys.length === 0) return obj;

  const clone = { ...obj };
  const first = keys[0]!;
  if (keys.length === 1) {
    clone[first] = value;
    return clone;
  }

  const rest = keys.slice(1);
  const child = (typeof clone[first] === 'object' && clone[first] !== null)
    ? { ...(clone[first] as Record<string, unknown>) }
    : {};
  clone[first] = setPath(child, rest.join('.'), value);
  return clone;
}

export function useConfigState() {
  const [rawToml, setRawToml] = useState('');
  const [parsedConfig, setParsedConfig] = useState<Record<string, unknown>>({});
  const [mode, setMode] = useState<ConfigMode>('form');
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [parseError, setParseError] = useState<string | null>(null);

  // Initial fetch
  useEffect(() => {
    getConfig()
      .then((data) => {
        const toml = typeof data === 'string' ? data : JSON.stringify(data, null, 2);
        setRawToml(toml);
        try {
          setParsedConfig(parse(toml) as Record<string, unknown>);
        } catch {
          // If initial parse fails, start in advanced mode
          setMode('advanced');
        }
      })
      .catch((err) => setError(err.message))
      .finally(() => setLoading(false));
  }, []);

  // Auto-dismiss success
  useEffect(() => {
    if (!success) return;
    const timer = setTimeout(() => setSuccess(null), 4000);
    return () => clearTimeout(timer);
  }, [success]);

  const updateField = useCallback((path: string, value: unknown) => {
    setParsedConfig((prev) => {
      const next = setPath(prev, path, value);
      setDirty(true);
      return next;
    });
  }, []);

  const getField = useCallback((path: string): unknown => {
    return getPath(parsedConfig, path);
  }, [parsedConfig]);

  const switchMode = useCallback((newMode: ConfigMode) => {
    if (newMode === mode) return;
    setParseError(null);

    if (newMode === 'advanced') {
      // Form → Advanced: stringify current parsed config
      try {
        setRawToml(stringify(parsedConfig));
      } catch (e) {
        setParseError(e instanceof Error ? e.message : 'Failed to serialize config');
        return;
      }
    } else {
      // Advanced → Form: parse current TOML string
      try {
        setParsedConfig(parse(rawToml) as Record<string, unknown>);
      } catch (e) {
        setParseError(e instanceof Error ? e.message : 'Invalid TOML — fix errors before switching to form view');
        return;
      }
    }
    setMode(newMode);
  }, [mode, parsedConfig, rawToml]);

  const updateRawToml = useCallback((toml: string) => {
    setRawToml(toml);
    setDirty(true);
  }, []);

  const save = useCallback(async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    try {
      let toml: string;
      if (mode === 'form') {
        toml = stringify(parsedConfig);
      } else {
        toml = rawToml;
      }
      await putConfig(toml);
      setSuccess('Configuration saved successfully');
      setDirty(false);
      // Re-sync both representations
      if (mode === 'form') {
        setRawToml(toml);
      } else {
        try { setParsedConfig(parse(toml) as Record<string, unknown>); } catch { /* ignore */ }
      }
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to save configuration');
    } finally {
      setSaving(false);
    }
  }, [mode, parsedConfig, rawToml]);

  const reload = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await getConfig();
      const toml = typeof data === 'string' ? data : JSON.stringify(data, null, 2);
      setRawToml(toml);
      try { setParsedConfig(parse(toml) as Record<string, unknown>); } catch { /* ignore */ }
      setDirty(false);
    } catch (err: unknown) {
      setError(err instanceof Error ? err.message : 'Failed to reload configuration');
    } finally {
      setLoading(false);
    }
  }, []);

  return {
    parsedConfig,
    rawToml,
    mode,
    loading,
    saving,
    dirty,
    error,
    success,
    parseError,
    updateField,
    getField,
    switchMode,
    updateRawToml,
    save,
    reload,
    setError,
    setSuccess,
  };
}
