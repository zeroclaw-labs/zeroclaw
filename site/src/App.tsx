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
import manifestRaw from "./generated/docs-manifest.json";

type Locale = "en" | "zh" | "es" | "pt" | "it";
type ThemeMode = "system" | "dark" | "light";
type ResolvedTheme = "dark" | "light";
type ReaderScale = "compact" | "comfortable" | "relaxed";
type ReaderWidth = "normal" | "wide";
type Journey =
  | "start"
  | "build"
  | "integrate"
  | "operate"
  | "secure"
  | "contribute"
  | "reference"
  | "hardware"
  | "localize"
  | "troubleshoot";
type Audience =
  | "newcomer"
  | "builder"
  | "operator"
  | "security"
  | "contributor"
  | "integrator"
  | "hardware";
type DocKind = "guide" | "reference" | "runbook" | "policy" | "template" | "report";
type GroupMode = "journey" | "section" | "kind" | "language";

type ManifestDoc = {
  id: string;
  path: string;
  title: string;
  summary: string;
  section: string;
  language: string;
  journey: Journey;
  audience: Audience;
  kind: DocKind;
  tags: string[];
  readingMinutes: number;
  startHere: boolean;
  sourceUrl: string;
};

type ManifestDocRaw = Omit<
  ManifestDoc,
  "journey" | "audience" | "kind" | "tags" | "readingMinutes" | "startHere"
> &
  Partial<
    Pick<
      ManifestDoc,
      "journey" | "audience" | "kind" | "tags" | "readingMinutes" | "startHere"
    >
  >;

type HeadingItem = {
  id: string;
  level: number;
  text: string;
};

type Localized = {
  en: string;
  zh: string;
  es?: string;
  pt?: string;
  it?: string;
};

type PaletteEntry = {
  id: string;
  label: string;
  hint: string;
  run: () => void;
};

const repoBase = "https://github.com/zeroclaw-labs/zeroclaw/blob/main";
const rawBase = "https://raw.githubusercontent.com/zeroclaw-labs/zeroclaw/main";
const localeOrder: Locale[] = ["en", "zh", "es", "pt", "it"];
const localeLabels: Record<Locale, string> = {
  en: "EN",
  zh: "中文",
  es: "ES",
  pt: "PT",
  it: "IT",
};

function normalizeLocale(value: string | null): Locale {
  const candidate = (value ?? "").toLowerCase();
  if (candidate === "en" || candidate === "zh" || candidate === "es" || candidate === "pt" || candidate === "it") {
    return candidate;
  }
  return "en";
}

function nextLocale(current: Locale): Locale {
  const index = localeOrder.indexOf(current);
  if (index === -1 || index === localeOrder.length - 1) {
    return localeOrder[0];
  }
  return localeOrder[index + 1] ?? localeOrder[0];
}

const languageNames: Record<string, Localized> = {
  en: { en: "English", zh: "英文", es: "Inglés", pt: "Inglês", it: "Inglese" },
  "zh-CN": {
    en: "Chinese (Simplified)",
    zh: "简体中文",
    es: "Chino (simplificado)",
    pt: "Chinês (simplificado)",
    it: "Cinese (semplificato)",
  },
  ja: { en: "Japanese", zh: "日文", es: "Japonés", pt: "Japonês", it: "Giapponese" },
  ru: { en: "Russian", zh: "俄文", es: "Ruso", pt: "Russo", it: "Russo" },
  fr: { en: "French", zh: "法文", es: "Francés", pt: "Francês", it: "Francese" },
  vi: { en: "Vietnamese", zh: "越南文", es: "Vietnamita", pt: "Vietnamita", it: "Vietnamita" },
  el: { en: "Greek", zh: "希腊文", es: "Griego", pt: "Grego", it: "Greco" },
  es: { en: "Spanish", zh: "西班牙文", es: "Español", pt: "Espanhol", it: "Spagnolo" },
  pt: { en: "Portuguese", zh: "葡萄牙文", es: "Portugués", pt: "Português", it: "Portoghese" },
  it: { en: "Italian", zh: "意大利文", es: "Italiano", pt: "Italiano", it: "Italiano" },
};

const journeyOrder: Journey[] = [
  "start",
  "build",
  "integrate",
  "operate",
  "secure",
  "contribute",
  "reference",
  "hardware",
  "localize",
  "troubleshoot",
];

const audienceOrder: Audience[] = [
  "newcomer",
  "builder",
  "integrator",
  "operator",
  "security",
  "contributor",
  "hardware",
];

const kindOrder: DocKind[] = ["guide", "reference", "runbook", "policy", "template", "report"];

const journeyNames: Record<Journey, Localized> = {
  start: { en: "Start", zh: "起步", es: "Inicio", pt: "Início", it: "Avvio" },
  build: { en: "Build", zh: "构建", es: "Construir", pt: "Construir", it: "Build" },
  integrate: { en: "Integrate", zh: "集成", es: "Integrar", pt: "Integrar", it: "Integrare" },
  operate: { en: "Operate", zh: "运维", es: "Operar", pt: "Operar", it: "Operare" },
  secure: { en: "Secure", zh: "安全", es: "Seguridad", pt: "Segurança", it: "Sicurezza" },
  contribute: { en: "Contribute", zh: "贡献", es: "Contribuir", pt: "Contribuir", it: "Contribuire" },
  reference: { en: "Reference", zh: "参考", es: "Referencia", pt: "Referência", it: "Riferimento" },
  hardware: { en: "Hardware", zh: "硬件", es: "Hardware", pt: "Hardware", it: "Hardware" },
  localize: { en: "Localization", zh: "多语言", es: "Localización", pt: "Localização", it: "Localizzazione" },
  troubleshoot: { en: "Troubleshoot", zh: "排障", es: "Diagnóstico", pt: "Diagnóstico", it: "Risoluzione problemi" },
};

const audienceNames: Record<Audience, Localized> = {
  newcomer: { en: "Newcomer", zh: "新手", es: "Principiante", pt: "Iniciante", it: "Nuovo utente" },
  builder: { en: "Builder", zh: "开发者", es: "Desarrollador", pt: "Desenvolvedor", it: "Builder" },
  integrator: { en: "Integrator", zh: "集成者", es: "Integrador", pt: "Integrador", it: "Integratore" },
  operator: { en: "Operator", zh: "运维", es: "Operador", pt: "Operador", it: "Operatore" },
  security: { en: "Security", zh: "安全", es: "Seguridad", pt: "Segurança", it: "Sicurezza" },
  contributor: { en: "Contributor", zh: "贡献者", es: "Colaborador", pt: "Colaborador", it: "Contributore" },
  hardware: { en: "Hardware", zh: "硬件工程师", es: "Hardware", pt: "Hardware", it: "Hardware" },
};

const kindNames: Record<DocKind, Localized> = {
  guide: { en: "Guide", zh: "指南", es: "Guía", pt: "Guia", it: "Guida" },
  reference: { en: "Reference", zh: "参考", es: "Referencia", pt: "Referência", it: "Riferimento" },
  runbook: { en: "Runbook", zh: "运行手册", es: "Runbook", pt: "Runbook", it: "Runbook" },
  policy: { en: "Policy", zh: "策略规范", es: "Política", pt: "Política", it: "Policy" },
  template: { en: "Template", zh: "模板", es: "Plantilla", pt: "Template", it: "Template" },
  report: { en: "Report", zh: "报告", es: "Informe", pt: "Relatório", it: "Report" },
};

const readingPaths: Array<{
  id: string;
  label: Localized;
  detail: Localized;
  filters: Partial<{
    journey: Journey;
    audience: Audience;
    kind: DocKind;
    section: string;
  }>;
}> = [
  {
    id: "newcomer",
    label: {
      en: "New to ZeroClaw",
      zh: "初次了解 ZeroClaw",
      es: "Nuevo en ZeroClaw",
      pt: "Novo no ZeroClaw",
      it: "Nuovo in ZeroClaw",
    },
    detail: {
      en: "Onboarding and first successful run in the shortest path.",
      zh: "最短路径完成安装、配置和首个可运行实例。",
      es: "Onboarding y primera ejecución exitosa por la ruta más corta.",
      pt: "Onboarding e primeira execução bem-sucedida pelo caminho mais curto.",
      it: "Onboarding e prima esecuzione riuscita nel percorso più rapido.",
    },
    filters: { journey: "start", audience: "newcomer" },
  },
  {
    id: "builder",
    label: {
      en: "Build & Extend",
      zh: "开发与扩展",
      es: "Construir y extender",
      pt: "Construir e expandir",
      it: "Build ed estensione",
    },
    detail: {
      en: "Commands, config, providers, channels, and architecture references.",
      zh: "命令、配置、Provider、Channel 与架构参考。",
      es: "Comandos, configuración, providers, canales y referencias de arquitectura.",
      pt: "Comandos, configuração, providers, canais e referências de arquitetura.",
      it: "Comandi, configurazione, provider, canali e riferimenti architetturali.",
    },
    filters: { journey: "build", audience: "builder" },
  },
  {
    id: "operate",
    label: {
      en: "Operate in Production",
      zh: "生产运维",
      es: "Operar en producción",
      pt: "Operar em produção",
      it: "Operare in produzione",
    },
    detail: {
      en: "Runbooks, CI/CD gates, release flow, and observability checks.",
      zh: "运行手册、CI/CD 门禁、发布流程与可观测性校验。",
      es: "Runbooks, puertas CI/CD, flujo de release y validaciones de observabilidad.",
      pt: "Runbooks, gates de CI/CD, fluxo de release e verificações de observabilidade.",
      it: "Runbook, gate CI/CD, flusso di rilascio e controlli di osservabilità.",
    },
    filters: { journey: "operate", audience: "operator" },
  },
  {
    id: "secure",
    label: {
      en: "Security Hardening",
      zh: "安全强化",
      es: "Hardening de seguridad",
      pt: "Hardening de segurança",
      it: "Hardening di sicurezza",
    },
    detail: {
      en: "Sandboxing, advisories, and secure runtime baseline.",
      zh: "沙箱、安全公告与安全运行时基线。",
      es: "Sandboxing, avisos y baseline de runtime seguro.",
      pt: "Sandboxing, advisories e baseline de runtime seguro.",
      it: "Sandboxing, advisory e baseline runtime sicura.",
    },
    filters: { journey: "secure", audience: "security" },
  },
  {
    id: "integrate",
    label: {
      en: "Integrations",
      zh: "外部集成",
      es: "Integraciones",
      pt: "Integrações",
      it: "Integrazioni",
    },
    detail: {
      en: "Connect ZeroClaw with chat platforms and external providers.",
      zh: "将 ZeroClaw 接入聊天平台与外部模型能力。",
      es: "Conecta ZeroClaw con plataformas de chat y providers externos.",
      pt: "Conecte o ZeroClaw com plataformas de chat e providers externos.",
      it: "Collega ZeroClaw a piattaforme chat e provider esterni.",
    },
    filters: { journey: "integrate", audience: "integrator" },
  },
  {
    id: "contribute",
    label: {
      en: "Contribute",
      zh: "贡献流程",
      es: "Contribuir",
      pt: "Contribuir",
      it: "Contribuire",
    },
    detail: {
      en: "Contributor workflow, reviews, and collaboration playbooks.",
      zh: "贡献者流程、评审规范与协作手册。",
      es: "Flujo de contribución, revisiones y playbooks de colaboración.",
      pt: "Fluxo de contribuição, reviews e playbooks de colaboração.",
      it: "Flusso contributivo, review e playbook di collaborazione.",
    },
    filters: { journey: "contribute", audience: "contributor" },
  },
];

function asJourney(value: string | undefined): Journey {
  const candidate = (value ?? "").toLowerCase() as Journey;
  return journeyOrder.includes(candidate) ? candidate : "build";
}

function asAudience(value: string | undefined): Audience {
  const candidate = (value ?? "").toLowerCase() as Audience;
  return audienceOrder.includes(candidate) ? candidate : "builder";
}

function asKind(value: string | undefined): DocKind {
  const candidate = (value ?? "").toLowerCase() as DocKind;
  return kindOrder.includes(candidate) ? candidate : "guide";
}

function normalizeManifestDoc(doc: ManifestDocRaw): ManifestDoc {
  const summary = doc.summary?.trim() || "Project documentation.";
  const tags = Array.isArray(doc.tags)
    ? doc.tags
        .map((tag) => tag.trim().toLowerCase())
        .filter((tag) => tag.length > 0)
        .slice(0, 8)
    : [];

  return {
    ...doc,
    summary,
    journey: asJourney(doc.journey),
    audience: asAudience(doc.audience),
    kind: asKind(doc.kind),
    tags,
    readingMinutes:
      typeof doc.readingMinutes === "number" && Number.isFinite(doc.readingMinutes)
        ? Math.max(1, Math.round(doc.readingMinutes))
        : 1,
    startHere: Boolean(doc.startHere),
  };
}

const docs = [...(manifestRaw as ManifestDocRaw[])]
  .map((doc) => normalizeManifestDoc(doc))
  .sort((a, b) => a.path.localeCompare(b.path));

const copyEn = {
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
  commandLaneTitle: "Runtime command lane",
  commandLaneHint: "From official setup and operations flow",
  engineeringTitle: "Engineering foundations",
  docsWorkspace: "Documentation Workspace",
  docsLead:
    "All repository docs are indexed and readable directly on this GitHub Pages site with engineering-first layout and typography.",
  docsIndexed: "Indexed",
  docsFiltered: "Filtered",
  docsActive: "Active",
  readingPathsTitle: "Reading paths",
  readingPathsLead:
    "Choose a task-oriented route first, then drill down with taxonomy filters.",
  startHereTitle: "Start here",
  startHereLead:
    "Core docs for first-time users who want the fastest reliable onboarding.",
  noStartHere: "No starter docs matched the current language/filter context.",
  startBadge: "Starter",
  sectionFilter: "Section",
  languageFilter: "Language",
  journeyFilter: "Journey",
  audienceFilter: "Audience",
  kindFilter: "Doc type",
  groupBy: "Group by",
  allJourneys: "All journeys",
  allAudiences: "All audiences",
  allKinds: "All doc types",
  groupJourney: "Journey",
  groupSection: "Section",
  groupKind: "Doc type",
  groupLanguage: "Language",
  resetFilters: "Reset filters",
  search: "Search docs by title, path, summary, or keyword",
  commandPalette: "Command palette",
  sourceLabel: "Source",
  docJourney: "Journey",
  docAudience: "Audience",
  docKind: "Type",
  docReadTime: "Read time",
  docTags: "Tags",
  minuteUnit: "min",
  relatedDocs: "Related docs",
  noRelated: "No strongly related docs found yet.",
  openOnGithub: "Open on GitHub",
  openRaw: "Open raw",
  loading: "Loading document...",
  fallback:
    "Document preview is unavailable right now. You can still open the source directly:",
  empty: "No docs matched your current filters.",
  allSections: "All sections",
  allLanguages: "All languages",
  outline: "Outline",
  noOutline: "No headings found in this document.",
  reading: "Reading mode",
  scaleLabel: "Scale",
  widthLabel: "Width",
  compact: "Compact",
  comfortable: "Comfortable",
  relaxed: "Relaxed",
  normalWidth: "Normal",
  wideWidth: "Wide",
  previousDoc: "Previous",
  nextDoc: "Next",
  paletteHint: "Type a command or document name",
  actionFocus: "Focus docs search",
  actionTop: "Back to top",
  actionTheme: "Cycle theme",
  actionLocale: "Toggle language",
  status: "Current Theme",
};

const copyZh: typeof copyEn = {
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
  commandLaneTitle: "运行命令通道",
  commandLaneHint: "来自官方安装与运维流程",
  engineeringTitle: "工程基础",
  docsWorkspace: "文档工作区",
  docsLead:
    "仓库全量文档已建立索引并支持在 GitHub Pages 页面内直接阅读，采用工程化排版与阅读体验。",
  docsIndexed: "总文档",
  docsFiltered: "筛选后",
  docsActive: "当前文档",
  readingPathsTitle: "阅读路径",
  readingPathsLead: "先按任务路径进入，再用分类筛选做深入浏览。",
  startHereTitle: "新手起步",
  startHereLead: "面向首次接触 ZeroClaw 的核心文档，快速完成有效上手。",
  noStartHere: "当前语言/筛选条件下暂无起步文档。",
  startBadge: "起步",
  sectionFilter: "分组",
  languageFilter: "语言",
  journeyFilter: "阶段路径",
  audienceFilter: "适用角色",
  kindFilter: "文档类型",
  groupBy: "分组方式",
  allJourneys: "全部路径",
  allAudiences: "全部角色",
  allKinds: "全部类型",
  groupJourney: "按路径",
  groupSection: "按目录",
  groupKind: "按类型",
  groupLanguage: "按语言",
  resetFilters: "重置筛选",
  search: "按标题、路径、摘要或关键字搜索",
  commandPalette: "命令面板",
  sourceLabel: "来源",
  docJourney: "阶段",
  docAudience: "角色",
  docKind: "类型",
  docReadTime: "阅读时长",
  docTags: "标签",
  minuteUnit: "分钟",
  relatedDocs: "相关推荐",
  noRelated: "暂未找到高相关文档。",
  openOnGithub: "在 GitHub 打开",
  openRaw: "打开原文",
  loading: "文档加载中...",
  fallback: "当前无法预览文档，你仍可直接打开源文件：",
  empty: "当前筛选下没有匹配文档。",
  allSections: "全部分组",
  allLanguages: "全部语言",
  outline: "目录",
  noOutline: "当前文档没有可提取的标题。",
  reading: "阅读模式",
  scaleLabel: "字号",
  widthLabel: "宽度",
  compact: "紧凑",
  comfortable: "舒适",
  relaxed: "宽松",
  normalWidth: "标准",
  wideWidth: "加宽",
  previousDoc: "上一篇",
  nextDoc: "下一篇",
  paletteHint: "输入命令或文档名称",
  actionFocus: "聚焦文档搜索",
  actionTop: "回到顶部",
  actionTheme: "切换主题",
  actionLocale: "切换语言",
  status: "当前主题",
};

const copyEs: typeof copyEn = {
  ...copyEn,
  navDocs: "Documentos",
  summary:
    "Infraestructura de asistente de IA rápida, ligera y totalmente autónoma.",
  summary2: "Despliega en cualquier lugar. Intercambia cualquier componente.",
  notice:
    "Canales oficiales: usa este repositorio como fuente de verdad y zeroclawlabs.ai como sitio oficial.",
  ctaDocs: "Leer documentación",
  ctaBootstrap: "Bootstrap en un clic",
  commandLaneTitle: "Canal de comandos runtime",
  commandLaneHint: "Basado en el flujo oficial de instalación y operaciones",
  engineeringTitle: "Fundamentos de ingeniería",
  docsWorkspace: "Espacio de documentación",
  docsLead:
    "Toda la documentación del repositorio está indexada y se puede leer directamente en este sitio GitHub Pages con diseño de ingeniería.",
  docsIndexed: "Indexados",
  docsFiltered: "Filtrados",
  docsActive: "Activo",
  readingPathsTitle: "Rutas de lectura",
  readingPathsLead:
    "Empieza con una ruta orientada a tareas y luego profundiza con filtros taxonómicos.",
  startHereTitle: "Empieza aquí",
  startHereLead:
    "Documentos base para usuarios nuevos que quieren una incorporación rápida y confiable.",
  noStartHere: "No hay documentos iniciales para el idioma/filtros actuales.",
  startBadge: "Inicial",
  sectionFilter: "Sección",
  languageFilter: "Idioma",
  journeyFilter: "Recorrido",
  audienceFilter: "Audiencia",
  kindFilter: "Tipo de documento",
  groupBy: "Agrupar por",
  allJourneys: "Todos los recorridos",
  allAudiences: "Todas las audiencias",
  allKinds: "Todos los tipos",
  groupJourney: "Recorrido",
  groupSection: "Sección",
  groupKind: "Tipo",
  groupLanguage: "Idioma",
  resetFilters: "Restablecer filtros",
  search: "Buscar por título, ruta, resumen o palabra clave",
  commandPalette: "Paleta de comandos",
  sourceLabel: "Fuente",
  docJourney: "Recorrido",
  docAudience: "Audiencia",
  docKind: "Tipo",
  docReadTime: "Tiempo de lectura",
  docTags: "Etiquetas",
  relatedDocs: "Documentos relacionados",
  noRelated: "Aún no se encontraron documentos fuertemente relacionados.",
  openOnGithub: "Abrir en GitHub",
  openRaw: "Abrir texto fuente",
  loading: "Cargando documento...",
  fallback:
    "La vista previa no está disponible ahora. Aun así puedes abrir la fuente directamente:",
  empty: "No hay documentos para los filtros actuales.",
  allSections: "Todas las secciones",
  allLanguages: "Todos los idiomas",
  outline: "Esquema",
  noOutline: "No se encontraron encabezados en este documento.",
  reading: "Modo lectura",
  scaleLabel: "Escala",
  widthLabel: "Ancho",
  compact: "Compacto",
  comfortable: "Cómodo",
  relaxed: "Amplio",
  normalWidth: "Normal",
  wideWidth: "Ancho",
  previousDoc: "Anterior",
  nextDoc: "Siguiente",
  paletteHint: "Escribe un comando o nombre de documento",
  actionFocus: "Enfocar búsqueda de documentos",
  actionTop: "Volver arriba",
  actionTheme: "Cambiar tema",
  actionLocale: "Cambiar idioma",
  status: "Tema actual",
};

const copyPt: typeof copyEn = {
  ...copyEn,
  navDocs: "Documentação",
  summary:
    "Infraestrutura de assistente de IA rápida, leve e totalmente autônoma.",
  summary2: "Implante em qualquer lugar. Troque qualquer componente.",
  notice:
    "Canais oficiais: use este repositório como fonte da verdade e zeroclawlabs.ai como site oficial.",
  ctaDocs: "Ler documentação",
  ctaBootstrap: "Bootstrap em um clique",
  commandLaneTitle: "Canal de comandos runtime",
  commandLaneHint: "Com base no fluxo oficial de setup e operações",
  engineeringTitle: "Fundamentos de engenharia",
  docsWorkspace: "Workspace de documentação",
  docsLead:
    "Toda a documentação do repositório está indexada e pode ser lida diretamente neste GitHub Pages com layout orientado à engenharia.",
  docsIndexed: "Indexados",
  docsFiltered: "Filtrados",
  docsActive: "Ativo",
  readingPathsTitle: "Trilhas de leitura",
  readingPathsLead:
    "Escolha primeiro uma trilha orientada por tarefa e depois aprofunde com filtros taxonômicos.",
  startHereTitle: "Comece aqui",
  startHereLead:
    "Documentos essenciais para quem está começando e quer onboarding rápido e confiável.",
  noStartHere: "Nenhum documento inicial corresponde ao idioma/filtros atuais.",
  startBadge: "Inicial",
  sectionFilter: "Seção",
  languageFilter: "Idioma",
  journeyFilter: "Jornada",
  audienceFilter: "Público",
  kindFilter: "Tipo de documento",
  groupBy: "Agrupar por",
  allJourneys: "Todas as jornadas",
  allAudiences: "Todos os públicos",
  allKinds: "Todos os tipos",
  groupJourney: "Jornada",
  groupSection: "Seção",
  groupKind: "Tipo",
  groupLanguage: "Idioma",
  resetFilters: "Limpar filtros",
  search: "Buscar por título, caminho, resumo ou palavra-chave",
  commandPalette: "Paleta de comandos",
  sourceLabel: "Fonte",
  docJourney: "Jornada",
  docAudience: "Público",
  docKind: "Tipo",
  docReadTime: "Tempo de leitura",
  docTags: "Tags",
  relatedDocs: "Documentos relacionados",
  noRelated: "Ainda não há documentos fortemente relacionados.",
  openOnGithub: "Abrir no GitHub",
  openRaw: "Abrir fonte bruta",
  loading: "Carregando documento...",
  fallback:
    "A pré-visualização não está disponível agora. Você ainda pode abrir a fonte diretamente:",
  empty: "Nenhum documento corresponde aos filtros atuais.",
  allSections: "Todas as seções",
  allLanguages: "Todos os idiomas",
  outline: "Sumário",
  noOutline: "Nenhum cabeçalho foi encontrado neste documento.",
  reading: "Modo de leitura",
  scaleLabel: "Escala",
  widthLabel: "Largura",
  compact: "Compacto",
  comfortable: "Confortável",
  relaxed: "Arejado",
  normalWidth: "Normal",
  wideWidth: "Amplo",
  previousDoc: "Anterior",
  nextDoc: "Próximo",
  paletteHint: "Digite um comando ou nome de documento",
  actionFocus: "Focar busca de documentos",
  actionTop: "Voltar ao topo",
  actionTheme: "Alternar tema",
  actionLocale: "Alternar idioma",
  status: "Tema atual",
};

const copyIt: typeof copyEn = {
  ...copyEn,
  navDocs: "Documentazione",
  summary:
    "Infrastruttura di assistente AI veloce, leggera e completamente autonoma.",
  summary2: "Distribuisci ovunque. Sostituisci qualsiasi componente.",
  notice:
    "Canali ufficiali: usa questo repository come fonte di verità e zeroclawlabs.ai come sito ufficiale.",
  ctaDocs: "Leggi la documentazione",
  ctaBootstrap: "Bootstrap con un clic",
  commandLaneTitle: "Canale comandi runtime",
  commandLaneHint: "Dal flusso ufficiale di setup e operazioni",
  engineeringTitle: "Fondamenta ingegneristiche",
  docsWorkspace: "Workspace documentazione",
  docsLead:
    "Tutta la documentazione del repository è indicizzata e leggibile direttamente su questo GitHub Pages con layout orientato all'ingegneria.",
  docsIndexed: "Indicizzati",
  docsFiltered: "Filtrati",
  docsActive: "Attivo",
  readingPathsTitle: "Percorsi di lettura",
  readingPathsLead:
    "Scegli prima un percorso orientato al task, poi approfondisci con filtri tassonomici.",
  startHereTitle: "Inizia qui",
  startHereLead:
    "Documenti fondamentali per nuovi utenti che vogliono onboarding rapido e affidabile.",
  noStartHere: "Nessun documento iniziale corrisponde a lingua/filtri correnti.",
  startBadge: "Starter",
  sectionFilter: "Sezione",
  languageFilter: "Lingua",
  journeyFilter: "Percorso",
  audienceFilter: "Audience",
  kindFilter: "Tipo documento",
  groupBy: "Raggruppa per",
  allJourneys: "Tutti i percorsi",
  allAudiences: "Tutte le audience",
  allKinds: "Tutti i tipi",
  groupJourney: "Percorso",
  groupSection: "Sezione",
  groupKind: "Tipo",
  groupLanguage: "Lingua",
  resetFilters: "Reimposta filtri",
  search: "Cerca per titolo, percorso, riepilogo o parola chiave",
  commandPalette: "Command palette",
  sourceLabel: "Sorgente",
  docJourney: "Percorso",
  docAudience: "Audience",
  docKind: "Tipo",
  docReadTime: "Tempo di lettura",
  docTags: "Tag",
  relatedDocs: "Documenti correlati",
  noRelated: "Nessun documento fortemente correlato trovato.",
  openOnGithub: "Apri su GitHub",
  openRaw: "Apri sorgente raw",
  loading: "Caricamento documento...",
  fallback:
    "L'anteprima del documento non è disponibile ora. Puoi comunque aprire direttamente la sorgente:",
  empty: "Nessun documento corrisponde ai filtri correnti.",
  allSections: "Tutte le sezioni",
  allLanguages: "Tutte le lingue",
  outline: "Indice",
  noOutline: "Nessuna intestazione trovata in questo documento.",
  reading: "Modalità lettura",
  scaleLabel: "Scala",
  widthLabel: "Larghezza",
  compact: "Compatto",
  comfortable: "Confortevole",
  relaxed: "Ampio",
  normalWidth: "Normale",
  wideWidth: "Larga",
  previousDoc: "Precedente",
  nextDoc: "Successivo",
  paletteHint: "Digita un comando o nome documento",
  actionFocus: "Metti a fuoco ricerca documenti",
  actionTop: "Torna in alto",
  actionTheme: "Cambia tema",
  actionLocale: "Cambia lingua",
  status: "Tema corrente",
};

const copy: Record<Locale, typeof copyEn> = {
  en: copyEn,
  zh: copyZh,
  es: copyEs,
  pt: copyPt,
  it: copyIt,
};

const commandLane: Array<{ command: string; hint: Localized }> = [
  {
    command: "curl -fsSL https://zeroclawlabs.ai/install.sh | bash",
    hint: {
      en: "Install dependencies, Rust, and launch interactive onboarding",
      zh: "安装依赖与 Rust，并启动交互式引导",
      es: "Instalar dependencias, Rust e iniciar onboarding interactivo",
      pt: "Instalar dependências, Rust e iniciar onboarding interativo",
      it: "Installa dipendenze, Rust e avvia onboarding interattivo",
    },
  },
  {
    command: "zeroclaw agent",
    hint: {
      en: "Run interactive agent mode",
      zh: "运行交互式 Agent 模式",
      es: "Ejecutar modo agente interactivo",
      pt: "Executar modo agente interativo",
      it: "Esegui modalità agente interattiva",
    },
  },
  {
    command: "zeroclaw gateway && zeroclaw daemon",
    hint: {
      en: "Start runtime services",
      zh: "启动运行时服务",
      es: "Iniciar servicios de runtime",
      pt: "Iniciar serviços de runtime",
      it: "Avvia servizi runtime",
    },
  },
  {
    command: "zeroclaw doctor",
    hint: {
      en: "Validate environment and runtime health",
      zh: "校验环境与运行时健康状态",
      es: "Validar entorno y salud del runtime",
      pt: "Validar ambiente e saúde do runtime",
      it: "Valida ambiente e salute del runtime",
    },
  },
];

const engineeringPillars: Array<{ title: Localized; detail: Localized }> = [
  {
    title: {
      en: "Trait-driven architecture",
      zh: "Trait 驱动架构",
      es: "Arquitectura guiada por traits",
      pt: "Arquitetura orientada a traits",
      it: "Architettura guidata da trait",
    },
    detail: {
      en: "Providers, channels, tools, memory, and tunnels remain swappable through interfaces.",
      zh: "Provider、Channel、Tool、Memory、Tunnel 通过接口保持可插拔。",
      es: "Providers, canales, tools, memoria y túneles se mantienen intercambiables mediante interfaces.",
      pt: "Providers, canais, tools, memória e túneis permanecem intercambiáveis via interfaces.",
      it: "Provider, canali, tool, memoria e tunnel restano sostituibili tramite interfacce.",
    },
  },
  {
    title: {
      en: "Secure by default runtime",
      zh: "默认安全运行时",
      es: "Runtime seguro por defecto",
      pt: "Runtime seguro por padrão",
      it: "Runtime sicuro per default",
    },
    detail: {
      en: "Pairing, sandboxing, explicit allowlists, and workspace scoping are baseline controls.",
      zh: "配对、沙箱、显式白名单与工作区作用域作为基线控制。",
      es: "Pairing, sandboxing, allowlists explícitas y scope de workspace son controles base.",
      pt: "Pairing, sandboxing, allowlists explícitas e escopo de workspace são controles de base.",
      it: "Pairing, sandboxing, allowlist esplicite e scope del workspace sono controlli di base.",
    },
  },
  {
    title: {
      en: "Build once, run anywhere",
      zh: "一次构建，到处运行",
      es: "Compila una vez, ejecuta en cualquier lugar",
      pt: "Compile uma vez, rode em qualquer lugar",
      it: "Build una volta, esegui ovunque",
    },
    detail: {
      en: "Single-binary Rust workflow across ARM, x86, and RISC-V from edge to cloud.",
      zh: "单一 Rust 二进制工作流覆盖 ARM、x86、RISC-V，从边缘到云端。",
      es: "Flujo Rust de binario único para ARM, x86 y RISC-V desde edge hasta cloud.",
      pt: "Fluxo Rust de binário único em ARM, x86 e RISC-V do edge à nuvem.",
      it: "Workflow Rust a binario singolo su ARM, x86 e RISC-V dall'edge al cloud.",
    },
  },
];

function normalizePath(input: string): string {
  return input
    .replace(/\\/g, "/")
    .replace(/^\/+/, "")
    .replace(/\/+/g, "/")
    .replace(/^\.\//, "");
}

function slugify(text: string): string {
  return text
    .toLowerCase()
    .replace(/<[^>]+>/g, "")
    .replace(/\[[^\]]+\]\([^)]*\)/g, "")
    .replace(/[^\w\u4e00-\u9fa5\s-]/g, "")
    .trim()
    .replace(/\s+/g, "-")
    .replace(/-+/g, "-");
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

function encodePathSegments(filePath: string): string {
  return normalizePath(filePath)
    .split("/")
    .map((segment) => encodeURIComponent(segment))
    .join("/");
}

function docsContentUrl(filePath: string): string {
  return `${import.meta.env.BASE_URL}docs-content/${encodePathSegments(filePath)}`;
}

function withRepo(filePath: string): string {
  return `${repoBase}/${normalizePath(filePath)}`;
}

function withRaw(filePath: string): string {
  return `${rawBase}/${normalizePath(filePath)}`;
}

function cleanHeadingText(raw: string): string {
  return raw
    .replace(/`([^`]+)`/g, "$1")
    .replace(/\[([^\]]+)\]\([^)]*\)/g, "$1")
    .replace(/\s+#*$/, "")
    .trim();
}

function extractHeadings(markdown: string): HeadingItem[] {
  const lines = markdown.split(/\r?\n/);
  const headings: HeadingItem[] = [];
  const slugCounts = new Map<string, number>();
  let inCode = false;

  for (const rawLine of lines) {
    const line = rawLine.trim();

    if (line.startsWith("```")) {
      inCode = !inCode;
      continue;
    }

    if (inCode) {
      continue;
    }

    const match = /^(#{1,3})\s+(.+)$/.exec(line);
    if (!match) {
      continue;
    }

    const level = match[1].length;
    const text = cleanHeadingText(match[2]);
    const base = slugify(text) || `section-${headings.length + 1}`;
    const seen = (slugCounts.get(base) ?? 0) + 1;
    slugCounts.set(base, seen);
    const id = seen === 1 ? base : `${base}-${seen}`;

    headings.push({ id, level, text });
  }

  return headings;
}

function inferSectionLabel(section: string, locale: Locale): string {
  if (section === "root") {
    return locale === "zh" ? "仓库根目录" : "Repository Root";
  }

  if (section === "docs") {
    return locale === "zh" ? "文档总览" : "Docs Core";
  }

  if (section.startsWith("i18n/")) {
    const language = section.split("/")[1] ?? "i18n";
    return locale === "zh"
      ? `多语言 / ${formatLanguage(language, locale)}`
      : `i18n / ${formatLanguage(language, locale)}`;
  }

  const pretty = section.replace(/[-_]/g, " ");
  return pretty.charAt(0).toUpperCase() + pretty.slice(1);
}

function localize(value: Localized, locale: Locale): string {
  return value[locale] ?? value.en;
}

function formatLanguage(language: string, locale: Locale): string {
  if (languageNames[language]) {
    return localize(languageNames[language], locale);
  }

  if (language === "en") {
    return locale === "zh" ? "英文" : "English";
  }

  return language;
}

function formatJourney(journey: Journey, locale: Locale): string {
  return localize(journeyNames[journey], locale);
}

function formatAudience(audience: Audience, locale: Locale): string {
  return localize(audienceNames[audience], locale);
}

function formatKind(kind: DocKind, locale: Locale): string {
  return localize(kindNames[kind], locale);
}

function groupLabel(groupBy: GroupMode, key: string, locale: Locale) {
  if (groupBy === "journey") {
    return formatJourney(asJourney(key), locale);
  }
  if (groupBy === "kind") {
    return formatKind(asKind(key), locale);
  }
  if (groupBy === "language") {
    return formatLanguage(key, locale);
  }
  return inferSectionLabel(key, locale);
}

function groupOrderIndex(groupBy: GroupMode, key: string): number {
  if (groupBy === "journey") {
    return journeyOrder.indexOf(asJourney(key));
  }
  if (groupBy === "kind") {
    return kindOrder.indexOf(asKind(key));
  }
  if (groupBy === "language") {
    return key === "en" ? -1 : 0;
  }
  if (groupBy === "section") {
    if (key === "root") return -2;
    if (key === "docs") return -1;
  }
  return 999;
}

function canonicalDocPath(candidate: string, docSet: Set<string>): string | null {
  const normalized = normalizePath(candidate);
  const attempts = new Set<string>([normalized]);

  if (normalized.endsWith("/")) {
    attempts.add(`${normalized}README.md`);
    attempts.add(`${normalized}README.mdx`);
  }

  if (!/\.[a-zA-Z0-9]+$/.test(normalized)) {
    attempts.add(`${normalized}.md`);
    attempts.add(`${normalized}.mdx`);
    attempts.add(`${normalized}/README.md`);
    attempts.add(`${normalized}/README.mdx`);
  }

  for (const attempt of attempts) {
    if (docSet.has(attempt)) {
      return attempt;
    }
  }

  return null;
}

function resolveRelativePath(fromPath: string, target: string): {
  path: string;
  hash: string;
} {
  const base = new URL(`https://repo.local/${normalizePath(fromPath)}`);
  const resolved = new URL(target, base);

  return {
    path: normalizePath(decodeURIComponent(resolved.pathname)),
    hash: decodeURIComponent(resolved.hash.replace(/^#/, "")),
  };
}

function getInitialDocPath(docSet: Set<string>): string {
  if (typeof window === "undefined") {
    return docs[0]?.path ?? "";
  }

  const url = new URL(window.location.href);
  const requested = url.searchParams.get("doc");
  if (requested) {
    const decoded = normalizePath(decodeURIComponent(requested));
    if (docSet.has(decoded)) {
      return decoded;
    }
  }

  return docs[0]?.path ?? "";
}

export default function App(): JSX.Element {
  const [locale, setLocale] = useState<Locale>(() => {
    if (typeof window === "undefined") {
      return "en";
    }
    return normalizeLocale(window.localStorage.getItem("zc-locale"));
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

  const docSet = useMemo(() => new Set(docs.map((doc) => doc.path)), []);

  const [selectedPath, setSelectedPath] = useState<string>(() =>
    getInitialDocPath(docSet)
  );

  const [resolvedTheme, setResolvedTheme] = useState<ResolvedTheme>("dark");
  const [query, setQuery] = useState("");
  const [sectionFilter, setSectionFilter] = useState("all");
  const [languageFilter, setLanguageFilter] = useState("all");
  const [journeyFilter, setJourneyFilter] = useState<Journey | "all">("all");
  const [audienceFilter, setAudienceFilter] = useState<Audience | "all">("all");
  const [kindFilter, setKindFilter] = useState<DocKind | "all">("all");
  const [groupBy, setGroupBy] = useState<GroupMode>("journey");
  const [activePathway, setActivePathway] = useState<string | null>(null);

  const [markdownCache, setMarkdownCache] = useState<Record<string, string>>({});
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState("");

  const [paletteOpen, setPaletteOpen] = useState(false);
  const [paletteQuery, setPaletteQuery] = useState("");

  const [readerScale, setReaderScale] = useState<ReaderScale>("comfortable");
  const [readerWidth, setReaderWidth] = useState<ReaderWidth>("normal");

  const [pendingAnchor, setPendingAnchor] = useState<string>("");

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

  useEffect(() => {
    const cardSelector = ".metric-card, .principle-card, .pathway-card, .reader-side .side-card";
    const cards = [...document.querySelectorAll<HTMLElement>(cardSelector)];

    if (!cards.length) {
      return;
    }

    const cleanups = cards.map((card) => {
      const onPointerMove = (event: PointerEvent): void => {
        const bounds = card.getBoundingClientRect();
        const x = event.clientX - bounds.left;
        const y = event.clientY - bounds.top;

        card.style.setProperty("--spotlight-x", `${x}px`);
        card.style.setProperty("--spotlight-y", `${y}px`);
        card.style.setProperty("--spotlight-opacity", "1");
      };

      const onPointerEnter = (): void => {
        card.style.setProperty("--spotlight-opacity", "0.9");
      };

      const onPointerLeave = (): void => {
        card.style.setProperty("--spotlight-opacity", "0");
      };

      card.addEventListener("pointermove", onPointerMove);
      card.addEventListener("pointerenter", onPointerEnter);
      card.addEventListener("pointerleave", onPointerLeave);

      return () => {
        card.removeEventListener("pointermove", onPointerMove);
        card.removeEventListener("pointerenter", onPointerEnter);
        card.removeEventListener("pointerleave", onPointerLeave);
      };
    });

    return () => {
      cleanups.forEach((cleanup) => cleanup());
    };
  }, []);

  const sectionOptions = useMemo(
    () => ["all", ...new Set(docs.map((doc) => doc.section))],
    []
  );

  const languageOptions = useMemo(
    () => ["all", ...new Set(docs.map((doc) => doc.language))],
    []
  );

  const journeyOptions = useMemo(
    () => ["all", ...journeyOrder.filter((journey) => docs.some((doc) => doc.journey === journey))],
    []
  );

  const audienceOptions = useMemo(
    () => ["all", ...audienceOrder.filter((audience) => docs.some((doc) => doc.audience === audience))],
    []
  );

  const kindOptions = useMemo(
    () => ["all", ...kindOrder.filter((kind) => docs.some((doc) => doc.kind === kind))],
    []
  );

  const filteredDocs = useMemo(() => {
    const needle = query.trim().toLowerCase();

    return docs.filter((doc) => {
      if (sectionFilter !== "all" && doc.section !== sectionFilter) {
        return false;
      }

      if (languageFilter !== "all" && doc.language !== languageFilter) {
        return false;
      }

      if (journeyFilter !== "all" && doc.journey !== journeyFilter) {
        return false;
      }

      if (audienceFilter !== "all" && doc.audience !== audienceFilter) {
        return false;
      }

      if (kindFilter !== "all" && doc.kind !== kindFilter) {
        return false;
      }

      if (!needle) {
        return true;
      }

      const bag = [
        doc.title,
        doc.summary,
        doc.path,
        doc.section,
        doc.language,
        doc.journey,
        doc.audience,
        doc.kind,
        doc.tags.join(" "),
      ]
        .join(" ")
        .toLowerCase();

      return bag.includes(needle);
    });
  }, [audienceFilter, journeyFilter, kindFilter, languageFilter, query, sectionFilter]);

  const pathwayStats = useMemo(
    () =>
      readingPaths.map((pathway) => {
        const total = docs.filter((doc) => {
          if (pathway.filters.journey && doc.journey !== pathway.filters.journey) {
            return false;
          }
          if (pathway.filters.audience && doc.audience !== pathway.filters.audience) {
            return false;
          }
          if (pathway.filters.kind && doc.kind !== pathway.filters.kind) {
            return false;
          }
          if (pathway.filters.section && doc.section !== pathway.filters.section) {
            return false;
          }
          return true;
        }).length;

        return { ...pathway, total };
      }),
    []
  );

  const starterDocs = useMemo(() => {
    const preferredLanguagesByLocale: Record<Locale, string[]> = {
      en: ["en"],
      zh: ["zh-CN", "en"],
      es: ["es", "en"],
      pt: ["pt", "en"],
      it: ["it", "en"],
    };
    const preferredLanguages = preferredLanguagesByLocale[locale] ?? ["en"];

    return docs
      .filter((doc) => doc.startHere)
      .map((doc) => {
        const languageRank = preferredLanguages.indexOf(doc.language);
        return {
          ...doc,
          _rank:
            (languageRank === -1 ? 9 : languageRank) * 100 +
            journeyOrder.indexOf(doc.journey) * 10 +
            doc.readingMinutes,
        };
      })
      .sort((a, b) => a._rank - b._rank)
      .slice(0, 8);
  }, [locale]);

  const docsByPath = useMemo(() => new Map(docs.map((doc) => [doc.path, doc])), []);

  const selectedDoc =
    docsByPath.get(selectedPath) ?? filteredDocs[0] ?? docs[0] ?? null;

  useEffect(() => {
    if (!selectedDoc) {
      return;
    }

    if (selectedPath !== selectedDoc.path) {
      setSelectedPath(selectedDoc.path);
    }
  }, [selectedDoc, selectedPath]);

  useEffect(() => {
    if (!selectedDoc) {
      return;
    }

    const url = new URL(window.location.href);
    const current = url.searchParams.get("doc");

    if (current !== selectedDoc.path) {
      url.searchParams.set("doc", selectedDoc.path);
      window.history.replaceState({}, "", `${url.pathname}?${url.searchParams.toString()}`);
    }
  }, [selectedDoc]);

  useEffect(() => {
    function onPopState(): void {
      const url = new URL(window.location.href);
      const next = url.searchParams.get("doc");
      if (!next) {
        return;
      }

      const normalized = normalizePath(decodeURIComponent(next));
      if (docSet.has(normalized)) {
        setSelectedPath(normalized);
      }
    }

    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, [docSet]);

  const activePath = selectedDoc?.path ?? "";
  const markdown = markdownCache[activePath] ?? "";

  useEffect(() => {
    if (!selectedDoc) {
      return;
    }

    if (markdownCache[activePath]) {
      return;
    }

    let cancelled = false;
    const controller = new AbortController();

    async function loadDoc(): Promise<void> {
      setLoading(true);
      setError("");

      const localUrl = docsContentUrl(activePath);
      const fallbackRawUrl = withRaw(activePath);

      const tryFetch = async (url: string): Promise<string | null> => {
        try {
          const response = await fetch(url, { signal: controller.signal });
          if (!response.ok) {
            return null;
          }
          return await response.text();
        } catch {
          return null;
        }
      };

      const localContent = await tryFetch(localUrl);
      const content = localContent ?? (await tryFetch(fallbackRawUrl));

      if (cancelled) {
        return;
      }

      if (content === null) {
        setError("fetch_failed");
        setLoading(false);
        return;
      }

      setMarkdownCache((prev) => ({
        ...prev,
        [activePath]: content,
      }));
      setLoading(false);
    }

    void loadDoc();

    return () => {
      cancelled = true;
      controller.abort();
    };
  }, [activePath, markdownCache, selectedDoc]);

  const headings = useMemo(() => extractHeadings(markdown), [markdown]);

  useEffect(() => {
    if (!pendingAnchor) {
      return;
    }

    const timer = window.setTimeout(() => {
      const target = document.getElementById(pendingAnchor);
      if (target) {
        target.scrollIntoView({ behavior: "smooth", block: "start" });
      }
      setPendingAnchor("");
    }, 90);

    return () => window.clearTimeout(timer);
  }, [headings, pendingAnchor]);

  const groupedDocs = useMemo(() => {
    const grouped = new Map<string, ManifestDoc[]>();

    for (const doc of filteredDocs) {
      const key =
        groupBy === "journey"
          ? doc.journey
          : groupBy === "kind"
            ? doc.kind
            : groupBy === "language"
              ? doc.language
              : doc.section;

      if (!grouped.has(key)) {
        grouped.set(key, []);
      }
      grouped.get(key)?.push(doc);
    }

    return [...grouped.entries()]
      .map(([key, entries]) => [
        key,
        [...entries].sort((a, b) => a.title.localeCompare(b.title)),
      ] as const)
      .sort((a, b) => {
        const aIndex = groupOrderIndex(groupBy, a[0]);
        const bIndex = groupOrderIndex(groupBy, b[0]);
        if (aIndex !== bIndex) {
          return aIndex - bIndex;
        }
        return groupLabel(groupBy, a[0], locale).localeCompare(groupLabel(groupBy, b[0], locale));
      });
  }, [filteredDocs, groupBy, locale]);

  const currentIndex = filteredDocs.findIndex((doc) => doc.path === activePath);
  const previousDoc = currentIndex > 0 ? filteredDocs[currentIndex - 1] : null;
  const nextDoc =
    currentIndex >= 0 && currentIndex < filteredDocs.length - 1
      ? filteredDocs[currentIndex + 1]
      : null;

  const relatedDocs = useMemo(() => {
    if (!selectedDoc) {
      return [];
    }

    const selectedTags = new Set(selectedDoc.tags);

    return docs
      .filter((doc) => doc.path !== selectedDoc.path)
      .map((doc) => {
        let score = 0;

        if (doc.journey === selectedDoc.journey) score += 4;
        if (doc.audience === selectedDoc.audience) score += 3;
        if (doc.kind === selectedDoc.kind) score += 2;
        if (doc.section === selectedDoc.section) score += 2;
        if (doc.language === selectedDoc.language) score += 1;

        const sharedTags = doc.tags.filter((tag) => selectedTags.has(tag)).length;
        score += sharedTags * 2;

        return { doc, score };
      })
      .filter((entry) => entry.score > 0)
      .sort((a, b) => {
        if (b.score !== a.score) {
          return b.score - a.score;
        }
        if (a.doc.readingMinutes !== b.doc.readingMinutes) {
          return a.doc.readingMinutes - b.doc.readingMinutes;
        }
        return a.doc.title.localeCompare(b.doc.title);
      })
      .slice(0, 8)
      .map((entry) => entry.doc);
  }, [selectedDoc]);

  const openDoc = (docPath: string, anchor = ""): void => {
    if (!docSet.has(docPath)) {
      return;
    }

    setSelectedPath(docPath);
    if (anchor) {
      setPendingAnchor(anchor);
    }
  };

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

  const applyPathway = (pathwayId: string): void => {
    const pathway = readingPaths.find((entry) => entry.id === pathwayId);
    if (!pathway) {
      return;
    }

    setActivePathway(pathway.id);
    setQuery("");
    setSectionFilter(pathway.filters.section ?? "all");
    setLanguageFilter("all");
    setJourneyFilter(pathway.filters.journey ?? "all");
    setAudienceFilter(pathway.filters.audience ?? "all");
    setKindFilter(pathway.filters.kind ?? "all");
    setGroupBy("journey");
  };

  const resetFilters = (): void => {
    setActivePathway(null);
    setQuery("");
    setSectionFilter("all");
    setLanguageFilter("all");
    setJourneyFilter("all");
    setAudienceFilter("all");
    setKindFilter("all");
    setGroupBy("journey");
  };

  const paletteActions = useMemo(
    () => [
      {
        id: "focus-search",
        label: text.actionFocus,
        hint: text.docsWorkspace,
        run: () => {
          document
            .getElementById("docs-workspace")
            ?.scrollIntoView({ behavior: "smooth", block: "start" });
          setTimeout(() => docsSearchRef.current?.focus(), 220);
        },
      },
      {
        id: "back-top",
        label: text.actionTop,
        hint: "Home",
        run: jumpToTop,
      },
      {
        id: "toggle-theme",
        label: text.actionTheme,
        hint: `${text.status}: ${resolvedTheme}`,
        run: cycleTheme,
      },
      {
        id: "toggle-locale",
        label: text.actionLocale,
        hint: `${localeLabels[locale]} -> ${localeLabels[nextLocale(locale)]}`,
        run: () => setLocale((prev) => nextLocale(prev)),
      },
    ],
    [locale, resolvedTheme, text.actionFocus, text.actionLocale, text.actionTheme, text.actionTop, text.docsWorkspace, text.status]
  );

  const paletteResults = useMemo(() => {
    const needle = paletteQuery.trim().toLowerCase();

    const actionEntries: PaletteEntry[] = paletteActions;

    const docEntries: PaletteEntry[] = docs
      .filter((doc) => {
        if (!needle) {
          return true;
        }

        return [
          doc.title,
          doc.summary,
          doc.path,
          doc.section,
          doc.language,
          doc.journey,
          doc.audience,
          doc.kind,
          doc.tags.join(" "),
        ]
          .join(" ")
          .toLowerCase()
          .includes(needle);
      })
      .slice(0, 18)
      .map((doc) => ({
        id: `doc-${doc.id}`,
        label: doc.title,
        hint: `${formatJourney(doc.journey, locale)} · ${doc.path}`,
        run: () => {
          openDoc(doc.path);
          document
            .getElementById("docs-workspace")
            ?.scrollIntoView({ behavior: "smooth", block: "start" });
        },
      }));

    if (!needle) {
      return [...actionEntries, ...docEntries.slice(0, 10)];
    }

    const matchedActions = actionEntries.filter((entry) =>
      `${entry.label} ${entry.hint}`.toLowerCase().includes(needle)
    );

    return [...matchedActions, ...docEntries];
  }, [locale, paletteActions, paletteQuery]);

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

  if (!selectedDoc) {
    return <div className="zc-app" />;
  }

  let headingRenderIndex = 0;

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
              {localeOrder.map((entry) => (
                <button
                  key={entry}
                  type="button"
                  className={locale === entry ? "active" : ""}
                  onClick={() => setLocale(entry)}
                  title={formatLanguage(entry === "zh" ? "zh-CN" : entry, locale)}
                >
                  {localeLabels[entry]}
                </button>
              ))}
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
            <div className="hero-layout">
              <div>
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
              </div>

              <aside className="hero-terminal" aria-label={text.commandLaneTitle}>
                <header>
                  <h2>{text.commandLaneTitle}</h2>
                  <p>{text.commandLaneHint}</p>
                </header>
                <ul>
                  {commandLane.map((item) => (
                    <li key={item.command}>
                      <code>{item.command}</code>
                      <span>{localize(item.hint, locale)}</span>
                    </li>
                  ))}
                </ul>
              </aside>
            </div>

            <div className="metrics" aria-label="Project metrics">
              <article className="metric-card">
                <p className="metric-label">Runtime Memory</p>
                <p className="metric-value">&lt; 5MB</p>
              </article>
              <article className="metric-card">
                <p className="metric-label">Cold Start</p>
                <p className="metric-value">&lt; 10ms</p>
              </article>
              <article className="metric-card">
                <p className="metric-label">Edge Hardware</p>
                <p className="metric-value">$10-class</p>
              </article>
              <article className="metric-card">
                <p className="metric-label">Docs Indexed</p>
                <p className="metric-value">{docs.length}</p>
              </article>
            </div>

            <section className="principles" aria-label={text.engineeringTitle}>
              <h2>{text.engineeringTitle}</h2>
              <div className="principles-grid">
                {engineeringPillars.map((pillar) => (
                  <article key={pillar.title.en} className="principle-card">
                    <h3>{localize(pillar.title, locale)}</h3>
                    <p>{localize(pillar.detail, locale)}</p>
                  </article>
                ))}
              </div>
            </section>
          </div>
        </section>

        <section id="docs-workspace" className="docs-shell">
          <div className="docs-head">
            <h2>{text.docsWorkspace}</h2>
            <p>{text.docsLead}</p>
          </div>

          <div className="workspace-meta" aria-label="Workspace state">
            <span>
              {text.docsIndexed}: <strong>{docs.length}</strong>
            </span>
            <span>
              {text.docsFiltered}: <strong>{filteredDocs.length}</strong>
            </span>
            <span>
              {text.docsActive}: <strong>{selectedDoc.title}</strong>
            </span>
            {activePathway ? (
              <span>
                {text.readingPathsTitle}:{" "}
                <strong>
                  {localize(
                    readingPaths.find((entry) => entry.id === activePathway)?.label ?? { en: "-", zh: "-" },
                    locale
                  )}
                </strong>
              </span>
            ) : null}
          </div>

          <section className="pathway-shell" aria-label={text.readingPathsTitle}>
            <header className="pathway-head">
              <h3>{text.readingPathsTitle}</h3>
              <p>{text.readingPathsLead}</p>
            </header>
            <div className="pathway-grid">
              {pathwayStats.map((pathway) => (
                <button
                  key={pathway.id}
                  type="button"
                  className={`pathway-card ${activePathway === pathway.id ? "active" : ""}`}
                  onClick={() => applyPathway(pathway.id)}
                >
                  <span className="pathway-title">{localize(pathway.label, locale)}</span>
                  <span className="pathway-detail">{localize(pathway.detail, locale)}</span>
                  <span className="pathway-count">{pathway.total}</span>
                </button>
              ))}
            </div>
          </section>

          <section className="starter-shell" aria-label={text.startHereTitle}>
            <header className="starter-head">
              <h3>{text.startHereTitle}</h3>
              <p>{text.startHereLead}</p>
            </header>
            <div className="starter-list">
              {starterDocs.length === 0 ? (
                <p className="empty-hint">{text.noStartHere}</p>
              ) : (
                starterDocs.map((doc) => (
                  <button
                    key={doc.id}
                    type="button"
                    className={`starter-item ${doc.path === activePath ? "active" : ""}`}
                    onClick={() => openDoc(doc.path)}
                  >
                    <span className="starter-title">{doc.title}</span>
                    <span className="starter-meta">
                      {formatJourney(doc.journey, locale)} · {doc.readingMinutes}
                      {text.minuteUnit}
                    </span>
                  </button>
                ))
              )}
            </div>
          </section>

          <div className="docs-toolbar">
            <input
              ref={docsSearchRef}
              type="search"
              value={query}
              onChange={(event) => {
                setActivePathway(null);
                setQuery(event.target.value);
              }}
              placeholder={text.search}
              aria-label={text.search}
            />

            <select
              value={journeyFilter}
              onChange={(event) => {
                setActivePathway(null);
                setJourneyFilter(event.target.value as Journey | "all");
              }}
              aria-label={text.journeyFilter}
            >
              <option value="all">{text.allJourneys}</option>
              {journeyOptions
                .filter((journey) => journey !== "all")
                .map((journey) => (
                  <option key={journey} value={journey}>
                    {formatJourney(journey as Journey, locale)}
                  </option>
                ))}
            </select>

            <select
              value={audienceFilter}
              onChange={(event) => {
                setActivePathway(null);
                setAudienceFilter(event.target.value as Audience | "all");
              }}
              aria-label={text.audienceFilter}
            >
              <option value="all">{text.allAudiences}</option>
              {audienceOptions
                .filter((audience) => audience !== "all")
                .map((audience) => (
                  <option key={audience} value={audience}>
                    {formatAudience(audience as Audience, locale)}
                  </option>
                ))}
            </select>

            <select
              value={kindFilter}
              onChange={(event) => {
                setActivePathway(null);
                setKindFilter(event.target.value as DocKind | "all");
              }}
              aria-label={text.kindFilter}
            >
              <option value="all">{text.allKinds}</option>
              {kindOptions
                .filter((kind) => kind !== "all")
                .map((kind) => (
                  <option key={kind} value={kind}>
                    {formatKind(kind as DocKind, locale)}
                  </option>
                ))}
            </select>

            <select
              value={sectionFilter}
              onChange={(event) => {
                setActivePathway(null);
                setSectionFilter(event.target.value);
              }}
              aria-label={text.sectionFilter}
            >
              <option value="all">{text.allSections}</option>
              {sectionOptions
                .filter((section) => section !== "all")
                .map((section) => (
                  <option key={section} value={section}>
                    {inferSectionLabel(section, locale)}
                  </option>
                ))}
            </select>

            <select
              value={languageFilter}
              onChange={(event) => {
                setActivePathway(null);
                setLanguageFilter(event.target.value);
              }}
              aria-label={text.languageFilter}
            >
              <option value="all">{text.allLanguages}</option>
              {languageOptions
                .filter((language) => language !== "all")
                .map((language) => (
                  <option key={language} value={language}>
                    {formatLanguage(language, locale)}
                  </option>
                ))}
            </select>

            <select
              value={groupBy}
              onChange={(event) => setGroupBy(event.target.value as GroupMode)}
              aria-label={text.groupBy}
            >
              <option value="journey">{text.groupJourney}</option>
              <option value="section">{text.groupSection}</option>
              <option value="kind">{text.groupKind}</option>
              <option value="language">{text.groupLanguage}</option>
            </select>

            <button type="button" className="btn ghost" onClick={resetFilters}>
              {text.resetFilters}
            </button>

            <button type="button" className="btn ghost" onClick={() => setPaletteOpen(true)}>
              {text.commandPalette}
            </button>
          </div>

          <div className="workspace-grid">
            <aside className="doc-list" aria-label="Document list">
              {filteredDocs.length === 0 ? (
                <p className="empty-hint">{text.empty}</p>
              ) : (
                groupedDocs.map(([groupKey, sectionDocs]) => (
                  <section key={groupKey} className="doc-group">
                    <h3>{groupLabel(groupBy, groupKey, locale)}</h3>
                    <div>
                      {sectionDocs.map((doc) => {
                        const isActive = doc.path === activePath;
                        return (
                          <button
                            key={doc.id}
                            type="button"
                            className={`doc-item ${isActive ? "active" : ""}`}
                            onClick={() => openDoc(doc.path)}
                          >
                            <span className="doc-meta">
                              <span>{formatLanguage(doc.language, locale)}</span>
                              <span>{formatJourney(doc.journey, locale)}</span>
                              <span>{formatKind(doc.kind, locale)}</span>
                            </span>
                            <span className="doc-title">{doc.title}</span>
                            <span className="doc-summary">{doc.summary}</span>
                            <span className="doc-chip-row">
                              <span className="doc-chip">{formatAudience(doc.audience, locale)}</span>
                              <span className="doc-chip">
                                {doc.readingMinutes}
                                {text.minuteUnit}
                              </span>
                              {doc.startHere ? <span className="doc-chip">{text.startBadge}</span> : null}
                            </span>
                            <span className="doc-path">{doc.path}</span>
                          </button>
                        );
                      })}
                    </div>
                  </section>
                ))
              )}
            </aside>

            <section className="doc-reader" aria-live="polite">
              <header className="reader-head">
                <div>
                  <p>{text.sourceLabel}</p>
                  <div className="doc-breadcrumb">
                    <span>{formatJourney(selectedDoc.journey, locale)}</span>
                    <span>/</span>
                    <span>{inferSectionLabel(selectedDoc.section, locale)}</span>
                    <span>/</span>
                    <span>{selectedDoc.title}</span>
                  </div>
                  <h3>{selectedDoc.title}</h3>
                  <code>{activePath}</code>
                  <div className="reader-meta-line">
                    <span>
                      {text.docJourney}: <strong>{formatJourney(selectedDoc.journey, locale)}</strong>
                    </span>
                    <span>
                      {text.docAudience}:{" "}
                      <strong>{formatAudience(selectedDoc.audience, locale)}</strong>
                    </span>
                    <span>
                      {text.docKind}: <strong>{formatKind(selectedDoc.kind, locale)}</strong>
                    </span>
                    <span>
                      {text.docReadTime}: <strong>{selectedDoc.readingMinutes}</strong>{" "}
                      {text.minuteUnit}
                    </span>
                  </div>
                  {selectedDoc.tags.length > 0 ? (
                    <div className="reader-tags" aria-label={text.docTags}>
                      {selectedDoc.tags.map((tag) => (
                        <span key={tag}>{tag}</span>
                      ))}
                    </div>
                  ) : null}
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
                <article className={`markdown-body size-${readerScale} width-${readerWidth}`}>
                  <ReactMarkdown
                    remarkPlugins={[remarkGfm]}
                    components={{
                      h1: ({ children }) => {
                        const id = headings[headingRenderIndex]?.id ?? slugify(nodeText(children));
                        headingRenderIndex += 1;
                        return <h1 id={id}>{children}</h1>;
                      },
                      h2: ({ children }) => {
                        const id = headings[headingRenderIndex]?.id ?? slugify(nodeText(children));
                        headingRenderIndex += 1;
                        return <h2 id={id}>{children}</h2>;
                      },
                      h3: ({ children }) => {
                        const id = headings[headingRenderIndex]?.id ?? slugify(nodeText(children));
                        headingRenderIndex += 1;
                        return <h3 id={id}>{children}</h3>;
                      },
                      a: ({ href, children }) => {
                        const target = href ?? "";

                        if (!target) {
                          return <span>{children}</span>;
                        }

                        if (target.startsWith("#")) {
                          const anchor = decodeURIComponent(target.replace(/^#/, ""));
                          return (
                            <a
                              href={target}
                              onClick={(event) => {
                                event.preventDefault();
                                setPendingAnchor(anchor);
                              }}
                            >
                              {children}
                            </a>
                          );
                        }

                        if (/^[a-z]+:/i.test(target)) {
                          return (
                            <a href={target} target="_blank" rel="noreferrer">
                              {children}
                            </a>
                          );
                        }

                        const resolved = resolveRelativePath(activePath, target);
                        const docPath = canonicalDocPath(resolved.path, docSet);

                        if (docPath) {
                          const hrefDoc = `?doc=${encodeURIComponent(docPath)}`;
                          const hrefAnchor = resolved.hash ? `#${encodeURIComponent(resolved.hash)}` : "";
                          return (
                            <a
                              href={`${hrefDoc}${hrefAnchor}`}
                              onClick={(event) => {
                                event.preventDefault();
                                openDoc(docPath, resolved.hash);
                              }}
                            >
                              {children}
                            </a>
                          );
                        }

                        const looksLikeAsset =
                          resolved.path.startsWith("docs/") ||
                          /\.(png|jpe?g|gif|webp|svg|avif|txt|toml|json|yaml|yml)$/i.test(
                            resolved.path
                          );

                        if (looksLikeAsset) {
                          return (
                            <a
                              href={docsContentUrl(resolved.path)}
                              target="_blank"
                              rel="noreferrer"
                            >
                              {children}
                            </a>
                          );
                        }

                        return (
                          <a href={withRepo(resolved.path)} target="_blank" rel="noreferrer">
                            {children}
                          </a>
                        );
                      },
                      img: ({ src, alt }) => {
                        const original = src ?? "";
                        if (!original) {
                          return null;
                        }

                        if (/^https?:\/\//i.test(original) || original.startsWith("data:")) {
                          return <img src={original} alt={alt ?? ""} loading="lazy" />;
                        }

                        const resolved = resolveRelativePath(activePath, original);
                        const localAsset = docsContentUrl(resolved.path);

                        return (
                          <img
                            src={localAsset}
                            alt={alt ?? ""}
                            loading="lazy"
                            onError={(event) => {
                              (event.currentTarget as HTMLImageElement).src = withRaw(
                                resolved.path
                              );
                            }}
                          />
                        );
                      },
                    }}
                  >
                    {markdown}
                  </ReactMarkdown>
                </article>
              ) : null}

              <footer className="reader-nav">
                <button
                  type="button"
                  disabled={!previousDoc}
                  onClick={() => previousDoc && openDoc(previousDoc.path)}
                >
                  {text.previousDoc}
                </button>
                <button
                  type="button"
                  disabled={!nextDoc}
                  onClick={() => nextDoc && openDoc(nextDoc.path)}
                >
                  {text.nextDoc}
                </button>
              </footer>
            </section>

            <aside className="reader-side" aria-label="Reader controls">
              <section className="side-card">
                <h3>{text.outline}</h3>
                {headings.length === 0 ? (
                  <p>{text.noOutline}</p>
                ) : (
                  <ul className="toc-list">
                    {headings.map((heading) => (
                      <li key={heading.id} data-level={heading.level}>
                        <button type="button" onClick={() => setPendingAnchor(heading.id)}>
                          {heading.text}
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </section>

              <section className="side-card">
                <h3>{text.relatedDocs}</h3>
                {relatedDocs.length === 0 ? (
                  <p>{text.noRelated}</p>
                ) : (
                  <div className="related-list">
                    {relatedDocs.map((doc) => (
                      <button key={doc.id} type="button" onClick={() => openDoc(doc.path)}>
                        <span>{doc.title}</span>
                        <small>
                          {formatJourney(doc.journey, locale)} · {doc.readingMinutes}
                          {text.minuteUnit}
                        </small>
                      </button>
                    ))}
                  </div>
                )}
              </section>

              <section className="side-card">
                <h3>{text.reading}</h3>

                <div className="side-control">
                  <p>{text.scaleLabel}</p>
                  <div className="pill-row">
                    {(["compact", "comfortable", "relaxed"] as ReaderScale[]).map((scale) => (
                      <button
                        key={scale}
                        type="button"
                        className={readerScale === scale ? "active" : ""}
                        onClick={() => setReaderScale(scale)}
                      >
                        {scale === "compact"
                          ? text.compact
                          : scale === "comfortable"
                            ? text.comfortable
                            : text.relaxed}
                      </button>
                    ))}
                  </div>
                </div>

                <div className="side-control">
                  <p>{text.widthLabel}</p>
                  <div className="pill-row">
                    {(["normal", "wide"] as ReaderWidth[]).map((width) => (
                      <button
                        key={width}
                        type="button"
                        className={readerWidth === width ? "active" : ""}
                        onClick={() => setReaderWidth(width)}
                      >
                        {width === "normal" ? text.normalWidth : text.wideWidth}
                      </button>
                    ))}
                  </div>
                </div>
              </section>
            </aside>
          </div>
        </section>
      </main>

      <footer className="footer">
        <p>
          ZeroClaw · Trait-driven architecture · secure-by-default runtime ·
          pluggable everything
        </p>
      </footer>

      {paletteOpen ? (
        <div className="palette-backdrop" onClick={() => setPaletteOpen(false)}>
          <div
            className="palette"
            role="dialog"
            aria-modal="true"
            onClick={(event) => event.stopPropagation()}
          >
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
              {paletteResults.slice(0, 16).map((entry) => (
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
