import { useEffect, useState } from 'react';
import { Link, Navigate } from 'react-router-dom';
import { getWorkspaceAvailability } from '@/lib/api';
import { loadPersisted } from '@/pages/chatWorkspace.state';
import { t } from '@/lib/i18n';
import ChatWorkspace from '@/pages/ChatWorkspace';

export default function AgentLanding() {
  const [target, setTarget] = useState<string | null>(null);
  const [empty, setEmpty] = useState(false);
  const [error, setError] = useState('');
  useEffect(() => {
    let cancelled = false;
    void getWorkspaceAvailability()
      .then(({ agents }) => {
        if (cancelled) return;
        const stored = loadPersisted();
        const last = stored.tabs?.find(
          (tab) => tab.key === stored.activeKey && agents.includes(tab.alias),
        );
        const alias = last?.alias ?? agents[0];
        if (alias)
          setTarget(
            `/agent/${encodeURIComponent(alias)}${last ? `?session=${encodeURIComponent(last.sessionId)}` : ''}`,
          );
        else setEmpty(true);
      })
      .catch((e: Error) => {
        if (!cancelled) setError(e.message);
      });
    return () => {
      cancelled = true;
    };
  }, []);
  if (target) return <Navigate to={target} replace />;
  if (empty) return <ChatWorkspace />;
  return (
    <div className="flex h-full flex-col items-center justify-center gap-4 p-6 text-sm text-pc-text-muted">
      <p role={error ? 'alert' : 'status'}>
        {error || t(empty ? 'workspace.no_agents' : 'common.loading')}
      </p>
      {(empty || error) && (
        <Link to="/config/agents" className="text-pc-accent">
          {t('config.all_settings')}
        </Link>
      )}
    </div>
  );
}
