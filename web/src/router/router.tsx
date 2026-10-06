import { Suspense } from 'react';
import { Navigate, Route, Routes } from 'react-router-dom';
import AgentLanding from '../pages/AgentLanding';
import Layout from '../components/layout/Layout';
import {
  AcpConsole,
  Admin,
  AgentChat,
  AgentWorkspaceExplorer,
  AgentsList,
  Canvas,
  Code,
  Config,
  Cron,
  Dashboard,
  Home,
  Sessions,
  Doctor,
  Integrations,
  Logs,
  Pairing,
  Plugins,
  Quickstart,
  RunDetail,
  Runs,
  Skills,
  SopWorkspace,
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
        <Route path="/" element={<Home />} />
        <Route path="/admin" element={<Admin />} />
        <Route path="/system" element={<Dashboard />} />
        <Route path="/sessions" element={<Sessions />} />
        <Route path="/code" element={<Code />} />
        <Route path="/agent" element={<AgentLanding />} />
        <Route path="/agents" element={<AgentsList />} />
        <Route path="/agent/:alias" element={<AgentChat />} />
        <Route path="/agent/:alias/workspace" element={<AgentWorkspaceExplorer />} />
        <Route path="/tools" element={<Tools />} />
        <Route path="/cron" element={<Cron />} />
        <Route path="/skills" element={<Skills />} />
        <Route path="/sops" element={<SopWorkspace />} />
        <Route path="/sops/new" element={<SopWorkspace />} />
        <Route path="/sops/:name" element={<SopWorkspace />} />
        <Route path="/sops/:name/edit" element={<SopWorkspace />} />
        <Route path="/runs" element={<Runs />} />
        <Route path="/runs/:sop/:runId" element={<RunDetail />} />
        <Route path="/integrations" element={<Integrations />} />
        <Route path="/plugins" element={<Plugins />} />
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
