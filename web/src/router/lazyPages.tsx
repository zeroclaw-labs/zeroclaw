import { lazy } from 'react';

export const Dashboard = lazy(() => import('../pages/Dashboard'));
export const AgentChat = lazy(() => import('../pages/AgentChat'));
export const AgentsList = lazy(() => import('../pages/AgentsList'));
export const AgentWorkspaceExplorer = lazy(() => import('../pages/AgentWorkspaceExplorer'));
export const Tools = lazy(() => import('../pages/Tools'));
export const Cron = lazy(() => import('../pages/Cron'));
export const Providers = lazy(() => import('../pages/Providers'));
export const Mcp = lazy(() => import('../pages/Mcp'));
export const Skills = lazy(() => import('../pages/Skills'));
export const Plugins = lazy(() => import('../pages/Plugins'));
export const Config = lazy(() => import('../pages/Config'));
export const Logs = lazy(() => import('../pages/Logs'));
export const Doctor = lazy(() => import('../pages/Doctor'));
export const Pairing = lazy(() => import('../pages/Pairing'));
export const Canvas = lazy(() => import('../pages/Canvas'));
export const Quickstart = lazy(() => import('../pages/quickstart/Quickstart'));
