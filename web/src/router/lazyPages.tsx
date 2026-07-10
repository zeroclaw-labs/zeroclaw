import { lazy } from 'react';

export const Dashboard = lazy(() => import('../pages/Dashboard'));
export const AgentChat = lazy(() => import('../pages/AgentChat'));
export const AgentsList = lazy(() => import('../pages/AgentsList'));
export const AgentWorkspaceExplorer = lazy(() => import('../pages/AgentWorkspaceExplorer'));
export const Tools = lazy(() => import('../pages/Tools'));
export const Cron = lazy(() => import('../pages/Cron'));
export const Integrations = lazy(() => import('../pages/Integrations'));
export const Config = lazy(() => import('../pages/Config'));
export const Logs = lazy(() => import('../pages/Logs'));
export const Doctor = lazy(() => import('../pages/Doctor'));
export const Pairing = lazy(() => import('../pages/Pairing'));
export const Canvas = lazy(() => import('../pages/Canvas'));
export const AcpConsole = lazy(() => import('../pages/AcpConsole'));
export const Quickstart = lazy(() => import('../pages/quickstart/Quickstart'));
export const Skills = lazy(() => import('../pages/Skills'));
export const SopsList = lazy(() => import('../pages/Sops').then((m) => ({ default: m.SopsList })));
export const SopView = lazy(() => import('../pages/Sops').then((m) => ({ default: m.SopView })));
export const SopEditor = lazy(() =>
  import('../pages/Sops').then((m) => ({ default: m.SopEditor })),
);
export const Runs = lazy(() => import('../pages/Runs'));
export const RunDetail = lazy(() => import('../pages/RunDetail'));
