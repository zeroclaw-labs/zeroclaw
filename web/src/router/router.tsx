import { Suspense } from 'react';
import { Navigate, Route, Routes } from 'react-router-dom';
import Layout from '../components/layout/Layout';
import {
  AcpConsole,
  AgentChat,
  AgentWorkspaceExplorer,
  AgentsList,
  Canvas,
  Config,
  Cron,
  Dashboard,
  Doctor,
  Integrations,
  Logs,
  Pairing,
  Quickstart,
  Tools,
} from './lazyPages';

function RouteFallback() {
  return (
    <div className="min-h-[60vh] flex items-center justify-center">
      <div
        className="h-8 w-8 border-2 rounded-full animate-spin"
        style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
      />
    </div>
  );
}

export const Router = () => (
  <Suspense fallback={<RouteFallback />}>
    <Routes>
      <Route element={<Layout />}>
        <Route path="/" element={<Dashboard />} />
        <Route path="/agent" element={<Navigate to="/agents" replace />} />
        <Route path="/agents" element={<AgentsList />} />
        <Route path="/agent/:alias" element={<AgentChat />} />
        <Route path="/agent/:alias/workspace" element={<AgentWorkspaceExplorer />} />
        <Route path="/tools" element={<Tools />} />
        <Route path="/cron" element={<Cron />} />
        <Route path="/integrations" element={<Integrations />} />
        <Route path="/memory" element={<Navigate to="/?tab=memories" replace />} />
        <Route path="/config" element={<Config />} />
        <Route path="/config/:section" element={<Config />} />
        <Route path="/config/:section/:type" element={<Config />} />
        <Route path="/config/:section/:type/:alias" element={<Config />} />
        <Route path="/setup/:section" element={<Config />} />
        <Route path="/logs" element={<Logs />} />
        <Route path="/doctor" element={<Doctor />} />
        <Route path="/pairing" element={<Pairing />} />
        <Route path="/canvas" element={<Canvas />} />
        <Route path="/acp-console" element={<AcpConsole />} />
        <Route path="/quickstart" element={<Quickstart />} />
        <Route path="*" element={<Navigate to="/" replace />} />
      </Route>
    </Routes>
  </Suspense>
)
