/**
 * Overview page (M5.0, US-002 + US-003).
 *
 * Aggregates four read-only resource summaries into a 2×2 card grid:
 *   - Memory      → /api/memory       (count + most recent timestamp)
 *   - Crons       → /api/cron         (count + next run time)
 *   - MCP         → /api/integrations (active count over total)
 *   - Skills      → /api/tools        (count)
 *
 * Each card has independent loading / error states so a single
 * endpoint outage does not break the whole page.
 *
 * The card "Open" links point to per-resource deep pages that don't
 * exist yet (M5.1+ adds them in their own PRs). Until those land, the
 * links resolve to /overview itself — they appear in markup as
 * placeholders whose hrefs become live in subsequent M5.x PRs.
 *
 * Strings are inline JSX rather than localised through `fl!()` to
 * match the rest of the dashboard's M3/M4 conventions; localisation
 * is M7-scope per plan §5.
 */
import { useMemo } from "react";
import { Link } from "react-router-dom";
import {
  Boxes,
  Brain,
  CalendarClock,
  Wrench,
  type LucideIcon,
} from "lucide-react";
import { useControlUiBootstrap } from "@/app/ControlUiBootstrapProvider";
import { SectionNav } from "@/app/SectionNav";
import { ThemeSwitcher } from "@/theme/ThemeSwitcher";
import {
  pickMostRecentMemory,
  pickNextCronRun,
  useCronsOverview,
  useIntegrationsOverview,
  useMemoryOverview,
  useToolsOverview,
} from "@/overview/overviewQueries";

export function OverviewPage() {
  const bootstrap = useControlUiBootstrap();

  return (
    <div className="flex flex-col h-full">
      <header
        className="flex items-center justify-between gap-2 px-4 py-3 border-b"
        style={{ borderColor: "var(--color-border)" }}
      >
        <div className="flex items-center gap-3 text-sm">
          <span className="font-semibold">
            {bootstrap.assistant_identity.name}
          </span>
          <span style={{ color: "var(--color-text-muted)" }}>·</span>
          <SectionNav layout="inline" />
        </div>
        <div className="flex items-center gap-2">
          <span className="text-xs opacity-50">
            v{bootstrap.server_version}
          </span>
          <ThemeSwitcher />
        </div>
      </header>

      <main className="flex-1 overflow-auto p-4">
        <h1 className="text-base font-semibold mb-3">Overview</h1>
        <p
          className="text-sm mb-6 max-w-2xl"
          style={{ color: "var(--color-text-muted)" }}
        >
          Quick view of memory entries, scheduled jobs, configured
          integrations, and tools available to the agent. Each card
          links to its full page.
        </p>
        <div className="grid grid-cols-1 sm:grid-cols-2 gap-4 max-w-4xl">
          <MemoryCard />
          <CronsCard />
          <IntegrationsCard />
          <SkillsCard />
        </div>
      </main>
    </div>
  );
}

// ── Card scaffold ──────────────────────────────────────────────────

interface CardShellProps {
  title: string;
  icon: LucideIcon;
  href: string;
  isLoading?: boolean;
  error?: unknown;
  children?: React.ReactNode;
}

/**
 * Each Overview card shares the same outer chrome — title row, icon,
 * "Open" link, body slot. Loading and error states render inside the
 * shell so the grid layout never reflows.
 */
function CardShell({
  title,
  icon: Icon,
  href,
  isLoading,
  error,
  children,
}: CardShellProps) {
  return (
    <article
      className="rounded border p-3 flex flex-col gap-2"
      style={{
        borderColor: "var(--color-border)",
        background: "var(--color-surface)",
      }}
    >
      <header className="flex items-center gap-2">
        <Icon size={14} aria-hidden="true" />
        <h2 className="text-sm font-semibold flex-1">{title}</h2>
        <Link
          to={href}
          className="text-xs underline"
          style={{ color: "var(--color-text-muted)" }}
        >
          Open
        </Link>
      </header>
      <div className="text-sm min-h-[3rem]">
        {isLoading ? (
          <CardSkeleton />
        ) : error ? (
          <p className="text-red-600 text-xs" role="alert">
            {String(error)}
          </p>
        ) : (
          children
        )}
      </div>
    </article>
  );
}

function CardSkeleton() {
  return (
    <div
      className="animate-pulse h-10 rounded"
      style={{ background: "var(--color-surface-muted)" }}
      aria-label="Loading"
    />
  );
}

// ── Memory ─────────────────────────────────────────────────────────

function MemoryCard() {
  const { data, isLoading, error } = useMemoryOverview();
  const count = data?.entries.length ?? 0;
  const recent = useMemo(
    () => (data ? pickMostRecentMemory(data.entries) : null),
    [data],
  );

  return (
    <CardShell
      title="Memory"
      icon={Brain}
      href="/overview"
      isLoading={isLoading}
      error={error}
    >
      <div className="flex items-baseline gap-2">
        <span className="text-2xl font-semibold tabular-nums">{count}</span>
        <span
          className="text-xs"
          style={{ color: "var(--color-text-muted)" }}
        >
          {count === 1 ? "entry" : "entries"}
        </span>
      </div>
      {recent ? (
        <p
          className="text-xs mt-1 truncate"
          style={{ color: "var(--color-text-muted)" }}
          title={recent.content}
        >
          Latest: {formatRelative(recent.timestamp)} —{" "}
          <span style={{ color: "var(--color-text)" }}>{recent.key}</span>
        </p>
      ) : null}
    </CardShell>
  );
}

// ── Crons ──────────────────────────────────────────────────────────

function CronsCard() {
  const { data, isLoading, error } = useCronsOverview();
  const jobs = data?.jobs ?? [];
  const enabledCount = jobs.filter((j) => j.enabled).length;
  const next = useMemo(() => pickNextCronRun(jobs), [jobs]);

  return (
    <CardShell
      title="Scheduled jobs"
      icon={CalendarClock}
      href="/overview"
      isLoading={isLoading}
      error={error}
    >
      <div className="flex items-baseline gap-2">
        <span className="text-2xl font-semibold tabular-nums">
          {enabledCount}
        </span>
        <span
          className="text-xs"
          style={{ color: "var(--color-text-muted)" }}
        >
          enabled / {jobs.length} total
        </span>
      </div>
      {next ? (
        <p
          className="text-xs mt-1 truncate"
          style={{ color: "var(--color-text-muted)" }}
          title={`${next.expression} (${next.next_run})`}
        >
          Next:{" "}
          <span style={{ color: "var(--color-text)" }}>
            {next.name ?? next.id}
          </span>{" "}
          in {formatRelative(next.next_run)}
        </p>
      ) : (
        <p
          className="text-xs mt-1"
          style={{ color: "var(--color-text-muted)" }}
        >
          No enabled jobs.
        </p>
      )}
    </CardShell>
  );
}

// ── Integrations / MCP ─────────────────────────────────────────────

function IntegrationsCard() {
  const { data, isLoading, error } = useIntegrationsOverview();
  const all = data?.integrations ?? [];
  const active = all.filter((i) => i.status === "Active").length;

  return (
    <CardShell
      title="Integrations"
      icon={Boxes}
      href="/overview"
      isLoading={isLoading}
      error={error}
    >
      <div className="flex items-baseline gap-2">
        <span className="text-2xl font-semibold tabular-nums">{active}</span>
        <span
          className="text-xs"
          style={{ color: "var(--color-text-muted)" }}
        >
          active / {all.length} available
        </span>
      </div>
      {all.length === 0 ? null : (
        <p
          className="text-xs mt-1"
          style={{ color: "var(--color-text-muted)" }}
        >
          {summariseCategories(all)}
        </p>
      )}
    </CardShell>
  );
}

// ── Skills / tools ─────────────────────────────────────────────────

function SkillsCard() {
  const { data, isLoading, error } = useToolsOverview();
  const count = data?.tools.length ?? 0;

  return (
    <CardShell
      title="Skills"
      icon={Wrench}
      href="/overview"
      isLoading={isLoading}
      error={error}
    >
      <div className="flex items-baseline gap-2">
        <span className="text-2xl font-semibold tabular-nums">{count}</span>
        <span
          className="text-xs"
          style={{ color: "var(--color-text-muted)" }}
        >
          {count === 1 ? "tool registered" : "tools registered"}
        </span>
      </div>
    </CardShell>
  );
}

// ── Formatting helpers ─────────────────────────────────────────────

/**
 * Renders a difference like "in 5m", "3h ago" without pulling in a
 * date library. Browser-native `Intl.RelativeTimeFormat` is ES2020
 * and is in every browser TS targets.
 */
function formatRelative(iso: string): string {
  const ts = Date.parse(iso);
  if (Number.isNaN(ts)) return iso;
  const diffSec = Math.round((ts - Date.now()) / 1000);
  const fmt = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  const abs = Math.abs(diffSec);
  if (abs < 60) return fmt.format(diffSec, "second");
  if (abs < 3600) return fmt.format(Math.round(diffSec / 60), "minute");
  if (abs < 86400) return fmt.format(Math.round(diffSec / 3600), "hour");
  return fmt.format(Math.round(diffSec / 86400), "day");
}

/**
 * Maps Rust IntegrationCategory variant names (the wire format produced
 * by `serde::Serialize` on a unit enum) to the human labels the
 * backend's own `IntegrationCategory::label()` exposes. The mapping
 * lives here rather than calling a hypothetical `/api/integrations/labels`
 * because the variant set is small and stable; if a new variant lands
 * upstream the unmapped string falls through to display unchanged so
 * the card never renders blank.
 *
 * Source of truth: crates/zeroclaw-runtime/src/integrations/mod.rs
 *   IntegrationCategory::label()
 */
const CATEGORY_LABELS: Record<string, string> = {
  Chat: "Chat Providers",
  AiModel: "AI Models",
  ToolsAutomation: "Tools & Automation",
  Platform: "Platforms",
};

function summariseCategories(
  entries: Array<{ category: string; status: string }>,
): string {
  const counts = new Map<string, number>();
  for (const e of entries) {
    if (e.status !== "Active") continue;
    counts.set(e.category, (counts.get(e.category) ?? 0) + 1);
  }
  if (counts.size === 0) return "Nothing configured yet.";
  const parts = [...counts.entries()]
    .sort((a, b) => b[1] - a[1])
    .map(([cat, n]) => `${CATEGORY_LABELS[cat] ?? cat} ${n}`);
  return parts.join(" · ");
}
