import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ReactNode,
} from "react";

type Locale = "en" | "zh";
type ThemeMode = "system" | "dark" | "light";
type ResolvedTheme = "dark" | "light";
type LocalizedText = Record<Locale, string>;

const categories = [
  "Getting Started",
  "Configuration",
  "Channels",
  "Operations",
  "Security",
  "Reference",
] as const;

const levelFilterOptions = ["All", "Core", "Advanced"] as const;

type DocCategory = (typeof categories)[number];
type CategoryFilter = "All" | DocCategory;
type DocLevel = "Core" | "Advanced";
type LevelFilter = (typeof levelFilterOptions)[number];

type DocEntry = {
  title: LocalizedText;
  path: string;
  category: DocCategory;
  summary: LocalizedText;
  level: DocLevel;
  featured?: boolean;
  keywords?: string[];
};

type QuickPath = {
  title: LocalizedText;
  summary: LocalizedText;
  docs: string[];
};

type Capability = {
  title: LocalizedText;
  text: LocalizedText;
};

type ExecutionPhase = {
  phase: LocalizedText;
  detail: LocalizedText;
};

type PaletteAction = {
  id: string;
  label: string;
  hint: string;
  keywords: string[];
  run: () => void;
};

type PaletteEntry = {
  id: string;
  kind: "action" | "doc";
  label: string;
  hint: string;
  meta?: string;
  run: () => void;
};

function bi(en: string, zh: string): LocalizedText {
  return { en, zh };
}

const categoryLabels: Record<Locale, Record<DocCategory, string>> = {
  en: {
    "Getting Started": "Getting Started",
    Configuration: "Configuration",
    Channels: "Channels",
    Operations: "Operations",
    Security: "Security",
    Reference: "Reference",
  },
  zh: {
    "Getting Started": "快速开始",
    Configuration: "配置",
    Channels: "渠道集成",
    Operations: "运维",
    Security: "安全",
    Reference: "参考资料",
  },
};

const levelLabels: Record<Locale, Record<LevelFilter, string>> = {
  en: {
    All: "All",
    Core: "Core",
    Advanced: "Advanced",
  },
  zh: {
    All: "全部",
    Core: "核心",
    Advanced: "进阶",
  },
};

const languageLabels: Record<Locale, Record<Locale, string>> = {
  en: {
    en: "EN",
    zh: "ZH",
  },
  zh: {
    en: "英文",
    zh: "中文",
  },
};

const themeLabels: Record<Locale, Record<ThemeMode, string>> = {
  en: {
    system: "Auto",
    dark: "Dark",
    light: "Light",
  },
  zh: {
    system: "自动",
    dark: "深色",
    light: "浅色",
  },
};

const capabilities: Capability[] = [
  {
    title: bi("Velocity Routing", "极速路由"),
    text: bi(
      "High-speed command and message lanes tuned for low latency and stable fan-out under sustained load.",
      "高吞吐命令与消息通道，针对低延迟和高压下稳定分发进行调优。"
    ),
  },
  {
    title: bi("Cold-State Control", "冷态控制"),
    text: bi(
      "Deterministic runtime boundaries keep long-running workflows crisp, predictable, and resilient.",
      "确定性的运行时边界让长流程保持清晰、可预测且具备韧性。"
    ),
  },
  {
    title: bi("Policy Surface", "策略面"),
    text: bi(
      "Security defaults stay explicit and auditable from ingress validation to tool invocation.",
      "从入口校验到工具调用，安全默认项保持显式、可审计。"
    ),
  },
  {
    title: bi("Signal Telemetry", "信号遥测"),
    text: bi(
      "Operator feedback loops expose execution rhythm in real time without sacrificing flow.",
      "在不破坏流畅性的前提下，实时暴露执行节奏与反馈回路。"
    ),
  },
];

const executionPhases: ExecutionPhase[] = [
  {
    phase: bi("Ingress", "接入"),
    detail: bi(
      "Webhook and channel payload normalization",
      "Webhook 与渠道负载标准化"
    ),
  },
  {
    phase: bi("Policy", "策略"),
    detail: bi(
      "Authentication and runtime guard checks",
      "认证与运行时防护校验"
    ),
  },
  {
    phase: bi("Inference", "推理"),
    detail: bi(
      "Provider dispatch with fallback controls",
      "带回退控制的模型路由分发"
    ),
  },
  {
    phase: bi("Delivery", "投递"),
    detail: bi(
      "Response routing with trace visibility",
      "带可追踪可见性的响应投递"
    ),
  },
];

const signalFeed: LocalizedText[] = [
  bi("gateway.pulse/ops-eu-1 :: RTT 17ms", "gateway.pulse/ops-eu-1 :: 往返 17ms"),
  bi("policy.guard/strict-mode :: pass", "policy.guard/strict-mode :: 通过"),
  bi("provider.dispatch/openai :: stable", "provider.dispatch/openai :: 稳定"),
  bi("channel.delivery/slack :: stream", "channel.delivery/slack :: 流式"),
];

const traitChips: LocalizedText[] = [
  bi("Rust-native Core", "Rust 原生核心"),
  bi("Low-latency Runtime", "低延迟运行时"),
  bi("Policy-first Execution", "策略优先执行"),
  bi("Traceable Operations", "可追踪运维"),
];

const sectionNav = [
  { id: "hero-core", label: bi("Overview", "总览") },
  { id: "core-shortcuts", label: bi("Shortcuts", "捷径") },
  { id: "quick-paths", label: bi("Pathways", "路径") },
  { id: "capability-stack", label: bi("Capabilities", "能力") },
  { id: "execution-lattice", label: bi("Execution", "执行") },
  { id: "docs-navigator", label: bi("Docs", "文档") },
] as const;

const quickPaths: QuickPath[] = [
  {
    title: bi("Launch a Fresh Node", "快速启动节点"),
    summary: bi(
      "From zero to a running instance with deployment boundaries defined early.",
      "从零到可运行实例，并提前明确部署边界。"
    ),
    docs: [
      "docs/getting-started/README.md",
      "docs/one-click-bootstrap.md",
      "docs/network-deployment.md",
    ],
  },
  {
    title: bi("Wire Channels Fast", "快速接入渠道"),
    summary: bi(
      "Connect external chat surfaces and validate routing behavior quickly.",
      "快速接入外部聊天渠道并验证路由行为。"
    ),
    docs: [
      "docs/channels-reference.md",
      "docs/mattermost-setup.md",
      "docs/nextcloud-talk-setup.md",
    ],
  },
  {
    title: bi("Harden Runtime Policy", "强化运行策略"),
    summary: bi(
      "Lock down sandbox, security posture, and operational guardrails.",
      "完善沙箱、安全姿态和运维防护策略。"
    ),
    docs: [
      "docs/security/README.md",
      "docs/sandboxing.md",
      "docs/operations/README.md",
    ],
  },
];

const docsCatalog: DocEntry[] = [
  {
    title: bi("Docs Home", "文档首页"),
    path: "docs/README.md",
    category: "Getting Started",
    summary: bi(
      "Main documentation hub with starting points and structure.",
      "文档总入口，包含起步路径与结构索引。"
    ),
    level: "Core",
    featured: true,
    keywords: ["docs", "home", "文档", "入口"],
  },
  {
    title: bi("Getting Started", "快速开始"),
    path: "docs/getting-started/README.md",
    category: "Getting Started",
    summary: bi(
      "Installation and first-run onboarding references.",
      "安装与首次运行的引导说明。"
    ),
    level: "Core",
    featured: true,
    keywords: ["install", "onboarding", "安装", "起步"],
  },
  {
    title: bi("One-click Bootstrap", "一键初始化"),
    path: "docs/one-click-bootstrap.md",
    category: "Getting Started",
    summary: bi(
      "Fast bootstrap flow for new environments.",
      "面向新环境的快速初始化流程。"
    ),
    level: "Core",
    keywords: ["bootstrap", "new env", "初始化"],
  },
  {
    title: bi("Network Deployment", "网络部署"),
    path: "docs/network-deployment.md",
    category: "Getting Started",
    summary: bi(
      "Gateway and callback deployment topology guidance.",
      "网关与回调部署拓扑指南。"
    ),
    level: "Advanced",
    keywords: ["network", "deployment", "网关", "部署"],
  },
  {
    title: bi("Config Reference", "配置参考"),
    path: "docs/config-reference.md",
    category: "Configuration",
    summary: bi(
      "Canonical runtime and provider configuration schema.",
      "标准运行时与模型提供方配置结构。"
    ),
    level: "Core",
    featured: true,
    keywords: ["config", "schema", "配置"],
  },
  {
    title: bi("Commands Reference", "命令参考"),
    path: "docs/commands-reference.md",
    category: "Configuration",
    summary: bi(
      "CLI command map and behavior details.",
      "CLI 命令清单与行为细节。"
    ),
    level: "Core",
    keywords: ["commands", "cli", "命令"],
  },
  {
    title: bi("Custom Providers", "自定义提供方"),
    path: "docs/custom-providers.md",
    category: "Configuration",
    summary: bi(
      "How to wire custom inference provider integrations.",
      "如何接入自定义模型提供方。"
    ),
    level: "Advanced",
    keywords: ["provider", "integration", "提供方", "集成"],
  },
  {
    title: bi("Channels Reference", "渠道参考"),
    path: "docs/channels-reference.md",
    category: "Channels",
    summary: bi(
      "Unified channel setup, routing, and troubleshooting.",
      "统一渠道配置、路由与排障指南。"
    ),
    level: "Core",
    featured: true,
    keywords: ["channels", "routing", "渠道", "路由"],
  },
  {
    title: bi("Mattermost Setup", "Mattermost 配置"),
    path: "docs/mattermost-setup.md",
    category: "Channels",
    summary: bi(
      "Mattermost integration runbook and expected behavior.",
      "Mattermost 集成操作手册与预期行为。"
    ),
    level: "Advanced",
    keywords: ["mattermost", "integration", "集成"],
  },
  {
    title: bi("Nextcloud Talk Setup", "Nextcloud Talk 配置"),
    path: "docs/nextcloud-talk-setup.md",
    category: "Channels",
    summary: bi(
      "Native Nextcloud Talk webhook and bot setup.",
      "原生 Nextcloud Talk webhook 与机器人配置。"
    ),
    level: "Advanced",
    keywords: ["nextcloud", "talk", "webhook"],
  },
  {
    title: bi("Operations Hub", "运维中心"),
    path: "docs/operations/README.md",
    category: "Operations",
    summary: bi(
      "Operational runbooks and recovery procedures.",
      "运维手册与故障恢复流程。"
    ),
    level: "Core",
    keywords: ["operations", "runbook", "运维", "恢复"],
  },
  {
    title: bi("Connectivity Probes Runbook", "连通性探针手册"),
    path: "docs/operations/connectivity-probes-runbook.md",
    category: "Operations",
    summary: bi(
      "Probe strategy, diagnostics, and alerting actions.",
      "探针策略、诊断流程与告警动作。"
    ),
    level: "Advanced",
    keywords: ["probe", "diagnostics", "告警", "探针"],
  },
  {
    title: bi("Security Overview", "安全总览"),
    path: "docs/security/README.md",
    category: "Security",
    summary: bi(
      "Security policy, process, and advisory handling.",
      "安全策略、流程与公告处理规范。"
    ),
    level: "Core",
    keywords: ["security", "policy", "安全"],
  },
  {
    title: bi("Sandboxing", "沙箱机制"),
    path: "docs/sandboxing.md",
    category: "Security",
    summary: bi(
      "Isolation modes, boundaries, and tradeoffs.",
      "隔离模式、边界与权衡。"
    ),
    level: "Advanced",
    keywords: ["sandbox", "isolation", "沙箱", "隔离"],
  },
  {
    title: bi("Agnostic Security", "跨提供方安全"),
    path: "docs/agnostic-security.md",
    category: "Security",
    summary: bi(
      "Cross-provider security posture and design rationale.",
      "跨模型提供方的安全姿态与设计依据。"
    ),
    level: "Advanced",
    keywords: ["agnostic", "cross-provider", "跨提供方"],
  },
  {
    title: bi("Reference Index", "参考索引"),
    path: "docs/reference/README.md",
    category: "Reference",
    summary: bi(
      "Reference material index for deeper lookup.",
      "用于深入查阅的参考资料索引。"
    ),
    level: "Core",
    keywords: ["reference", "index", "索引"],
  },
  {
    title: bi("Docs Inventory", "文档清单"),
    path: "docs/docs-inventory.md",
    category: "Reference",
    summary: bi(
      "Inventory and ownership view of documentation assets.",
      "文档资产与归属的清单视图。"
    ),
    level: "Advanced",
    keywords: ["inventory", "ownership", "文档清单"],
  },
  {
    title: bi("Resource Limits", "资源限制"),
    path: "docs/resource-limits.md",
    category: "Reference",
    summary: bi(
      "Runtime and deployment resource limit guidance.",
      "运行时与部署资源限制建议。"
    ),
    level: "Advanced",
    keywords: ["resource", "limits", "资源"],
  },
  {
    title: bi("Repository README", "仓库 README"),
    path: "README.md",
    category: "Reference",
    summary: bi(
      "Project overview, features, and quick commands.",
      "项目概览、能力说明与快速命令。"
    ),
    level: "Core",
    featured: true,
    keywords: ["readme", "project", "仓库", "项目"],
  },
];

const monitorStats = [
  {
    label: bi("Queue Depth", "队列深度"),
    value: bi("Stable", "稳定"),
  },
  {
    label: bi("Retry Rate", "重试率"),
    value: bi("0.4%", "0.4%"),
  },
  {
    label: bi("Dispatch RTT", "分发延迟"),
    value: bi("17ms", "17ms"),
  },
  {
    label: bi("Guard State", "防护状态"),
    value: bi("Strict", "严格"),
  },
];

const textBundle = {
  en: {
    navDocs: "Docs Navigator",
    navRepo: "Repository",
    openPalette: "Command Palette",
    languageLabel: "Language",
    themeLabel: "Theme",
    statusPill: "Cold Core Online",
    heroKicker: "Private Gateway Control Plane",
    heroTitle: ["Lightning Fast.", "Ice Cold.", "Precise by Design."],
    heroLead:
      "Inspired by modern product surfaces, this interface blends high contrast, restrained glow, and rapid yet smooth motion while keeping documentation access immediate and obvious.",
    browseDocsNow: "Browse Docs Now",
    openDocsHome: "Open Docs Home",
    metrics: [
      { label: "Execution Profile", value: "Rapid / Deterministic" },
      { label: "Build Material", value: "100% Rust Engine" },
      { label: "Visual Mode", value: "Black · Blue · Silver" },
      { label: "Docs Indexed", value: `${docsCatalog.length} Files` },
    ],
    runtimeLabel: "runtime://zeroclaw/gateway",
    live: "Live",
    coreDocsShortcuts: "Core Docs Shortcuts",
    openFullNavigator: "Open Full Navigator",
    quickStartPaths: "Quick Start Paths",
    taskFirstRouting: "Task-first Routing",
    capabilityStack: "Capability Stack",
    executionLattice: "Execution Lattice",
    operatorSignal: "Operator Signal",
    operatorSignalIntro:
      "Fast transitions, clear hierarchy, and controlled glow ensure the interface reads quickly while staying calm in long sessions.",
    docsNavigator: "Docs Navigator",
    docsIntro:
      "Search by topic, file name, or category. Filter quickly, then open rendered docs directly on GitHub.",
    searchDocs: "Search docs",
    searchPlaceholder: "Try: config, channels, security, operations...",
    quickKeys: "Quick keys:",
    quickKeySearchHint: "focus search",
    quickKeyResetHint: "reset filters",
    resetFilters: "Reset Filters",
    docsCount: (count: number, filtered: boolean) =>
      `${count} doc${count === 1 ? "" : "s"} matched${
        filtered ? " in filtered mode" : " across all sections"
      }`,
    topMatches: "Top Matches",
    noMatchPrefix: "No docs matched your filter. Try broader keywords like",
    noMatchA: "config",
    noMatchConnector: "or",
    noMatchB: "security",
    panelFooter:
      "Documentation remains the primary navigation target. Every major section is indexed for direct reading access.",
    paletteTitle: "Command Palette",
    palettePlaceholder: "Type a command or search docs...",
    paletteClose: "Close",
    paletteActionsGroup: "Quick Actions",
    paletteDocsGroup: "Documentation",
    paletteNoResults: "No matching command or doc entry.",
    palettePreviewTitle: "Live Preview",
    palettePreviewNoSelection: "Select an entry to inspect details and run.",
    palettePreviewKindAction: "Action",
    palettePreviewKindDoc: "Doc Entry",
    palettePreviewOpenHint: "Opens in a new tab",
    paletteHintNavigate: "Arrow/Tab to navigate",
    paletteHintRun: "Enter to run",
    paletteHintClose: "Esc to close",
    actionJumpDocs: "Jump to Docs Navigator",
    actionJumpDocsHint: "Scroll to the main docs section",
    actionOpenDocsHome: "Open Docs Home",
    actionOpenDocsHomeHint: "Open docs hub in a new tab",
    actionOpenRepo: "Open Repository",
    actionOpenRepoHint: "Open GitHub repository",
    actionToggleLang: "Switch Language",
    actionToggleLangHint: "Toggle between English and Chinese",
    actionThemeDark: "Set Theme: Dark",
    actionThemeLight: "Set Theme: Light",
    actionThemeSystem: "Set Theme: Auto",
    actionThemeHint: "Update visual theme mode",
    sectionRailLabel: "Section navigation",
    mobileDockDocs: "Docs",
    mobileDockPalette: "Palette",
  },
  zh: {
    navDocs: "文档导航",
    navRepo: "仓库",
    openPalette: "命令面板",
    languageLabel: "语言",
    themeLabel: "主题",
    statusPill: "冷核在线",
    heroKicker: "私有网关控制平面",
    heroTitle: ["闪电级速度。", "冰冷而克制。", "精准而生。"],
    heroLead:
      "参考现代产品界面的节奏，这个页面将高对比、克制发光与迅捷流畅动效结合，同时把文档可达性放在第一优先级。",
    browseDocsNow: "立即浏览文档",
    openDocsHome: "打开文档首页",
    metrics: [
      { label: "执行画像", value: "快速 / 确定性" },
      { label: "构建材质", value: "100% Rust 引擎" },
      { label: "视觉模式", value: "黑 · 蓝 · 银" },
      { label: "已索引文档", value: `${docsCatalog.length} 个文件` },
    ],
    runtimeLabel: "runtime://zeroclaw/gateway",
    live: "实时",
    coreDocsShortcuts: "核心文档捷径",
    openFullNavigator: "打开完整导航",
    quickStartPaths: "任务快速路径",
    taskFirstRouting: "任务优先路由",
    capabilityStack: "能力栈",
    executionLattice: "执行晶格",
    operatorSignal: "操作信号",
    operatorSignalIntro:
      "快速过渡、清晰层级与克制发光，让界面在长时间使用时依然高效且不疲劳。",
    docsNavigator: "文档导航器",
    docsIntro:
      "按主题、文件名或分类搜索，快速过滤后可直接打开 GitHub 渲染文档。",
    searchDocs: "搜索文档",
    searchPlaceholder: "例如：config、channels、security、operations...",
    quickKeys: "快捷键：",
    quickKeySearchHint: "聚焦搜索",
    quickKeyResetHint: "重置筛选",
    resetFilters: "重置筛选",
    docsCount: (count: number, filtered: boolean) =>
      `匹配到 ${count} 个文档${filtered ? "（已筛选）" : "（全量范围）"}`,
    topMatches: "最佳匹配",
    noMatchPrefix: "没有匹配结果，建议尝试更宽泛关键词，例如",
    noMatchA: "config",
    noMatchConnector: "或",
    noMatchB: "security",
    panelFooter: "文档是第一导航目标，所有核心部分均已索引并可直达阅读。",
    paletteTitle: "命令面板",
    palettePlaceholder: "输入命令或搜索文档...",
    paletteClose: "关闭",
    paletteActionsGroup: "快捷动作",
    paletteDocsGroup: "文档",
    paletteNoResults: "没有匹配的命令或文档。",
    palettePreviewTitle: "实时预览",
    palettePreviewNoSelection: "选择一条结果查看细节并执行。",
    palettePreviewKindAction: "动作",
    palettePreviewKindDoc: "文档项",
    palettePreviewOpenHint: "将使用新标签页打开",
    paletteHintNavigate: "方向键/Tab 切换",
    paletteHintRun: "Enter 执行",
    paletteHintClose: "Esc 关闭",
    actionJumpDocs: "跳转到文档导航",
    actionJumpDocsHint: "滚动到主文档区域",
    actionOpenDocsHome: "打开文档首页",
    actionOpenDocsHomeHint: "新标签页打开文档总入口",
    actionOpenRepo: "打开仓库",
    actionOpenRepoHint: "打开 GitHub 仓库",
    actionToggleLang: "切换语言",
    actionToggleLangHint: "中英文切换",
    actionThemeDark: "设置主题：深色",
    actionThemeLight: "设置主题：浅色",
    actionThemeSystem: "设置主题：自动",
    actionThemeHint: "更新视觉主题模式",
    sectionRailLabel: "分区导航",
    mobileDockDocs: "文档",
    mobileDockPalette: "面板",
  },
} as const;

const docsBase = "https://github.com/zeroclaw-labs/zeroclaw/blob/main";

function getDocUrl(path: string): string {
  return `${docsBase}/${path}`;
}

function localize(value: LocalizedText, locale: Locale): string {
  return value[locale];
}

function escapeRegExp(text: string): string {
  return text.replace(/[.*+?^${}()|[\]\\]/g, "\\$&");
}

function tokenizeQuery(query: string): string[] {
  return Array.from(
    new Set(
      query
        .trim()
        .toLowerCase()
        .split(/\s+/)
        .map((token) => token.trim())
        .filter((token) => token.length > 0)
        .filter((token) => !/^[a-z0-9]$/i.test(token))
    )
  );
}

function scoreDoc(doc: DocEntry, tokens: string[], locale: Locale): number {
  if (tokens.length === 0) {
    return 0;
  }

  const altLocale: Locale = locale === "en" ? "zh" : "en";
  const primaryTitle = localize(doc.title, locale).toLowerCase();
  const primarySummary = localize(doc.summary, locale).toLowerCase();
  const altTitle = localize(doc.title, altLocale).toLowerCase();
  const altSummary = localize(doc.summary, altLocale).toLowerCase();
  const primaryCategory = categoryLabels[locale][doc.category].toLowerCase();
  const altCategory = categoryLabels[altLocale][doc.category].toLowerCase();
  const path = doc.path.toLowerCase();
  const keywords = (doc.keywords ?? []).join(" ").toLowerCase();

  return tokens.reduce((score, token) => {
    let current = score;

    if (primaryTitle.includes(token)) {
      current += 7;
    }
    if (primaryCategory.includes(token)) {
      current += 5;
    }
    if (primarySummary.includes(token)) {
      current += 4;
    }
    if (keywords.includes(token)) {
      current += 3;
    }
    if (path.includes(token)) {
      current += 2;
    }
    if (altTitle.includes(token) || altSummary.includes(token) || altCategory.includes(token)) {
      current += 2;
    }

    return current;
  }, 0);
}

function highlightMatches(text: string, tokens: string[]): ReactNode {
  if (tokens.length === 0) {
    return text;
  }

  const matcher = new RegExp(`(${tokens.map(escapeRegExp).join("|")})`, "ig");
  const parts = text.split(matcher);

  if (parts.length === 1) {
    return text;
  }

  return parts.map((part, index) => {
    const isMatch = tokens.some((token) => token === part.toLowerCase());
    if (isMatch) {
      return <mark key={`${part}-${index}`}>{part}</mark>;
    }
    return <span key={`${part}-${index}`}>{part}</span>;
  });
}

export default function App() {
  const [query, setQuery] = useState("");
  const [category, setCategory] = useState<CategoryFilter>("All");
  const [level, setLevel] = useState<LevelFilter>("All");
  const [locale, setLocale] = useState<Locale>("en");
  const [themeMode, setThemeMode] = useState<ThemeMode>("system");
  const [systemTheme, setSystemTheme] = useState<ResolvedTheme>("dark");
  const [isPaletteOpen, setPaletteOpen] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");
  const [paletteIndex, setPaletteIndex] = useState(0);
  const [activeSection, setActiveSection] = useState<(typeof sectionNav)[number]["id"]>(
    sectionNav[0].id
  );
  const [scrollProgress, setScrollProgress] = useState(0);

  const searchInputRef = useRef<HTMLInputElement>(null);
  const paletteInputRef = useRef<HTMLInputElement>(null);

  const copy = textBundle[locale];
  const resolvedTheme: ResolvedTheme =
    themeMode === "system" ? systemTheme : themeMode;

  const featuredDocs = useMemo(
    () => docsCatalog.filter((doc) => doc.featured).slice(0, 4),
    []
  );

  const queryTokens = useMemo(() => tokenizeQuery(query), [query]);
  const paletteTokens = useMemo(() => tokenizeQuery(paletteQuery), [paletteQuery]);

  const docsLookup = useMemo(() => {
    return new Map(docsCatalog.map((doc) => [doc.path, doc] as const));
  }, []);

  const filteredDocs = useMemo(() => {
    const ranked = docsCatalog.map((doc) => ({
      doc,
      score: scoreDoc(doc, queryTokens, locale),
    }));

    return ranked
      .filter(({ doc, score }) => {
        const categoryMatch = category === "All" || doc.category === category;
        if (!categoryMatch) {
          return false;
        }

        const levelMatch = level === "All" || doc.level === level;
        if (!levelMatch) {
          return false;
        }

        if (queryTokens.length === 0) {
          return true;
        }

        return score > 0;
      })
      .sort((left, right) => {
        if (right.score !== left.score) {
          return right.score - left.score;
        }
        if (left.doc.featured !== right.doc.featured) {
          return left.doc.featured ? -1 : 1;
        }
        if (left.doc.level !== right.doc.level) {
          return left.doc.level === "Core" ? -1 : 1;
        }
        return localize(left.doc.title, locale).localeCompare(
          localize(right.doc.title, locale),
          locale === "zh" ? "zh-Hans" : "en"
        );
      })
      .map(({ doc }) => doc);
  }, [category, level, locale, queryTokens]);

  const categoryCounts = useMemo(() => {
    return categories.reduce((acc, currentCategory) => {
      const matched = docsCatalog.filter((doc) => {
        if (doc.category !== currentCategory) {
          return false;
        }
        if (level !== "All" && doc.level !== level) {
          return false;
        }
        if (queryTokens.length === 0) {
          return true;
        }
        return scoreDoc(doc, queryTokens, locale) > 0;
      });

      acc[currentCategory] = matched.length;
      return acc;
    }, {} as Record<DocCategory, number>);
  }, [level, locale, queryTokens]);

  const totalCategoryCount = useMemo(
    () => Object.values(categoryCounts).reduce((sum, current) => sum + current, 0),
    [categoryCounts]
  );

  const hasActiveFilters =
    queryTokens.length > 0 || category !== "All" || level !== "All";

  const topMatches = useMemo(() => {
    if (!hasActiveFilters) {
      return [];
    }
    return filteredDocs.slice(0, 4);
  }, [filteredDocs, hasActiveFilters]);

  const docsByCategory = useMemo(() => {
    return categories.reduce((acc, currentCategory) => {
      acc[currentCategory] = filteredDocs.filter(
        (doc) => doc.category === currentCategory
      );
      return acc;
    }, {} as Record<DocCategory, DocEntry[]>);
  }, [filteredDocs]);

  const paletteActions = useMemo<PaletteAction[]>(() => {
    const openExternal = (path: string) => {
      window.open(getDocUrl(path), "_blank", "noopener,noreferrer");
    };

    return [
      {
        id: "jump-docs",
        label: copy.actionJumpDocs,
        hint: copy.actionJumpDocsHint,
        keywords: ["docs", "navigator", "文档", "导航"],
        run: () => {
          document
            .getElementById("docs-navigator")
            ?.scrollIntoView({ behavior: "smooth", block: "start" });
        },
      },
      {
        id: "open-docs-home",
        label: copy.actionOpenDocsHome,
        hint: copy.actionOpenDocsHomeHint,
        keywords: ["docs", "home", "文档", "首页"],
        run: () => {
          openExternal("docs/README.md");
        },
      },
      {
        id: "open-repo",
        label: copy.actionOpenRepo,
        hint: copy.actionOpenRepoHint,
        keywords: ["repo", "github", "仓库"],
        run: () => {
          openExternal("README.md");
        },
      },
      {
        id: "switch-language",
        label: copy.actionToggleLang,
        hint: copy.actionToggleLangHint,
        keywords: ["language", "locale", "语言", "中英"],
        run: () => {
          setLocale((current) => (current === "en" ? "zh" : "en"));
        },
      },
      {
        id: "theme-dark",
        label: copy.actionThemeDark,
        hint: copy.actionThemeHint,
        keywords: ["theme", "dark", "深色", "主题"],
        run: () => {
          setThemeMode("dark");
        },
      },
      {
        id: "theme-light",
        label: copy.actionThemeLight,
        hint: copy.actionThemeHint,
        keywords: ["theme", "light", "浅色", "主题"],
        run: () => {
          setThemeMode("light");
        },
      },
      {
        id: "theme-system",
        label: copy.actionThemeSystem,
        hint: copy.actionThemeHint,
        keywords: ["theme", "auto", "system", "自动"],
        run: () => {
          setThemeMode("system");
        },
      },
    ];
  }, [copy]);

  const filteredPaletteActions = useMemo(() => {
    if (paletteTokens.length === 0) {
      return paletteActions;
    }

    return paletteActions.filter((action) => {
      const haystack = `${action.label} ${action.hint} ${action.keywords.join(" ")}`.toLowerCase();
      return paletteTokens.every((token) => haystack.includes(token));
    });
  }, [paletteActions, paletteTokens]);

  const paletteDocResults = useMemo(() => {
    const ranked = docsCatalog.map((doc) => ({
      doc,
      score: scoreDoc(doc, paletteTokens, locale),
    }));

    return ranked
      .filter(({ doc, score }) => {
        if (paletteTokens.length === 0) {
          return doc.featured || doc.level === "Core";
        }
        return score > 0;
      })
      .sort((left, right) => {
        if (right.score !== left.score) {
          return right.score - left.score;
        }
        if (left.doc.featured !== right.doc.featured) {
          return left.doc.featured ? -1 : 1;
        }
        return localize(left.doc.title, locale).localeCompare(
          localize(right.doc.title, locale),
          locale === "zh" ? "zh-Hans" : "en"
        );
      })
      .slice(0, 8)
      .map(({ doc }) => doc);
  }, [locale, paletteTokens]);

  const actionEntries = useMemo<PaletteEntry[]>(() => {
    return filteredPaletteActions.map((action) => ({
      id: `action-${action.id}`,
      kind: "action",
      label: action.label,
      hint: action.hint,
      run: action.run,
    }));
  }, [filteredPaletteActions]);

  const docEntries = useMemo<PaletteEntry[]>(() => {
    return paletteDocResults.map((doc) => ({
      id: `doc-${doc.path}`,
      kind: "doc",
      label: localize(doc.title, locale),
      hint: localize(doc.summary, locale),
      meta: doc.path,
      run: () => {
        window.open(getDocUrl(doc.path), "_blank", "noopener,noreferrer");
      },
    }));
  }, [locale, paletteDocResults]);

  const paletteEntries = useMemo(
    () => [...actionEntries, ...docEntries],
    [actionEntries, docEntries]
  );

  const activePaletteEntry = paletteEntries[paletteIndex] ?? null;

  useEffect(() => {
    const media = window.matchMedia("(prefers-color-scheme: dark)");
    const updateSystemTheme = () => {
      setSystemTheme(media.matches ? "dark" : "light");
    };

    updateSystemTheme();
    media.addEventListener("change", updateSystemTheme);

    return () => {
      media.removeEventListener("change", updateSystemTheme);
    };
  }, []);

  useEffect(() => {
    const searchLang = new URL(window.location.href).searchParams.get("lang");
    const storedLocale = window.localStorage.getItem("zeroclaw.locale");

    if (searchLang === "en" || searchLang === "zh") {
      setLocale(searchLang);
    } else if (storedLocale === "en" || storedLocale === "zh") {
      setLocale(storedLocale);
    } else if (navigator.language.toLowerCase().startsWith("zh")) {
      setLocale("zh");
    }

    const storedTheme = window.localStorage.getItem("zeroclaw.theme");
    if (storedTheme === "system" || storedTheme === "dark" || storedTheme === "light") {
      setThemeMode(storedTheme);
    }
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = resolvedTheme;
    document.documentElement.style.colorScheme = resolvedTheme;
    window.localStorage.setItem("zeroclaw.theme", themeMode);
  }, [resolvedTheme, themeMode]);

  useEffect(() => {
    document.documentElement.lang = locale === "zh" ? "zh-CN" : "en";
    window.localStorage.setItem("zeroclaw.locale", locale);

    const url = new URL(window.location.href);
    if (url.searchParams.get("lang") !== locale) {
      url.searchParams.set("lang", locale);
      window.history.replaceState({}, "", `${url.pathname}${url.search}${url.hash}`);
    }
  }, [locale]);

  useEffect(() => {
    const updateProgress = () => {
      const scrollHeight =
        document.documentElement.scrollHeight - window.innerHeight;
      if (scrollHeight <= 0) {
        setScrollProgress(0);
        return;
      }

      setScrollProgress(Math.min(1, Math.max(0, window.scrollY / scrollHeight)));
    };

    updateProgress();
    window.addEventListener("scroll", updateProgress, { passive: true });
    window.addEventListener("resize", updateProgress);

    return () => {
      window.removeEventListener("scroll", updateProgress);
      window.removeEventListener("resize", updateProgress);
    };
  }, []);

  useEffect(() => {
    const nodes = sectionNav
      .map((section) => document.getElementById(section.id))
      .filter((node): node is HTMLElement => Boolean(node));

    if (nodes.length === 0 || !("IntersectionObserver" in window)) {
      return;
    }

    const observer = new IntersectionObserver(
      (entries) => {
        const inView = entries
          .filter((entry) => entry.isIntersecting)
          .sort((left, right) => right.intersectionRatio - left.intersectionRatio);

        if (inView.length > 0) {
          setActiveSection(inView[0].target.id as (typeof sectionNav)[number]["id"]);
        }
      },
      {
        threshold: [0.12, 0.3, 0.55, 0.85],
        rootMargin: "-28% 0px -58% 0px",
      }
    );

    nodes.forEach((node) => observer.observe(node));
    return () => observer.disconnect();
  }, []);

  useEffect(() => {
    const prefersReducedMotion = window.matchMedia(
      "(prefers-reduced-motion: reduce)"
    ).matches;
    const revealNodes = document.querySelectorAll<HTMLElement>(".reveal");
    let observer: IntersectionObserver | null = null;

    if (prefersReducedMotion || !("IntersectionObserver" in window)) {
      revealNodes.forEach((node) => node.classList.add("in"));
    } else {
      observer = new IntersectionObserver(
        (entries) => {
          entries.forEach((entry) => {
            if (entry.isIntersecting) {
              entry.target.classList.add("in");
              observer?.unobserve(entry.target);
            }
          });
        },
        { threshold: 0.12 }
      );
      revealNodes.forEach((node) => observer?.observe(node));
    }

    const root = document.documentElement;
    const handlePointerMove = (event: PointerEvent) => {
      root.style.setProperty("--pointer-x", `${event.clientX}px`);
      root.style.setProperty("--pointer-y", `${event.clientY}px`);
    };

    if (!prefersReducedMotion) {
      window.addEventListener("pointermove", handlePointerMove, {
        passive: true,
      });
    }

    return () => {
      observer?.disconnect();
      if (!prefersReducedMotion) {
        window.removeEventListener("pointermove", handlePointerMove);
      }
    };
  }, []);

  useEffect(() => {
    if (!isPaletteOpen) {
      return;
    }

    const timer = window.setTimeout(() => {
      paletteInputRef.current?.focus();
      paletteInputRef.current?.select();
    }, 20);

    const previousOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";

    return () => {
      window.clearTimeout(timer);
      document.body.style.overflow = previousOverflow;
    };
  }, [isPaletteOpen]);

  useEffect(() => {
    if (!isPaletteOpen) {
      return;
    }
    setPaletteIndex(0);
  }, [actionEntries.length, docEntries.length, isPaletteOpen, paletteQuery]);

  useEffect(() => {
    if (paletteEntries.length === 0 && paletteIndex !== 0) {
      setPaletteIndex(0);
      return;
    }

    if (paletteIndex > paletteEntries.length - 1) {
      setPaletteIndex(Math.max(0, paletteEntries.length - 1));
    }
  }, [paletteEntries.length, paletteIndex]);

  useEffect(() => {
    if (!isPaletteOpen) {
      return;
    }

    const activeNode = document.querySelector<HTMLElement>(
      `[data-palette-index="${paletteIndex}"]`
    );
    activeNode?.scrollIntoView({ block: "nearest" });
  }, [isPaletteOpen, paletteIndex]);

  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      const key = event.key.toLowerCase();
      const target = event.target as HTMLElement | null;
      const isEditable =
        target instanceof HTMLInputElement ||
        target instanceof HTMLTextAreaElement ||
        Boolean(target?.isContentEditable);

      if ((event.metaKey || event.ctrlKey) && key === "k") {
        event.preventDefault();
        setPaletteOpen((open) => {
          const next = !open;
          if (!open) {
            setPaletteIndex(0);
          }
          return next;
        });
        return;
      }

      if (event.key === "Escape") {
        if (isPaletteOpen) {
          event.preventDefault();
          setPaletteOpen(false);
          setPaletteQuery("");
          setPaletteIndex(0);
          return;
        }

        if (target === searchInputRef.current) {
          if (query || category !== "All" || level !== "All") {
            setQuery("");
            setCategory("All");
            setLevel("All");
          }
          searchInputRef.current?.blur();
        }
      }

      if (isPaletteOpen && (event.key === "ArrowDown" || event.key === "ArrowUp")) {
        if (paletteEntries.length === 0) {
          return;
        }
        event.preventDefault();
        setPaletteIndex((currentIndex) => {
          if (event.key === "ArrowDown") {
            return (currentIndex + 1) % paletteEntries.length;
          }
          return (currentIndex - 1 + paletteEntries.length) % paletteEntries.length;
        });
        return;
      }

      if (isPaletteOpen && event.key === "Tab") {
        if (paletteEntries.length === 0) {
          return;
        }
        event.preventDefault();
        setPaletteIndex((currentIndex) => {
          if (event.shiftKey) {
            return (currentIndex - 1 + paletteEntries.length) % paletteEntries.length;
          }
          return (currentIndex + 1) % paletteEntries.length;
        });
        return;
      }

      if (isPaletteOpen && event.key === "Enter" && paletteEntries.length > 0) {
        event.preventDefault();
        const selected = paletteEntries[paletteIndex] ?? paletteEntries[0];
        selected?.run();
        setPaletteOpen(false);
        setPaletteQuery("");
        setPaletteIndex(0);
        return;
      }

      if (event.key === "/" && !isEditable && !isPaletteOpen) {
        event.preventDefault();
        searchInputRef.current?.focus();
        searchInputRef.current?.select();
      }
    };

    window.addEventListener("keydown", handleKeyDown);
    return () => {
      window.removeEventListener("keydown", handleKeyDown);
    };
  }, [category, isPaletteOpen, level, paletteEntries, paletteIndex, query]);

  const closePalette = () => {
    setPaletteOpen(false);
    setPaletteQuery("");
    setPaletteIndex(0);
  };

  const resetFilters = () => {
    setQuery("");
    setCategory("All");
    setLevel("All");
    searchInputRef.current?.focus();
  };

  return (
    <div className="app-shell">
      <div className="ambient-grid" aria-hidden="true" />
      <div className="ambient-glow" aria-hidden="true" />
      <div className="cursor-glow" aria-hidden="true" />
      <div className="scroll-progress" aria-hidden="true">
        <i style={{ transform: `scaleX(${scrollProgress})` }} />
      </div>

      <nav className="section-rail" aria-label={copy.sectionRailLabel}>
        {sectionNav.map((section) => (
          <a
            key={section.id}
            href={`#${section.id}`}
            className={`rail-link ${activeSection === section.id ? "active" : ""}`}
            onClick={() => setActiveSection(section.id)}
          >
            <span className="rail-dot" aria-hidden="true" />
            <span>{localize(section.label, locale)}</span>
          </a>
        ))}
      </nav>

      <header className="topbar">
        <div className="container topbar-inner">
          <a className="wordmark" href="#">
            ZeroClaw
          </a>

          <div className="top-actions">
            <a className="top-link" href="#docs-navigator">
              {copy.navDocs}
            </a>
            <a className="top-link" href={getDocUrl("README.md")} target="_blank" rel="noreferrer">
              {copy.navRepo}
            </a>
            <button
              type="button"
              className="top-link top-link-btn"
              onClick={() => {
                setPaletteOpen(true);
                setPaletteIndex(0);
              }}
            >
              {copy.openPalette}
              <span className="top-shortcut">Ctrl/Cmd+K</span>
            </button>

            <div className="toggle-cluster" role="group" aria-label={copy.languageLabel}>
              {(["en", "zh"] as const).map((item) => (
                <button
                  key={item}
                  type="button"
                  className={`toggle-chip ${locale === item ? "active" : ""}`}
                  onClick={() => setLocale(item)}
                  aria-pressed={locale === item}
                >
                  {languageLabels[locale][item]}
                </button>
              ))}
            </div>

            <div className="toggle-cluster" role="group" aria-label={copy.themeLabel}>
              {(["system", "dark", "light"] as const).map((mode) => (
                <button
                  key={mode}
                  type="button"
                  className={`toggle-chip ${themeMode === mode ? "active" : ""}`}
                  onClick={() => setThemeMode(mode)}
                  aria-pressed={themeMode === mode}
                >
                  {themeLabels[locale][mode]}
                </button>
              ))}
            </div>

            <span className="status-pill" title={`theme:${resolvedTheme}`}>
              {copy.statusPill}
            </span>
          </div>
        </div>
      </header>

      <main className="container main-layout">
        <section id="hero-core" className="hero hero-grid">
          <div className="hero-copy">
            <p className="hero-kicker reveal">{copy.heroKicker}</p>
            <h1 className="hero-title reveal delay-1">
              {copy.heroTitle[0]}
              <br />
              {copy.heroTitle[1]}
              <br />
              {copy.heroTitle[2]}
            </h1>
            <p className="hero-lead reveal delay-2">{copy.heroLead}</p>

            <div className="hero-actions reveal delay-3">
              <a className="btn btn-primary" href="#docs-navigator">
                {copy.browseDocsNow}
              </a>
              <a
                className="btn"
                href={getDocUrl("docs/README.md")}
                target="_blank"
                rel="noreferrer"
              >
                {copy.openDocsHome}
              </a>
            </div>

            <dl className="metrics reveal delay-4">
              {copy.metrics.map((metric) => (
                <div className="metric" key={metric.label}>
                  <dt>{metric.label}</dt>
                  <dd>{metric.value}</dd>
                </div>
              ))}
            </dl>
          </div>

          <aside className="command-deck reveal delay-2">
            <header className="deck-head">
              <span>{copy.runtimeLabel}</span>
              <span className="live-pill">{copy.live}</span>
            </header>

            <div className="deck-body">
              {signalFeed.map((line, index) => (
                <p
                  key={line.en}
                  className="feed-row"
                  style={{ animationDelay: `${index * 120}ms` } as CSSProperties}
                >
                  <span className="feed-index">0{index + 1}</span>
                  <span>{localize(line, locale)}</span>
                </p>
              ))}
            </div>

            <div className="deck-bars">
              {executionPhases.map((phase, index) => (
                <div key={phase.phase.en} className="deck-bar-row">
                  <span>{localize(phase.phase, locale)}</span>
                  <span className="bar-track">
                    <i
                      style={
                        {
                          width: `${Math.min(95, 58 + index * 11)}%`,
                          animationDelay: `${index * 160}ms`,
                        } as CSSProperties
                      }
                    />
                  </span>
                </div>
              ))}
            </div>
          </aside>
        </section>

        <section className="trait-band reveal">
          {traitChips.map((chip, index) => (
            <span key={chip.en} className={`trait-chip delay-${Math.min(index + 1, 4)}`}>
              {localize(chip, locale)}
            </span>
          ))}
        </section>

        <section id="core-shortcuts" className="glass-panel">
          <div className="section-head">
            <h2 className="section-title reveal">{copy.coreDocsShortcuts}</h2>
            <a className="section-head-link reveal delay-1" href="#docs-navigator">
              {copy.openFullNavigator}
            </a>
          </div>
          <div className="links-grid">
            {featuredDocs.map((doc, index) => (
              <a
                key={doc.path}
                href={getDocUrl(doc.path)}
                className={`doc-link reveal delay-${Math.min(index + 1, 4)}`}
                target="_blank"
                rel="noreferrer"
              >
                <span>{localize(doc.title, locale)}</span>
                <svg viewBox="0 0 24 24" aria-hidden="true">
                  <path d="M7 17L17 7M17 7H8M17 7V16" />
                </svg>
              </a>
            ))}
          </div>
        </section>

        <section id="quick-paths" className="glass-panel">
          <div className="section-head">
            <h2 className="section-title reveal">{copy.quickStartPaths}</h2>
            <span className="section-tag reveal delay-1">{copy.taskFirstRouting}</span>
          </div>
          <div className="path-grid">
            {quickPaths.map((path, index) => (
              <article
                key={path.title.en}
                className={`path-card reveal delay-${Math.min(index + 1, 4)}`}
              >
                <h3>{localize(path.title, locale)}</h3>
                <p>{localize(path.summary, locale)}</p>
                <div className="path-docs">
                  {path.docs.map((docPath) => {
                    const doc = docsLookup.get(docPath);
                    if (!doc) {
                      return null;
                    }
                    return (
                      <a
                        key={doc.path}
                        href={getDocUrl(doc.path)}
                        target="_blank"
                        rel="noreferrer"
                        className="path-doc-link"
                      >
                        <span>{localize(doc.title, locale)}</span>
                        <code>{doc.path}</code>
                      </a>
                    );
                  })}
                </div>
              </article>
            ))}
          </div>
        </section>

        <section id="capability-stack" className="glass-panel">
          <h2 className="section-title reveal">{copy.capabilityStack}</h2>
          <div className="cap-grid">
            {capabilities.map((cap, index) => (
              <article
                key={cap.title.en}
                className={`cap-card reveal delay-${Math.min(index + 1, 4)}`}
              >
                <h3>{localize(cap.title, locale)}</h3>
                <p>{localize(cap.text, locale)}</p>
              </article>
            ))}
          </div>
        </section>

        <section id="execution-lattice" className="workflow-grid">
          <article className="glass-panel">
            <h2 className="section-title reveal">{copy.executionLattice}</h2>
            <ol className="phase-list">
              {executionPhases.map((phase, index) => (
                <li
                  key={phase.phase.en}
                  className={`phase-item reveal delay-${Math.min(index + 1, 4)}`}
                >
                  <span className="phase-num">{index + 1}</span>
                  <div className="phase-text">
                    <h3>{localize(phase.phase, locale)}</h3>
                    <p>{localize(phase.detail, locale)}</p>
                  </div>
                </li>
              ))}
            </ol>
          </article>

          <aside className="glass-panel monitor-panel reveal delay-2">
            <h2 className="section-title">{copy.operatorSignal}</h2>
            <p className="monitor-intro">{copy.operatorSignalIntro}</p>
            <div className="monitor-grid">
              {monitorStats.map((stat) => (
                <div key={stat.label.en}>
                  <span>{localize(stat.label, locale)}</span>
                  <strong>{localize(stat.value, locale)}</strong>
                </div>
              ))}
            </div>
          </aside>
        </section>

        <section id="docs-navigator" className="glass-panel docs-panel">
          <h2 className="section-title reveal">{copy.docsNavigator}</h2>
          <p className="docs-intro reveal delay-1">{copy.docsIntro}</p>

          <div className="docs-tools reveal delay-1">
            <label className="docs-search-wrap" htmlFor="docs-search-input">
              <span className="docs-search-label">{copy.searchDocs}</span>
              <input
                id="docs-search-input"
                ref={searchInputRef}
                value={query}
                onChange={(event) => setQuery(event.target.value)}
                placeholder={copy.searchPlaceholder}
                autoComplete="off"
              />
            </label>

            <div className="docs-filter-stack">
              <div className="category-row" role="tablist" aria-label="Doc category filters">
                {(["All", ...categories] as const).map((item) => (
                  <button
                    key={item}
                    type="button"
                    className={`category-pill ${category === item ? "active" : ""}`}
                    onClick={() => setCategory(item)}
                    aria-pressed={category === item}
                  >
                    <span>
                      {item === "All" ? levelLabels[locale].All : categoryLabels[locale][item]}
                    </span>
                    <small className="pill-count">
                      {item === "All" ? totalCategoryCount : categoryCounts[item]}
                    </small>
                  </button>
                ))}
              </div>

              <div className="category-row level-row" role="tablist" aria-label="Doc level filters">
                {levelFilterOptions.map((item) => (
                  <button
                    key={item}
                    type="button"
                    className={`category-pill level-pill ${level === item ? "active" : ""}`}
                    onClick={() => setLevel(item)}
                    aria-pressed={level === item}
                  >
                    {levelLabels[locale][item]}
                  </button>
                ))}
              </div>
            </div>

            <div className="docs-utility-row">
              <p className="docs-hint">
                {copy.quickKeys} <kbd>/</kbd> {copy.quickKeySearchHint}, <kbd>Esc</kbd>{" "}
                {copy.quickKeyResetHint}.
              </p>
              <button
                type="button"
                className="reset-filters-btn"
                onClick={resetFilters}
                disabled={!hasActiveFilters}
              >
                {copy.resetFilters}
              </button>
            </div>
          </div>

          <p className="docs-count reveal delay-2">
            {copy.docsCount(filteredDocs.length, hasActiveFilters)}
          </p>

          {topMatches.length > 0 && (
            <section className="docs-top-matches reveal in delay-2">
              <h3 className="docs-group-title">{copy.topMatches}</h3>
              <div className="docs-grid">
                {topMatches.map((doc) => (
                  <a
                    key={`top-${doc.path}`}
                    href={getDocUrl(doc.path)}
                    className="doc-nav-card doc-nav-card-priority"
                    target="_blank"
                    rel="noreferrer"
                  >
                    <div className="doc-nav-head">
                      <strong>{highlightMatches(localize(doc.title, locale), queryTokens)}</strong>
                      <span>{levelLabels[locale][doc.level]}</span>
                    </div>
                    <p>{highlightMatches(localize(doc.summary, locale), queryTokens)}</p>
                    <code>{highlightMatches(doc.path, queryTokens)}</code>
                  </a>
                ))}
              </div>
            </section>
          )}

          <div className="docs-groups">
            {categories.map((cat, catIndex) => {
              const docs = docsByCategory[cat];
              if (docs.length === 0) {
                return null;
              }
              return (
                <section
                  key={cat}
                  className={`docs-group reveal delay-${Math.min(catIndex + 1, 4)}`}
                >
                  <h3 className="docs-group-title">{categoryLabels[locale][cat]}</h3>
                  <div className="docs-grid">
                    {docs.map((doc) => (
                      <a
                        key={doc.path}
                        href={getDocUrl(doc.path)}
                        className="doc-nav-card"
                        target="_blank"
                        rel="noreferrer"
                      >
                        <div className="doc-nav-head">
                          <strong>{highlightMatches(localize(doc.title, locale), queryTokens)}</strong>
                          <span>{levelLabels[locale][doc.level]}</span>
                        </div>
                        <p>{highlightMatches(localize(doc.summary, locale), queryTokens)}</p>
                        <code>{highlightMatches(doc.path, queryTokens)}</code>
                      </a>
                    ))}
                  </div>
                </section>
              );
            })}

            {filteredDocs.length === 0 && (
              <div className="doc-empty reveal in" role="status" aria-live="polite">
                {copy.noMatchPrefix}
                <code> {copy.noMatchA} </code>
                {copy.noMatchConnector}
                <code> {copy.noMatchB} </code>.
              </div>
            )}
          </div>

          <footer className="panel-footer reveal delay-4">{copy.panelFooter}</footer>
        </section>
      </main>

      <div className="mobile-dock">
        <a className="dock-btn" href="#docs-navigator">
          {copy.mobileDockDocs}
        </a>
        <button
          type="button"
          className="dock-btn"
          onClick={() => {
            setPaletteOpen(true);
            setPaletteIndex(0);
          }}
        >
          {copy.mobileDockPalette}
        </button>
      </div>

      {isPaletteOpen && (
        <div className="palette-overlay" role="presentation" onClick={closePalette}>
          <section
            className="palette-panel"
            role="dialog"
            aria-modal="true"
            aria-label={copy.paletteTitle}
            onClick={(event) => event.stopPropagation()}
          >
            <header className="palette-head">
              <input
                ref={paletteInputRef}
                value={paletteQuery}
                onChange={(event) => setPaletteQuery(event.target.value)}
                placeholder={copy.palettePlaceholder}
                aria-label={copy.palettePlaceholder}
              />
              <button type="button" className="palette-close" onClick={closePalette}>
                {copy.paletteClose}
              </button>
            </header>

            <div className="palette-grid">
              <div
                className="palette-results"
                role="listbox"
                aria-activedescendant={
                  activePaletteEntry ? `palette-option-${activePaletteEntry.id}` : undefined
                }
              >
                <section>
                  <p className="palette-section-title">
                    {copy.paletteActionsGroup}
                    <small>{actionEntries.length}</small>
                  </p>
                  <div className="palette-list">
                    {actionEntries.map((entry, index) => (
                      <button
                        type="button"
                        id={`palette-option-${entry.id}`}
                        key={entry.id}
                        data-palette-index={index}
                        className={`palette-item ${paletteIndex === index ? "active" : ""}`}
                        role="option"
                        aria-selected={paletteIndex === index}
                        onMouseEnter={() => setPaletteIndex(index)}
                        onClick={() => {
                          entry.run();
                          closePalette();
                        }}
                      >
                        <span className="palette-item-main">
                          {highlightMatches(entry.label, paletteTokens)}
                        </span>
                        <small>{highlightMatches(entry.hint, paletteTokens)}</small>
                      </button>
                    ))}
                  </div>
                </section>

                <section>
                  <p className="palette-section-title">
                    {copy.paletteDocsGroup}
                    <small>{docEntries.length}</small>
                  </p>
                  <div className="palette-list">
                    {docEntries.map((entry, index) => {
                      const globalIndex = actionEntries.length + index;
                      return (
                        <button
                          type="button"
                          id={`palette-option-${entry.id}`}
                          key={entry.id}
                          data-palette-index={globalIndex}
                          className={`palette-item palette-doc-item ${paletteIndex === globalIndex ? "active" : ""}`}
                          role="option"
                          aria-selected={paletteIndex === globalIndex}
                          onMouseEnter={() => setPaletteIndex(globalIndex)}
                          onClick={() => {
                            entry.run();
                            closePalette();
                          }}
                        >
                          <span className="palette-item-main">
                            {highlightMatches(entry.label, paletteTokens)}
                          </span>
                          <small>{highlightMatches(entry.hint, paletteTokens)}</small>
                          <code>{highlightMatches(entry.meta ?? "", paletteTokens)}</code>
                        </button>
                      );
                    })}
                  </div>
                </section>

                {paletteEntries.length === 0 && (
                  <p className="palette-empty">{copy.paletteNoResults}</p>
                )}
              </div>

              <aside className="palette-preview" aria-live="polite">
                <p className="palette-preview-title">{copy.palettePreviewTitle}</p>

                {activePaletteEntry ? (
                  <div className="preview-card">
                    <p className="preview-kind">
                      {activePaletteEntry.kind === "action"
                        ? copy.palettePreviewKindAction
                        : copy.palettePreviewKindDoc}
                    </p>
                    <h3>{highlightMatches(activePaletteEntry.label, paletteTokens)}</h3>
                    <p>{highlightMatches(activePaletteEntry.hint, paletteTokens)}</p>
                    {activePaletteEntry.meta && (
                      <code>{highlightMatches(activePaletteEntry.meta, paletteTokens)}</code>
                    )}
                    {activePaletteEntry.kind === "doc" && (
                      <span className="preview-open-hint">{copy.palettePreviewOpenHint}</span>
                    )}
                  </div>
                ) : (
                  <p className="palette-preview-empty">{copy.palettePreviewNoSelection}</p>
                )}

                <div className="palette-kbd-hints">
                  <span>{copy.paletteHintNavigate}</span>
                  <span>{copy.paletteHintRun}</span>
                  <span>{copy.paletteHintClose}</span>
                </div>
              </aside>
            </div>
          </section>
        </div>
      )}
    </div>
  );
}
