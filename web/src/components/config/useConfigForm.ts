import { useState, useCallback, useRef, useEffect } from 'react';
import { parse, stringify } from 'smol-toml';
import { getConfig, putConfig } from '@/lib/api';

const MASKED = '***MASKED***';

type ParsedConfig = Record<string, unknown>;

function deepClone<T>(obj: T): T {
  return JSON.parse(JSON.stringify(obj));
}

/** Recursively scan for MASKED strings and collect their dotted paths. */
function scanMasked(obj: unknown, prefix: string, out: Set<string>) {
  if (obj === null || obj === undefined) return;
  if (typeof obj === 'string' && obj === MASKED) {
    out.add(prefix);
    return;
  }
  if (Array.isArray(obj)) {
    obj.forEach((item, i) => {
      scanMasked(item, `${prefix}.${i}`, out);
    });
    return;
  }
  if (typeof obj === 'object') {
    for (const [k, v] of Object.entries(obj as Record<string, unknown>)) {
      scanMasked(v, prefix ? `${prefix}.${k}` : k, out);
    }
  }
}

/** Navigate into an object by dotted path segments, returning the value. */
function getNestedValue(obj: unknown, segments: string[]): unknown {
  let current: unknown = obj;
  for (const seg of segments) {
    if (current === null || current === undefined || typeof current !== 'object') return undefined;
    current = (current as Record<string, unknown>)[seg];
  }
  return current;
}

/** Set a value in an object by dotted path segments, creating intermediates. */
function setNestedValue(obj: Record<string, unknown>, segments: string[], value: unknown) {
  if (segments.length === 0) return;
  let current: Record<string, unknown> = obj;
  for (let i = 0; i < segments.length - 1; i++) {
    const seg: string = segments[i]!;
    if (current[seg] === undefined || current[seg] === null || typeof current[seg] !== 'object') {
      current[seg] = {};
    }
    current = current[seg] as Record<string, unknown>;
  }
  const lastSeg: string = segments[segments.length - 1]!;
  if (value === undefined || value === '') {
    delete current[lastSeg];
  } else {
    current[lastSeg] = value;
  }
}

export type EditorMode = 'form' | 'raw';

export interface ConfigFormState {
  loading: boolean;
  saving: boolean;
  error: string | null;
  success: string | null;
  mode: EditorMode;
  rawToml: string;
  parsed: ParsedConfig;
  maskedPaths: Set<string>;
  dirtyPaths: Set<string>;
  setMode: (mode: EditorMode) => boolean;
  getFieldValue: (sectionPath: string, fieldKey: string) => unknown;
  setFieldValue: (sectionPath: string, fieldKey: string, value: unknown) => void;
  isFieldMasked: (sectionPath: string, fieldKey: string) => boolean;
  isFieldDirty: (sectionPath: string, fieldKey: string) => boolean;
  setRawToml: (raw: string) => void;
  save: () => Promise<void>;
  reload: () => Promise<void>;
  clearMessages: () => void;
}

export function useConfigForm(): ConfigFormState {
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [mode, setModeState] = useState<EditorMode>('form');
  const [rawToml, setRawTomlState] = useState('');
  const [parsed, setParsed] = useState<ParsedConfig>({});
  const maskedPathsRef = useRef<Set<string>>(new Set());
  const dirtyPathsRef = useRef<Set<string>>(new Set());
  const successTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [, forceRender] = useState(0);

  const loadConfig = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const data = await getConfig();
      const raw = typeof data === 'string' ? data : JSON.stringify(data, null, 2);
      setRawTomlState(raw);

      try {
        const obj = parse(raw) as ParsedConfig;
        setParsed(obj);
        const masked = new Set<string>();
        scanMasked(obj, '', masked);
        maskedPathsRef.current = masked;
      } catch {
        // If TOML parse fails, start in raw mode
        setParsed({});
        maskedPathsRef.current = new Set();
        setModeState('raw');
      }

      dirtyPathsRef.current = new Set();
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to load configuration');
    } finally {
      setLoading(false);
    }
  }, []);

  // Load once on mount.
  const hasLoaded = useRef(false);
  useEffect(() => {
    if (!hasLoaded.current) {
      hasLoaded.current = true;
      void loadConfig();
    }
  }, [loadConfig]);

  useEffect(() => {
    return () => {
      if (successTimeoutRef.current) {
        clearTimeout(successTimeoutRef.current);
      }
    };
  }, []);

  const fieldPath = (sectionPath: string, fieldKey: string) =>
    sectionPath ? `${sectionPath}.${fieldKey}` : fieldKey;

  const fieldSegments = (sectionPath: string, fieldKey: string) => {
    const full = fieldPath(sectionPath, fieldKey);
    return full.split('.').filter(Boolean);
  };

  const getFieldValue = useCallback(
    (sectionPath: string, fieldKey: string): unknown => {
      const segments = fieldSegments(sectionPath, fieldKey);
      return getNestedValue(parsed, segments);
    },
    [parsed],
  );

  const setFieldValue = useCallback(
    (sectionPath: string, fieldKey: string, value: unknown) => {
      const fp = fieldPath(sectionPath, fieldKey);
      const segments = fieldSegments(sectionPath, fieldKey);

      setParsed((prev) => {
        const next = deepClone(prev);
        setNestedValue(next, segments, value);
        return next;
      });

      dirtyPathsRef.current.add(fp);
      forceRender((n) => n + 1);
    },
    [],
  );

  const isFieldMasked = useCallback(
    (sectionPath: string, fieldKey: string): boolean => {
      const fp = fieldPath(sectionPath, fieldKey);
      return maskedPathsRef.current.has(fp) && !dirtyPathsRef.current.has(fp);
    },
    [],
  );

  const isFieldDirty = useCallback(
    (sectionPath: string, fieldKey: string): boolean => {
      const fp = fieldPath(sectionPath, fieldKey);
      return dirtyPathsRef.current.has(fp);
    },
    [],
  );

  const syncFormToRaw = useCallback((): string => {
    try {
      const toml = stringify(parsed);
      return toml;
    } catch {
      return rawToml;
    }
  }, [parsed, rawToml]);

  const syncRawToForm = useCallback(
    (raw: string): boolean => {
      try {
        const obj = parse(raw) as ParsedConfig;
        setParsed(obj);
        // Re-scan masked paths from fresh parse, preserving dirty overrides
        const masked = new Set<string>();
        scanMasked(obj, '', masked);
        maskedPathsRef.current = masked;
        return true;
      } catch {
        return false;
      }
    },
    [],
  );

  const setMode = useCallback(
    (newMode: EditorMode): boolean => {
      if (newMode === mode) return true;

      if (newMode === 'raw') {
        // form → raw: serialize parsed to TOML
        const toml = syncFormToRaw();
        setRawTomlState(toml);
        setModeState('raw');
        return true;
      } else {
        // raw → form: parse TOML
        if (syncRawToForm(rawToml)) {
          setModeState('form');
          return true;
        } else {
          setError('Invalid TOML syntax. Fix errors before switching to Form view.');
          return false;
        }
      }
    },
    [mode, syncFormToRaw, syncRawToForm, rawToml],
  );

  const setRawToml = useCallback((raw: string) => {
    setRawTomlState(raw);
  }, []);

  const save = useCallback(async () => {
    setSaving(true);
    setError(null);
    setSuccess(null);
    if (successTimeoutRef.current) {
      clearTimeout(successTimeoutRef.current);
    }

    try {
      let toml: string;
      if (mode === 'form') {
        toml = syncFormToRaw();
      } else {
        toml = rawToml;
      }
      await putConfig(toml);
      setSuccess('Configuration saved successfully.');

      // Auto-dismiss success after 4 seconds
      successTimeoutRef.current = setTimeout(() => setSuccess(null), 4000);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save configuration');
    } finally {
      setSaving(false);
    }
  }, [mode, syncFormToRaw, rawToml]);

  const reload = useCallback(async () => {
    await loadConfig();
  }, [loadConfig]);

  const clearMessages = useCallback(() => {
    setError(null);
    setSuccess(null);
    if (successTimeoutRef.current) {
      clearTimeout(successTimeoutRef.current);
      successTimeoutRef.current = null;
    }
  }, []);

  return {
    loading,
    saving,
    error,
    success,
    mode,
    rawToml,
    parsed,
    maskedPaths: maskedPathsRef.current,
    dirtyPaths: dirtyPathsRef.current,
    setMode,
    getFieldValue,
    setFieldValue,
    isFieldMasked,
    isFieldDirty,
    setRawToml,
    save,
    reload,
    clearMessages,
  };
}
