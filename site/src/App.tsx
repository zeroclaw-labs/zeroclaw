import {
  isValidElement,
  useEffect,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from "react";
import ReactMarkdown from "react-markdown";
import remarkGfm from "remark-gfm";

type Locale = "en" | "zh";
type ThemeMode = "system" | "dark" | "light";
type ResolvedTheme = "dark" | "light";
type Category =
  | "Core"
  | "Setup"
  | "Operations"
  | "Security"
  | "Reference"
  | "International";

type Localized = Record<Locale, string>;

type DocEntry = {
  id: string;
  category: Category;
  path: string;
  zhPath?: string;
  title: Localized;
  summary: Localized;
  keywords?: string[];
};

type PaletteEntry = {
  id: string;
  label: string;
  hint: string;
  run: () => void;
};

const repoBase = "https://github.com/zeroclaw-labs/zeroclaw/blob/main";
const rawBase = "https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main";

const docs: DocEntry[] = [
  {
    id: "repo-readme",
    category: "Core",
    path: "README.md",
    zhPath: "README.zh-CN.md",
    title: { en: "Repository README", zh: "仓库 README" },
    summary: {
      en: "Project overview, architecture, benchmarks, setup, and operations.",
      zh: "项目总览、架构、基准、安装与运维入口。",
    },
    keywords: ["overview", "architecture", "benchmark", "quick start"],
  },
  {
    id: "docs-home",
    category: "Core",
    path: "docs/README.md",
    zhPath: "docs/i18n/zh-CN/README.md",
    title: { en: "Docs Hub", zh: "文档总览" },
    summary: {
      en: "Primary documentation hub for all ZeroClaw capabilities.",
      zh: "ZeroClaw 全量文档的总入口。",
    },
    keywords: ["docs", "hub", "summary"],
  },
  {
    id: "docs-summary",
    category: "Core",
    path: "docs/SUMMARY.md",
    zhPath: "docs/i18n/zh-CN/SUMMARY.md",
    title: { en: "Docs Table of Contents", zh: "文档目录" },
    summary: {
      en: "Structured index for all docs sections and files.",
      zh: "结构化文档目录与索引。",
    },
    keywords: ["toc", "summary", "index"],
  },
  {
    id: "one-click-bootstrap",
    category: "Setup",
    path: "docs/one-click-bootstrap.md",
    zhPath: "docs/i18n/zh-CN/one-click-bootstrap.md",
    title: { en: "One-Click Bootstrap", zh: "一键安装" },
    summary: {
      en: "Fast installer path for dependencies and ZeroClaw runtime.",
      zh: "快速完成依赖与 ZeroClaw 运行时安装。",
    },
  },
  {
    id: "getting-started",
    category: "Setup",
    path: "docs/getting-started/README.md",
    title: { en: "Getting Started", zh: "快速开始" },
    summary: {
      en: "Boot sequence, first commands, and onboarding flow.",
      zh: "启动流程、首批命令与引导步骤。",
    },
  },
  {
    id: "network-deployment",
    category: "Setup",
    path: "docs/network-deployment.md",
    zhPath: "docs/i18n/zh-CN/network-deployment.md",
    title: { en: "Network Deployment", zh: "网络部署" },
    summary: {
      en: "Service setup, daemon/gateway, and network run modes.",
      zh: "服务配置、daemon/gateway 与网络运行模式。",
    },
  },
  {
    id: "hardware",
    category: "Setup",
    path: "docs/hardware/README.md",
    title: { en: "Hardware Guide", zh: "硬件指南" },
    summary: {
      en: "Board and hardware references for edge deployment.",
      zh: "边缘部署相关板卡与硬件参考。",
    },
  },
  {
    id: "operations-runbook",
    category: "Operations",
    path: "docs/operations-runbook.md",
    zhPath: "docs/i18n/zh-CN/operations-runbook.md",
    title: { en: "Operations Runbook", zh: "运维手册" },
    summary: {
      en: "Operational procedures, incident handling, and checks.",
      zh: "运维流程、故障处理与巡检实践。",
    },
  },
  {
    id: "ops-overview",
    category: "Operations",
    path: "docs/operations/README.md",
    title: { en: "Operations Overview", zh: "运维概览" },
    summary: {
      en: "Operations section hub with runbooks and safeguards.",
      zh: "运维章节入口，包含 runbook 与保障策略。",
    },
  },
  {
    id: "connectivity-probes",
    category: "Operations",
    path: "docs/operations/connectivity-probes-runbook.md",
    title: { en: "Connectivity Probes", zh: "连通性探针" },
    summary: {
      en: "Probe workflow and diagnosis guidelines.",
      zh: "探针流程与诊断指南。",
    },
  },
  {
    id: "troubleshooting",
    category: "Operations",
    path: "docs/troubleshooting.md",
    zhPath: "docs/i18n/zh-CN/troubleshooting.md",
    title: { en: "Troubleshooting", zh: "问题排查" },
    summary: {
      en: "Systematic recovery checklist for common failures.",
      zh: "常见故障的系统化排查与恢复清单。",
    },
  },
  {
    id: "security-overview",
    category: "Security",
    path: "docs/security/README.md",
    title: { en: "Security Overview", zh: "安全概览" },
    summary: {
      en: "Security architecture, controls, and policy model.",
      zh: "安全架构、控制项与策略模型。",
    },
  },
  {
    id: "sandboxing",
    category: "Security",
    path: "docs/sandboxing.md",
    zhPath: "docs/i18n/zh-CN/sandboxing.md",
    title: { en: "Sandboxing", zh: "沙箱机制" },
    summary: {
      en: "Runtime sandbox boundaries and risk containment.",
      zh: "运行时沙箱边界与风险隔离。",
    },
  },
  {
    id: "agnostic-security",
    category: "Security",
    path: "docs/agnostic-security.md",
    zhPath: "docs/i18n/zh-CN/agnostic-security.md",
    title: { en: "Agnostic Security", zh: "模型无关安全" },
    summary: {
      en: "Provider-agnostic security stance and operating model.",
      zh: "面向多模型的统一安全基线与运行方式。",
    },
  },
  {
    id: "config-reference",
    category: "Reference",
    path: "docs/config-reference.md",
    zhPath: "docs/i18n/zh-CN/config-reference.md",
    title: { en: "Config Reference", zh: "配置参考" },
    summary: {
      en: "All runtime configuration fields and defaults.",
      zh: "运行时配置字段与默认值。",
    },
  },
  {
    id: "commands-reference",
    category: "Reference",
    path: "docs/commands-reference.md",
    zhPath: "docs/i18n/zh-CN/commands-reference.md",
    title: { en: "Commands Reference", zh: "命令参考" },
    summary: {
      en: "CLI command map for onboarding, runtime, and tooling.",
      zh: "覆盖引导、运行时与工具的 CLI 命令总览。",
    },
  },
  {
    id: "custom-providers",
    category: "Reference",
    path: "docs/custom-providers.md",
    zhPath: "docs/i18n/zh-CN/custom-providers.md",
    title: { en: "Custom Providers", zh: "自定义模型提供方" },
    summary: {
      en: "OpenAI-compatible and custom endpoint integration.",
      zh: "OpenAI 兼容与自定义端点集成指南。",
    },
  },
  {
    id: "channels-reference",
    category: "Reference",
    path: "docs/channels-reference.md",
    zhPath: "docs/i18n/zh-CN/channels-reference.md",
    title: { en: "Channels Reference", zh: "渠道参考" },
    summary: {
      en: "Slack/Telegram/Discord/WhatsApp and channel wiring.",
      zh: "Slack/Telegram/Discord/WhatsApp 等渠道配置。",
    },
  },
  {
    id: "reference-overview",
    category: "Reference",
    path: "docs/reference/README.md",
    title: { en: "Reference Overview", zh: "参考总览" },
    summary: {
      en: "Reference section index across runtime internals.",
      zh: "运行时内部参考索引。",
    },
  },
  {
    id: "resource-limits",
    category: "Reference",
    path: "docs/resource-limits.md",
    zhPath: "docs/i18n/zh-CN/resource-limits.md",
    title: { en: "Resource Limits", zh: "资源限制" },
    summary: {
      en: "CPU, memory, and execution constraints guide.",
      zh: "CPU、内存与执行约束说明。",
    },
  },
  {
    id: "i18n-guide",
    category: "International",
    path: "docs/i18n-guide.md",
    zhPath: "docs/i18n/zh-CN/i18n-guide.md",
    title: { en: "i18n Guide", zh: "国际化指南" },
    summary: {
      en: "Localization strategy and docs translation workflow.",
      zh: "本地化策略与文档翻译流程。",
    },
  },
  {
    id: "zh-docs-home",
    category: "International",
    path: "docs/i18n/zh-CN/README.md",
    title: { en: "Chinese Docs Hub", zh: "中文文档总览" },
    summary: {
      en: "Chinese documentation index and translated content set.",
      zh: "中文文档入口与翻译内容索引。",
    },
  },
];

const categories: Array<Category | "All"> = [
  "All",
  "Core",
  "Setup",
  "Operations",
  "Security",
  "Reference",
  "International",
];

const categoryLabel: Record<Locale, Record<Category | "All", string>> = {
  en: {
    All: "All",
    Core: "Core",
    Setup: "Setup",
    Operations: "Operations",
    Security: "Security",
    Reference: "Reference",
    International: "International",
  },
  zh: {
    All: "全部",
    Core: "核心",
    Setup: "部署",
    Operations: "运维",
    Security: "安全",
    Reference: "参考",
    International: "多语言",
  },
};

const copy = {
  en: {
    navDocs: "Docs",
    navGitHub: "GitHub",
    navWebsite: "zeroclawlabs.ai",
    badge: "PRIVATE AGENT INTELLIGENCE.",
    title: "Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.",
    summary:
      "Fast, small, and fully autonomous AI assistant infrastructure.",
    summary2: "Deploy anywhere. Swap anything.",
    notice:
      "Official source channels: use this repository as the source of truth and zeroclawlabs.ai as the official website.",
    ctaDocs: "Read docs now",
    ctaBootstrap: "One-click bootstrap",
    metrics: [
      { label: "Runtime Memory", value: "< 5MB" },
      { label: "Cold Start", value: "< 10ms" },
      { label: "Edge Hardware", value: "$10-class" },
      { label: "Language", value: "100% Rust" },
    ],
    docsWorkspace: "Documentation Workspace",
    docsLead:
      "Browse, filter, and read project docs directly in-page. Open any item in GitHub when needed.",
    search: "Search docs by topic, path, or keyword",
    commandPalette: "Command palette",
    sourceLabel: "Source",
    openOnGithub: "Open on GitHub",
    openRaw: "Open raw",
    loading: "Loading document...",
    fallback:
      "Document preview is unavailable right now. You can still open the source directly:",
    empty: "No docs matched your current filter.",
    paletteHint: "Type a command or document name",
    actionFocus: "Focus docs search",
    actionTop: "Back to top",
    actionTheme: "Cycle theme",
    actionLocale: "Toggle language",
    status: "Current Theme",
  },
  zh: {
    navDocs: "文档",
    navGitHub: "GitHub",
    navWebsite: "zeroclawlabs.ai",
    badge: "PRIVATE AGENT INTELLIGENCE.",
    title: "Zero overhead. Zero compromise. 100% Rust. 100% Agnostic.",
    summary: "Fast, small, and fully autonomous AI assistant infrastructure.",
    summary2: "Deploy anywhere. Swap anything.",
    notice:
      "官方信息渠道：请以本仓库为事实来源，以 zeroclawlabs.ai 为官方网站。",
    ctaDocs: "立即阅读文档",
    ctaBootstrap: "一键安装",
    metrics: [
      { label: "运行内存", value: "< 5MB" },
      { label: "冷启动", value: "< 10ms" },
      { label: "边缘硬件", value: "$10 级别" },
      { label: "语言", value: "100% Rust" },
    ],
    docsWorkspace: "文档工作区",
    docsLead: "在页面内直接浏览、过滤并阅读文档；需要时可一键跳转 GitHub 原文。",
    search: "按主题、路径或关键字搜索",
    commandPalette: "命令面板",
    sourceLabel: "来源",
    openOnGithub: "在 GitHub 打开",
    openRaw: "打开原文",
    loading: "文档加载中...",
    fallback: "当前无法预览文档，你仍可直接打开源文件：",
    empty: "当前筛选下没有匹配文档。",
    paletteHint: "输入命令或文档名称",
    actionFocus: "聚焦文档搜索",
    actionTop: "回到顶部",
    actionTheme: "切换主题",
    actionLocale: "切换语言",
    status: "当前主题",
  },
} as const;

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^\w\u4e00-\u9fa5\s-]/g, "")
    .trim()
    .replace(/\s+/g, "-");
}

function nodeText(node: ReactNode): string {
  if (typeof node === "string" || typeof node === "number") {
    return String(node);
  }
  if (Array.isArray(node)) {
    return node.map((part) => nodeText(part)).join("");
  }
  if (isValidElement(node)) {
    return nodeText(node.props.children as ReactNode);
  }
  return "";
}

function withRepo(path: string): string {
  return `${repoBase}/${path}`;
}

function withRaw(path: string): string {
  return `${rawBase}/${path}`;
}

function resolvePath(doc: DocEntry, locale: Locale): string {
  if (locale === "zh" && doc.zhPath) {
    return doc.zhPath;
  }
  return doc.path;
}

export default function App(): JSX.Element {
  const [locale, setLocale] = useState<Locale>(() => {
    if (typeof window === "undefined") {
      return "en";
    }
    return window.localStorage.getItem("zc-locale") === "zh" ? "zh" : "en";
  });

  const [themeMode, setThemeMode] = useState<ThemeMode>(() => {
    if (typeof window === "undefined") {
      return "system";
    }
    const stored = window.localStorage.getItem("zc-theme");
    if (stored === "light" || stored === "dark" || stored === "system") {
      return stored;
    }
    return "system";
  });

  const [resolvedTheme, setResolvedTheme] = useState<ResolvedTheme>("dark");
  const [category, setCategory] = useState<Category | "All">("All");
  const [query, setQuery] = useState("");
  const [selectedId, setSelectedId] = useState<string>("docs-home");

  const [markdownCache, setMarkdownCache] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");

  const docsSearchRef = useRef<HTMLInputElement | null>(null);
  const paletteInputRef = useRef<HTMLInputElement | null>(null);

  const text = copy[locale];

  useEffect(() => {
    window.localStorage.setItem("zc-locale", locale);
  }, [locale]);

  useEffect(() => {
    window.localStorage.setItem("zc-theme", themeMode);

    const media = window.matchMedia("(prefers-color-scheme: dark)");

    const applyTheme = (): void => {
      const nextTheme: ResolvedTheme =
        themeMode === "system" ? (media.matches ? "dark" : "light") : themeMode;
      document.documentElement.setAttribute("data-theme", nextTheme);
      setResolvedTheme(nextTheme);
    };

    applyTheme();

    if (themeMode === "system") {
      media.addEventListener("change", applyTheme);
      return () => media.removeEventListener("change", applyTheme);
    }

    return undefined;
  }, [themeMode]);

  const filteredDocs = useMemo(() => {
    const normalized = query.trim().toLowerCase();

    return docs.filter((doc) => {
      if (category !== "All" && doc.category !== category) {
        return false;
      }

      if (!normalized) {
        return true;
      }

      const bag = [
        doc.title[locale],
        doc.title.en,
        doc.title.zh,
        doc.summary[locale],
        doc.path,
        ...(doc.keywords ?? []),
      ]
        .join(" ")
        .toLowerCase();

      return bag.includes(normalized);
    });
  }, [category, locale, query]);

  useEffect(() => {
    if (filteredDocs.length === 0) {
      return;
    }

    const stillVisible = filteredDocs.some((doc) => doc.id === selectedId);
    if (!stillVisible) {
      setSelectedId(filteredDocs[0].id);
    }
  }, [filteredDocs, selectedId]);

  const selectedDoc =
    docs.find((doc) => doc.id === selectedId) ?? docs.find((doc) => doc.id === "docs-home") ?? docs[0];

  const activePath = resolvePath(selectedDoc, locale);
  const markdown = markdownCache[activePath] ?? "";

  useEffect(() => {
    let cancelled = false;
    const controller = new AbortController();

    if (markdownCache[activePath]) {
      return () => {
        cancelled = true;
        controller.abort();
      };
    }

    async function load(): Promise<void> {
      setLoading(true);
      setError("");

      try {
        const response = await fetch(withRaw(activePath), {
          signal: controller.signal,
        });

        if (!response.ok) {
          throw new Error(`HTTP ${response.status}`);
        }

        const textBody = await response.text();

        if (cancelled) {
          return;
        }

        setMarkdownCache((prev) => ({
          ...prev,
          [activePath]: textBody,
        }));
      } catch (err) {
        if (cancelled) {
          return;
        }

        const message = err instanceof Error ? err.message : "Failed to fetch";
        setError(message);
      } finally {
        if (!cancelled) {
          setLoading(false);
        }
      }
    }

    void load();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [activePath, markdownCache]);

  const cycleTheme = (): void => {
    setThemeMode((prev) => {
      if (prev === "system") return "dark";
      if (prev === "dark") return "light";
      return "system";
    });
  };

  const jumpToTop = (): void => {
    window.scrollTo({ top: 0, behavior: "smooth" });
  };

  const focusSearch = (): void => {
    docsSearchRef.current?.focus();
  };

  const paletteEntries = useMemo(() => {
    const staticEntries: PaletteEntry[] = [
      {
        id: "focus-search",
        label: text.actionFocus,
        hint: text.docsWorkspace,
        run: () => {
          document.getElementById("docs-workspace")?.scrollIntoView({ behavior: "smooth", block: "start" });
          setTimeout(() => docsSearchRef.current?.focus(), 300);
        },
      },
      {
        id: "top",
        label: text.actionTop,
        hint: "Home",
        run: jumpToTop,
      },
      {
        id: "theme",
        label: text.actionTheme,
        hint: `${text.status}: ${resolvedTheme}`,
        run: cycleTheme,
      },
      {
        id: "locale",
        label: text.actionLocale,
        hint: locale === "en" ? "EN -> 中文" : "中文 -> EN",
        run: () => setLocale((prev) => (prev === "en" ? "zh" : "en")),
      },
    ];

    const dynamicEntries: PaletteEntry[] = filteredDocs.slice(0, 10).map((doc) => ({
      id: `doc-${doc.id}`,
      label: doc.title[locale],
      hint: doc.path,
      run: () => {
        setSelectedId(doc.id);
        document.getElementById("docs-workspace")?.scrollIntoView({ behavior: "smooth", block: "start" });
      },
    }));

    return [...staticEntries, ...dynamicEntries];
  }, [filteredDocs, locale, resolvedTheme, text.actionFocus, text.actionLocale, text.actionTheme, text.actionTop, text.docsWorkspace, text.status]);

  const paletteResults = useMemo(() => {
    const normalized = paletteQuery.trim().toLowerCase();
    if (!normalized) {
      return paletteEntries;
    }

    return paletteEntries.filter((entry) => {
      return `${entry.label} ${entry.hint}`.toLowerCase().includes(normalized);
    });
  }, [paletteEntries, paletteQuery]);

  useEffect(() => {
    function onKeyDown(event: KeyboardEvent): void {
      const withCommand = (event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k";
      if (withCommand) {
        event.preventDefault();
        setPaletteOpen((prev) => !prev);
        return;
      }

      if (event.key === "Escape") {
        setPaletteOpen(false);
      }
    }

    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    if (paletteOpen) {
      setTimeout(() => paletteInputRef.current?.focus(), 0);
    } else {
      setPaletteQuery("");
    }
  }, [paletteOpen]);

  return (
    <div className="zc-app">
      <header className="topbar">
        <div className="topbar-inner">
          <a className="brand" href="#top">
            ZeroClaw
          </a>

          <nav className="top-nav" aria-label="Primary">
            <a href="#docs-workspace">{text.navDocs}</a>
            <a href="https://github.com/zeroclaw-labs/zeroclaw" target="_blank" rel="noreferrer">
              {text.navGitHub}
            </a>
            <a href="https://zeroclawlabs.ai" target="_blank" rel="noreferrer">
              {text.navWebsite}
            </a>
          </nav>

          <div className="controls">
            <div className="segmented" role="group" aria-label="Language">
              <button
                type="button"
                className={locale === "en" ? "active" : ""}
                onClick={() => setLocale("en")}
              >
                EN
              </button>
              <button
                type="button"
                className={locale === "zh" ? "active" : ""}
                onClick={() => setLocale("zh")}
              >
                中文
              </button>
            </div>

            <div className="segmented" role="group" aria-label="Theme">
              {(["system", "dark", "light"] as ThemeMode[]).map((mode) => (
                <button
                  key={mode}
                  type="button"
                  className={themeMode === mode ? "active" : ""}
                  onClick={() => setThemeMode(mode)}
                >
                  {mode}
                </button>
              ))}
            </div>

            <button type="button" className="palette-trigger" onClick={() => setPaletteOpen(true)}>
              ⌘K
            </button>
          </div>
        </div>
      </header>

      <main id="top">
        <section className="hero">
          <div className="hero-inner">
            <p className="eyebrow">{text.badge}</p>
            <h1>{text.title}</h1>
            <p className="lead">{text.summary}</p>
            <p className="lead muted">{text.summary2}</p>

            <div className="hero-cta">
              <a className="btn primary" href="#docs-workspace">
                {text.ctaDocs}
              </a>
              <a
                className="btn ghost"
                href={withRepo("docs/one-click-bootstrap.md")}
                target="_blank"
                rel="noreferrer"
              >
                {text.ctaBootstrap}
              </a>
            </div>

            <p className="notice">{text.notice}</p>

            <div className="metrics" aria-label="Project metrics">
              {text.metrics.map((metric) => (
                <article key={metric.label} className="metric-card">
                  <p className="metric-label">{metric.label}</p>
                  <p className="metric-value">{metric.value}</p>
                </article>
              ))}
            </div>
          </div>
        </section>

        <section id="docs-workspace" className="docs-shell">
          <div className="docs-head">
            <h2>{text.docsWorkspace}</h2>
            <p>{text.docsLead}</p>
          </div>

          <div className="docs-toolbar">
            <input
              ref={docsSearchRef}
              type="search"
              value={query}
              onChange={(event) => setQuery(event.target.value)}
              placeholder={text.search}
              aria-label={text.search}
            />
            <button type="button" className="btn ghost" onClick={() => setPaletteOpen(true)}>
              {text.commandPalette}
            </button>
          </div>

          <div className="category-row" role="tablist" aria-label="Doc categories">
            {categories.map((item) => (
              <button
                key={item}
                type="button"
                role="tab"
                aria-selected={category === item}
                className={category === item ? "active" : ""}
                onClick={() => setCategory(item)}
              >
                {categoryLabel[locale][item]}
              </button>
            ))}
          </div>

          <div className="workspace-grid">
            <aside className="doc-list" aria-label="Document list">
              {filteredDocs.length === 0 ? (
                <p className="empty-hint">{text.empty}</p>
              ) : (
                filteredDocs.map((doc) => {
                  const isActive = doc.id === selectedId;
                  return (
                    <button
                      key={doc.id}
                      type="button"
                      className={`doc-item ${isActive ? "active" : ""}`}
                      onClick={() => setSelectedId(doc.id)}
                    >
                      <span className="doc-meta">{categoryLabel[locale][doc.category]}</span>
                      <span className="doc-title">{doc.title[locale]}</span>
                      <span className="doc-summary">{doc.summary[locale]}</span>
                      <span className="doc-path">{resolvePath(doc, locale)}</span>
                    </button>
                  );
                })
              )}
            </aside>

            <section className="doc-reader" aria-live="polite">
              <header className="reader-head">
                <div>
                  <p>{text.sourceLabel}</p>
                  <h3>{selectedDoc.title[locale]}</h3>
                  <code>{activePath}</code>
                </div>
                <div className="reader-actions">
                  <a href={withRepo(activePath)} target="_blank" rel="noreferrer">
                    {text.openOnGithub}
                  </a>
                  <a href={withRaw(activePath)} target="_blank" rel="noreferrer">
                    {text.openRaw}
                  </a>
                </div>
              </header>

              {loading ? <p className="reader-status">{text.loading}</p> : null}

              {!loading && error ? (
                <p className="reader-status">
                  {text.fallback} <a href={withRepo(activePath)}>{withRepo(activePath)}</a>
                </p>
              ) : null}

              {!loading && !error && markdown ? (
                <article className="markdown-body">
                  <ReactMarkdown
                    remarkPlugins={[remarkGfm]}
                    components={{
                      h1: ({ children }) => {
                        const id = slugify(nodeText(children));
                        return <h1 id={id}>{children}</h1>;
                      },
                      h2: ({ children }) => {
                        const id = slugify(nodeText(children));
                        return <h2 id={id}>{children}</h2>;
                      },
                      h3: ({ children }) => {
                        const id = slugify(nodeText(children));
                        return <h3 id={id}>{children}</h3>;
                      },
                      a: ({ href, children }) => {
                        const url = href ?? "#";
                        const external = /^https?:\/\//i.test(url);
                        return (
                          <a href={url} target={external ? "_blank" : undefined} rel={external ? "noreferrer" : undefined}>
                            {children}
                          </a>
                        );
                      },
                    }}
                  >
                    {markdown}
                  </ReactMarkdown>
                </article>
              ) : null}
            </section>
          </div>
        </section>
      </main>

      <footer className="footer">
        <p>ZeroClaw · Trait-driven architecture · secure-by-default runtime · pluggable everything</p>
      </footer>

      {paletteOpen ? (
        <div className="palette-backdrop" onClick={() => setPaletteOpen(false)}>
          <div className="palette" role="dialog" aria-modal="true" onClick={(event) => event.stopPropagation()}>
            <input
              ref={paletteInputRef}
              type="search"
              value={paletteQuery}
              onChange={(event) => setPaletteQuery(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === "Enter" && paletteResults[0]) {
                  paletteResults[0].run();
                  setPaletteOpen(false);
                }
              }}
              placeholder={text.paletteHint}
              aria-label={text.paletteHint}
            />
            <div className="palette-list">
              {paletteResults.slice(0, 12).map((entry) => (
                <button
                  key={entry.id}
                  type="button"
                  onClick={() => {
                    entry.run();
                    setPaletteOpen(false);
                  }}
                >
                  <span>{entry.label}</span>
                  <small>{entry.hint}</small>
                </button>
              ))}
            </div>
          </div>
        </div>
      ) : null}

      <button
        type="button"
        className="floating"
        onClick={focusSearch}
        aria-label={text.actionFocus}
        title={text.actionFocus}
      >
        {text.navDocs}
      </button>
    </div>
  );
}
