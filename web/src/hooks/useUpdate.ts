import { useState, useEffect, useCallback, useRef } from 'react';
import { useSSE } from './useSSE';
import { checkForUpdate, runUpdate } from '@/lib/api';

export type UpdateState = 'idle' | 'checking' | 'available' | 'up-to-date' | 'updating' | 'complete' | 'error';

export interface UpdateInfo {
  state: UpdateState;
  latestVersion: string | null;
  progressMsg: string;
  errorMsg: string;
  check: () => void;
  run: () => void;
}

export function useUpdate(): UpdateInfo {
  const [state, setState] = useState<UpdateState>('idle');
  const [latestVersion, setLatestVersion] = useState<string | null>(null);
  const [progressMsg, setProgressMsg] = useState('');
  const [errorMsg, setErrorMsg] = useState('');

  const { events } = useSSE({
    filterTypes: ['update_progress', 'update_complete', 'update_failed'],
    autoConnect: true,
  });
  const eventsRef = useRef(events);
  eventsRef.current = events;

  useEffect(() => {
    const latest = events[events.length - 1];
    if (!latest) return;
    if (latest.type === 'update_progress') {
      setState('updating');
      setProgressMsg((latest as { message?: string }).message || 'Updating...');
    } else if (latest.type === 'update_complete') {
      setState('complete');
      setProgressMsg('');
    } else if (latest.type === 'update_failed') {
      setState('error');
      setErrorMsg((latest as { error?: string }).error || 'Update failed');
    }
  }, [events]);

  const check = useCallback(async () => {
    setState('checking');
    try {
      const result = await checkForUpdate();
      if (result.is_newer) {
        setLatestVersion(result.latest_version);
        setState('available');
      } else {
        setState('up-to-date');
      }
    } catch {
      setState('error');
      setErrorMsg('Failed to check for updates');
    }
  }, []);

  const run = useCallback(async () => {
    try {
      setState('updating');
      setProgressMsg('Starting update...');
      await runUpdate();
    } catch (e) {
      if (!(e instanceof Error && e.message.includes('409'))) {
        setState('error');
        setErrorMsg(e instanceof Error ? e.message : 'Failed to start update');
      }
    }
  }, []);

  return { state, latestVersion, progressMsg, errorMsg, check, run };
}
