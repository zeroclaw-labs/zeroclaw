import { lazy } from 'react';

export const Dashboard = lazy(() => import('../pages/Dashboard'));
export const AgentChat = lazy(() => import('../pages/AgentChat'));
export const Tools = lazy(() => import('../pages/Tools'));
export const Cron = lazy(() => import('../pages/Cron'));
export const Integrations = lazy(() => import('../pages/Integrations'));
export const Memory = lazy(() => import('../pages/Memory'));
export const Config = lazy(() => import('../pages/Config'));
export const Cost = lazy(() => import('../pages/Cost'));
export const Logs = lazy(() => import('../pages/Logs'));
export const Doctor = lazy(() => import('../pages/Doctor'));
export const Pairing = lazy(() => import('../pages/Pairing'));
export const Canvas = lazy(() => import('../pages/Canvas'));
export const Nodes = lazy(() => import('../pages/Nodes'));
export const Onboard = lazy(() => import('../pages/onboard/Onboard'));
