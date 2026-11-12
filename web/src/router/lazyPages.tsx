import { lazy, type ComponentType } from 'react';
import { loadPageOrReload } from '../lib/chunkReload';

/** A page loaded on first visit; see `loadPageOrReload` for a failed load. */
function lazyPage<T extends ComponentType<any>>(load: () => Promise<{ default: T }>) {
  return lazy(() => loadPageOrReload(load));
}

export const Dashboard = lazyPage(() => import('../pages/Dashboard'));
export const AgentChat = lazyPage(() => import('../pages/AgentChat'));
export const AgentsList = lazyPage(() => import('../pages/AgentsList'));
export const AgentWorkspaceExplorer = lazyPage(() => import('../pages/AgentWorkspaceExplorer'));
export const Tools = lazyPage(() => import('../pages/Tools'));
export const Cron = lazyPage(() => import('../pages/Cron'));
export const Integrations = lazyPage(() => import('../pages/Integrations'));
export const Plugins = lazyPage(() => import('../pages/Plugins'));
export const Config = lazyPage(() => import('../pages/Config'));
export const Logs = lazyPage(() => import('../pages/Logs'));
export const Doctor = lazyPage(() => import('../pages/Doctor'));
export const Pairing = lazyPage(() => import('../pages/Pairing'));
export const Canvas = lazyPage(() => import('../pages/Canvas'));
export const AcpConsole = lazyPage(() => import('../pages/AcpConsole'));
export const Quickstart = lazyPage(() => import('../pages/quickstart/Quickstart'));
export const Skills = lazyPage(() => import('../pages/Skills'));
export const SopsList = lazyPage(() => import('../pages/Sops').then((m) => ({ default: m.SopsList })));
export const SopView = lazyPage(() => import('../pages/Sops').then((m) => ({ default: m.SopView })));
export const SopEditor = lazyPage(() =>
  import('../pages/Sops').then((m) => ({ default: m.SopEditor })),
);
export const Runs = lazyPage(() => import('../pages/Runs'));
export const RunDetail = lazyPage(() => import('../pages/RunDetail'));
