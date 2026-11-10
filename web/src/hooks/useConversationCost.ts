import { useEffect, useRef, useState } from 'react';
import {
  ApiError, HttpError, OperatorRequiredError, UnauthorizedError,
  getSessionCost, type ConversationCostSummary,
} from '@/lib/api';
import { AUTH_EVENTS } from '@/lib/auth';
import { ConversationCostPoller } from '@/lib/conversationCost';

export function useConversationCost(
  sessionId: string,
  connected: boolean,
  typing: boolean,
  load: typeof getSessionCost = getSessionCost,
  visible = true,
): ConversationCostSummary | null {
  const [result, setResult] = useState<{ sessionId: string; value: ConversationCostSummary | null } | null>(null);
  const [authGeneration, setAuthGeneration] = useState(0);
  const pollerRef = useRef<ConversationCostPoller | null>(null);
  const visibleRef = useRef(visible);
  useEffect(() => {
    visibleRef.current = visible;
    pollerRef.current?.setVisible(visible && document.visibilityState !== 'hidden');
  }, [visible]);
  const blocked = useRef<'unsupported' | 'denied' | null>(null);
  useEffect(() => {
    if (blocked.current === 'unsupported') blocked.current = null;
  }, [connected, load]);
  useEffect(() => {
    const resume = () => {
      if (blocked.current === 'denied') blocked.current = null;
      setAuthGeneration((value) => value + 1);
    };
    const suspend = () => {
      blocked.current = 'denied';
      pollerRef.current?.dispose();
      pollerRef.current = null;
      setResult(null);
    };
    const refusals = [AUTH_EVENTS.unauthorized, AUTH_EVENTS.reauth, AUTH_EVENTS.sessionEnded, AUTH_EVENTS.accessLost];
    window.addEventListener(AUTH_EVENTS.resumed, resume);
    for (const event of refusals) window.addEventListener(event, suspend);
    return () => {
      window.removeEventListener(AUTH_EVENTS.resumed, resume);
      for (const event of refusals) window.removeEventListener(event, suspend);
    };
  }, []);
  useEffect(() => {
    setResult(null);
    if (!connected || blocked.current) return;
    const poller = new ConversationCostPoller(
      (signal) => load(sessionId, signal),
      (value) => setResult({ sessionId, value }),
      (error) => {
        if (error instanceof UnauthorizedError || error instanceof OperatorRequiredError
          || ((error instanceof HttpError || error instanceof ApiError) && [401, 403].includes(error.status))) {
          blocked.current = 'denied';
        } else if ((error instanceof HttpError || error instanceof ApiError) && [404, 405].includes(error.status)) {
          blocked.current = 'unsupported';
        }
        return blocked.current !== null;
      },
    );
    pollerRef.current = poller;
    const visibility = () => poller.setVisible(visibleRef.current && document.visibilityState !== 'hidden');
    document.addEventListener('visibilitychange', visibility);
    visibility();
    return () => {
      poller.dispose();
      pollerRef.current = null;
      document.removeEventListener('visibilitychange', visibility);
    };
  }, [sessionId, connected, authGeneration, load]);
  const previousTyping = useRef(typing);
  useEffect(() => {
    if (previousTyping.current && !typing) pollerRef.current?.refresh();
    previousTyping.current = typing;
  }, [typing]);
  return connected && result?.sessionId === sessionId ? result.value : null;
}
