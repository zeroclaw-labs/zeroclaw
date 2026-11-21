import { useEffect, useRef, useState } from 'react';
import { useLocation, useNavigate } from 'react-router-dom';
import { basePath } from '@/lib/basePath';
import { generateUUID } from '@/lib/uuid';
import { linkedSession } from './chatWorkspace.state';

/**
 * Launch Services may open the URL in a second browser tab. The existing chat
 * window handles that link so a new copy of every live socket is never mounted.
 * Ordinary dashboard windows elect a receiver without blocking their own UI.
 */
export function useNotificationWindow(alias: string) {
  const location = useLocation();
  const navigate = useNavigate();
  const latestNavigate = useRef(navigate);
  useEffect(() => { latestNavigate.current = navigate; }, [navigate]);
  const [arrival] = useState(() => {
    const query = new URLSearchParams(location.search);
    return { alias, session: linkedSession(location.search), linked: query.has('notify') || query.has('session') };
  });
  const supported = typeof navigator !== 'undefined' && !!navigator.locks && typeof BroadcastChannel !== 'undefined';
  const [state, setState] = useState<'waiting' | 'active' | 'forwarded'>(arrival.linked && supported ? 'waiting' : 'active');
  useEffect(() => {
    if (!supported) return;
    const name = 'zeroclaw-dashboard-chat' + basePath;
    const channel = new BroadcastChannel(name);
    const controller = new AbortController();
    const { signal } = controller;
    const closed = new Promise<void>((resolve) => signal.addEventListener('abort', () => resolve(), { once: true }));
    const requestId = generateUUID();
    let retry: ReturnType<typeof setTimeout>;
    let leader = false;
    channel.onmessage = ({ data }) => {
      if (!data || signal.aborted) return;
      if (data.type === 'opened' && data.id === requestId) {
        clearTimeout(retry);
        setState('forwarded');
      } else if (leader && data.type === 'open' && typeof data.alias === 'string' && /^[\w-]{1,128}$/.test(data.alias)) {
        const query = new URLSearchParams();
        if (typeof data.session === 'string' && /^[\w-]{1,128}$/.test(data.session)) query.set('session', data.session);
        latestNavigate.current({ pathname: '/agent/' + data.alias, search: query.toString() });
        window.focus();
        channel.postMessage({ type: 'opened', id: data.id });
      }
    };
    const claim = () => {
      void navigator.locks.request(name, arrival.linked ? { ifAvailable: true } : { signal }, async (lock) => {
        if (signal.aborted) return;
        if (lock) {
          leader = true;
          setState('active');
          await closed;
        } else {
          channel.postMessage({ type: 'open', id: requestId, alias: arrival.alias, session: arrival.session });
          // The receiver can close between the lock check and the message.
          retry = setTimeout(claim, 500);
        }
      }).catch(() => { if (!signal.aborted) setState('active'); });
    };
    claim();
    return () => { controller.abort(); clearTimeout(retry); channel.close(); };
  }, [arrival, supported]);
  return state;
}
