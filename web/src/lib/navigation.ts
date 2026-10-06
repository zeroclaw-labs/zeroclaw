import {
  Activity,
  Bot,
  Blocks,
  Clock,
  Code2,
  History,
  House,
  ListChecks,
  MessageSquare,
  Monitor,
  Puzzle,
  Settings,
  ShieldCheck,
  Smartphone,
  Sparkles,
  Stethoscope,
  Terminal,
  Workflow,
  Wrench,
  type LucideIcon,
} from "lucide-react";

/** The shared destination catalogue for navigation, search and page titles. */
export interface NavItem {
  to: string;
  icon: LucideIcon;
  labelKey: string;
  /** Presentation ownership for the Admin directory; runtime policy stays server-owned. */
  adminGroup?: "dashboards" | "management" | "advanced";
  descriptionKey?: string;
}

export interface NavGroup {
  headingKey: string;
  items: NavItem[];
}
export const navGroups: NavGroup[] = [
  {
    headingKey: "nav.group.home",
    items: [
      { to: "/", icon: House, labelKey: "home.title" },
      { to: "/admin", icon: ShieldCheck, labelKey: "workspace.admin" },
      { to: "/sessions", icon: History, labelKey: "home.sessions", adminGroup: "dashboards", descriptionKey: "home.sessions_description" },
      { to: "/agents", icon: MessageSquare, labelKey: "nav.agents", adminGroup: "management", descriptionKey: "admin.agents_description" },
      { to: "/code", icon: Code2, labelKey: "nav.code" },
      { to: "/sops", icon: Workflow, labelKey: "nav.sops" },
      { to: "/runs", icon: ListChecks, labelKey: "nav.runs", adminGroup: "dashboards", descriptionKey: "admin.runs_description" },
    ],
  },
  {
    headingKey: "nav.group.configure",
    items: [
      { to: "/config", icon: Settings, labelKey: "nav.config" },
      { to: "/config/agents", icon: Bot, labelKey: "nav.agent" },
      { to: "/tools", icon: Wrench, labelKey: "nav.tools", adminGroup: "management", descriptionKey: "home.tools_description" },
      { to: "/skills", icon: Sparkles, labelKey: "nav.skills", adminGroup: "management", descriptionKey: "home.skills_description" },
      { to: "/integrations", icon: Puzzle, labelKey: "nav.integrations", adminGroup: "management", descriptionKey: "home.integrations_description" },
      { to: "/plugins", icon: Blocks, labelKey: "nav.plugins", adminGroup: "management", descriptionKey: "plugins.subtitle" },
      { to: "/cron", icon: Clock, labelKey: "nav.cron", adminGroup: "management", descriptionKey: "home.cron_description" },
    ],
  },
  {
    headingKey: "nav.group.operations",
    items: [
      { to: "/system", icon: Activity, labelKey: "nav.system", adminGroup: "dashboards", descriptionKey: "home.system_description" },
      { to: "/logs", icon: Terminal, labelKey: "nav.logs", adminGroup: "dashboards", descriptionKey: "home.logs_description" },
      { to: "/pairing", icon: Smartphone, labelKey: "nav.pairing", adminGroup: "management", descriptionKey: "home.pairing_description" },
      { to: "/doctor", icon: Stethoscope, labelKey: "nav.doctor", adminGroup: "dashboards", descriptionKey: "home.doctor_description" },
      { to: "/canvas", icon: Monitor, labelKey: "nav.canvas", adminGroup: "advanced", descriptionKey: "home.canvas_description" },
      { to: "/acp-console", icon: Terminal, labelKey: "nav.acp", adminGroup: "advanced", descriptionKey: "home.acp_description" },
    ],
  },
];

const railDestinations = navGroups.flatMap(({ headingKey, items }) =>
  items.map((item) => ({ ...item, groupKey: headingKey })),
);

export const destinations: (NavItem & { groupKey: string })[] = [
  ...railDestinations,
  {
    to: "/system?tab=channels",
    adminGroup: "dashboards",
    descriptionKey: "admin.channels_description",
    icon: MessageSquare,
    labelKey: "dashboard.channels",
    groupKey: "nav.group.operations",
  },
  {
    to: "/system?tab=memories",
    adminGroup: "dashboards",
    descriptionKey: "admin.memory_description",
    icon: Bot,
    labelKey: "nav.memory",
    groupKey: "nav.group.operations",
  },
  {
    to: "/system?tab=cost",
    adminGroup: "dashboards",
    descriptionKey: "admin.cost_description",
    icon: Activity,
    labelKey: "nav.cost",
    groupKey: "nav.group.operations",
  },
  {
    to: "/system?tab=health",
    adminGroup: "dashboards",
    descriptionKey: "admin.health_description",
    icon: Stethoscope,
    labelKey: "dashboard.health",
    groupKey: "nav.group.operations",
  },
  {
    to: "/quickstart",
    adminGroup: "advanced",
    descriptionKey: "admin.setup_description",
    icon: Sparkles,
    labelKey: "nav.quickstart",
    groupKey: "nav.group.configure",
  },
];

export function routeTitleKey(path: string): string | undefined {
  if (path.startsWith("/agent/")) return "nav.agent";
  if (path.startsWith("/setup/")) return "nav.config";
  if (path === "/quickstart") return "nav.quickstart";
  return destinations
    .filter(
      ({ to }) => path === to || (to !== "/" && path.startsWith(`${to}/`)),
    )
    .sort((a, b) => b.to.length - a.to.length)[0]?.labelKey;
}

/** Context links are routes to the owning configuration, never policy copies. */
export function featureSettingsPath(path: string): string | null {
  if (path === "/admin") return "/config";
  const segments = path.split("/").filter(Boolean);
  if (segments[0] === "agent" && segments[1])
    return `/config/agents/${segments[1]}`;
  const sections: Record<string, string> = {
    agents: "agents",
    code: "agents",
    sessions: "agents",
    sops: "sop",
    runs: "sop",
    cron: "cron",
    tools: "risk_profiles",
    skills: "skill_bundles",
    integrations: "mcp",
    pairing: "gateway",
    logs: "observability",
    "acp-console": "acp",
    system: "gateway",
  };
  const section = sections[segments[0] ?? ""];
  return section ? `/config/${section}` : null;
}

/** Workspace selection is derived from the current route, never saved separately. */
export function activeWorkspace(path: string): string | null {
  const owns = (root: string) => path === root || path.startsWith(`${root}/`);
  if (owns('/agent')) return '/agent';
  if (owns('/code')) return '/code';
  if (owns('/sops') || owns('/runs')) return '/sops';
  if (owns('/setup') || destinations.some(({ to }) => to !== '/' && owns(to.split('?')[0]!))) return '/admin';
  return null;
}
