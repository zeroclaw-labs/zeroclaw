import { useState, useEffect, useCallback, useRef } from 'react';
import {
  getStatus,
  getTools,
  getCronJobs,
  getIntegrations,
  getMemory,
  getCost,
  getCliTools,
  getHealth,
  runDoctor,
} from '../lib/api';
import type {
  StatusResponse,
  ToolSpec,
  CronJob,
  Integration,
  MemoryEntry,
  CostSummary,
  CliTool,
  HealthSnapshot,
  DiagResult,
} from '../types/api';

// ---------------------------------------------------------------------------
// Generic async-data hook
// ---------------------------------------------------------------------------

interface UseApiResult<T> {
  data: T | null;
  error: Error | null;
  loading: boolean;
  /** Re-fetch the data manually. */
  refetch: () => void;
}

function useApiCall<T>(
  fetcher: () => Promise<T>,
  deps: unknown[] = [],
): UseApiResult<T> {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const [loading, setLoading] = useState<boolean>(true);
  const mountedRef = useRef(true);
  const triggerRef = useRef(0);

  const refetch = useCallback(() => {
    triggerRef.current += 1;
    setLoading(true);
    setError(null);

    fetcher()
      .then((result) => {
        if (mountedRef.current) {
          setData(result);
          setError(null);
        }
      })
      .catch((err: unknown) => {
        if (mountedRef.current) {
          setError(err instanceof Error ? err : new Error(String(err)));
        }
      })
      .finally(() => {
        if (mountedRef.current) {
          setLoading(false);
        }
      });
  }, [fetcher, ...deps]); // eslint-disable-line react-hooks/exhaustive-deps

  useEffect(() => {
    mountedRef.current = true;
    refetch();
    return () => {
      mountedRef.current = false;
    };
  }, [refetch]);

  return { data, error, loading, refetch };
}

// ---------------------------------------------------------------------------
// Typed hooks
// ---------------------------------------------------------------------------

/** Fetch agent status from /api/status. */
export function useStatus(): UseApiResult<StatusResponse> {
  return useApiCall(getStatus);
}

/** Fetch registered tools from /api/tools. */
export function useTools(): UseApiResult<ToolSpec[]> {
  return useApiCall(getTools);
}

/** Fetch cron jobs from /api/cron. */
export function useCronJobs(): UseApiResult<CronJob[]> {
  return useApiCall(getCronJobs);
}

/** Fetch integrations from /api/integrations. */
export function useIntegrations(): UseApiResult<Integration[]> {
  return useApiCall(getIntegrations);
}

/** Fetch memory entries, optionally filtered by query and category. */
export function useMemory(
  query?: string,
  category?: string,
): UseApiResult<MemoryEntry[]> {
  const fetcher = useCallback(
    () => getMemory(query, category),
    [query, category],
  );
  return useApiCall(fetcher, [query, category]);
}

/** Fetch cost summary from /api/cost. */
export function useCost(): UseApiResult<CostSummary> {
  return useApiCall(getCost);
}

/** Fetch CLI tools from /api/cli-tools. */
export function useCliTools(): UseApiResult<CliTool[]> {
  return useApiCall(getCliTools);
}

/** Fetch health snapshot from /api/health. */
export function useHealth(): UseApiResult<HealthSnapshot> {
  return useApiCall(getHealth);
}

/** Run doctor diagnostics from /api/doctor. */
export function useDoctor(): UseApiResult<DiagResult[]> & {
  /** Manually trigger a diagnostic run. */
  run: () => void;
} {
  const [data, setData] = useState<DiagResult[] | null>(null);
  const [error, setError] = useState<Error | null>(null);
  const [loading, setLoading] = useState<boolean>(false);
  const mountedRef = useRef(true);

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const run = useCallback(() => {
    setLoading(true);
    setError(null);

    runDoctor()
      .then((result) => {
        if (mountedRef.current) {
          setData(result);
          setError(null);
        }
      })
      .catch((err: unknown) => {
        if (mountedRef.current) {
          setError(err instanceof Error ? err : new Error(String(err)));
        }
      })
      .finally(() => {
        if (mountedRef.current) {
          setLoading(false);
        }
      });
  }, []);

  return { data, error, loading, refetch: run, run };
}
