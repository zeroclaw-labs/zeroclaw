import { useEffect, useState } from "react";
import { Puzzle, TriangleAlert } from "lucide-react";

import { Badge, Card, PageHeader } from "@/components/ui";
import { getPlugins } from "@/lib/api";
import type {
  PluginCatalogIssue,
  PluginsResponse,
} from "@/lib/api";
import { t } from "@/lib/i18n";
import {
  catalogCapabilities,
  catalogDescription,
  matchesCatalogFilter,
} from "./pluginCatalog";
import type { PluginCatalogFilter } from "./pluginCatalog";

const FILTERS: readonly PluginCatalogFilter[] = [
  "all",
  "installed",
  "available",
];

function issueLabel(issue: PluginCatalogIssue): string {
  if (issue.source === "installed" && issue.code === "discovery_failed") {
    return t("plugins.issue.discovery_failed");
  }
  if (issue.source === "registry" && issue.code === "cache_read_failed") {
    return t("plugins.issue.registry_cache_failed");
  }
  return t("plugins.issue.unknown");
}

function displayToken(token: string): string {
  return token.split("_").join(" ");
}

export default function Plugins() {
  const [response, setResponse] = useState<PluginsResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [filter, setFilter] = useState<PluginCatalogFilter>("all");
  const [reload, setReload] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setResponse(null);
    setError(null);
    getPlugins()
      .then((catalog) => {
        if (!cancelled) setResponse(catalog);
      })
      .catch((reason: unknown) => {
        if (!cancelled) {
          setError(reason instanceof Error ? reason.message : String(reason));
        }
      });
    return () => {
      cancelled = true;
    };
  }, [reload]);

  if (error) {
    return (
      <div className="p-6">
        <div
          role="alert"
          className="space-y-3 rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-4 text-sm text-status-error"
        >
          <p>
            {t("plugins.load_error")}: {error}
          </p>
          <button
            type="button"
            onClick={() => setReload((value) => value + 1)}
            className="rounded-[var(--radius-sm)] border border-current px-3 py-1.5 font-medium focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]"
          >
            {t("common.retry")}
          </button>
        </div>
      </div>
    );
  }

  if (!response) {
    return (
      <div
        role="status"
        aria-live="polite"
        className="flex h-64 items-center justify-center gap-3 text-sm text-pc-text-muted"
      >
        <span
          aria-hidden="true"
          className="h-8 w-8 animate-spin rounded-full border-2 border-pc-border"
          style={{ borderTopColor: "var(--pc-accent)" }}
        />
        <span>{t("plugins.loading")}</span>
      </div>
    );
  }

  const visible = response.plugins.filter((entry) =>
    matchesCatalogFilter(entry, filter),
  );
  const systemStatus = !response.wasm_plugins_available
    ? t("plugins.system_unavailable")
    : response.plugins_enabled
      ? t("plugins.system_enabled")
      : t("plugins.system_disabled");

  return (
    <div className="space-y-6 p-6">
      <PageHeader
        title={t("plugins.title")}
        description={t("plugins.subtitle")}
        actions={
          <Badge
            tone={
              response.wasm_plugins_available && response.plugins_enabled
                ? "ok"
                : "neutral"
            }
          >
            {systemStatus}
          </Badge>
        }
      />

      {!response.wasm_plugins_available && (
        <div
          role="status"
          className="rounded-[var(--radius-md)] border border-pc-border bg-pc-surface p-4 text-sm text-pc-text-muted"
        >
          {t("plugins.wasm_unavailable_hint")}
        </div>
      )}

      {response.issues.length > 0 && (
        <div
          role="alert"
          className="rounded-[var(--radius-md)] border border-status-warning/25 bg-status-warning/10 p-4 text-sm text-pc-text"
        >
          <div className="mb-2 flex items-center gap-2 font-medium">
            <TriangleAlert
              aria-hidden="true"
              className="h-4 w-4 text-status-warning"
            />
            {t("plugins.partial_title")}
          </div>
          <ul className="list-disc space-y-1 pl-5 text-pc-text-muted">
            {response.issues.map((issue) => (
              <li key={`${issue.source}:${issue.code}`}>{issueLabel(issue)}</li>
            ))}
          </ul>
        </div>
      )}

      <div
        className="flex flex-wrap gap-2"
        role="group"
        aria-label={t("plugins.filter_label")}
      >
        {FILTERS.map((option) => {
          const active = filter === option;
          return (
            <button
              key={option}
              type="button"
              aria-pressed={active}
              onClick={() => setFilter(option)}
              className={[
                "inline-flex h-7 cursor-pointer items-center rounded-[var(--radius-md)] border px-3 text-[13px] font-medium transition-colors",
                "focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-[var(--pc-focus)]",
                active
                  ? "border-transparent bg-pc-accent text-[#0b1220]"
                  : "border-pc-border bg-transparent text-pc-text-secondary hover:border-pc-border-strong hover:bg-[var(--pc-hover)] hover:text-pc-text",
              ].join(" ")}
            >
              {t(`plugins.filter.${option}`)}
            </button>
          );
        })}
      </div>

      {visible.length === 0 ? (
        <Card className="p-10 text-center">
          <Puzzle
            aria-hidden="true"
            className="mx-auto mb-3 h-10 w-10 text-pc-text-faint"
          />
          <p className="text-sm text-pc-text-muted">{t("plugins.empty")}</p>
        </Card>
      ) : (
        <div className="grid grid-cols-1 gap-3 md:grid-cols-2 xl:grid-cols-3">
          {visible.map((entry) => {
            const capabilities = catalogCapabilities(entry);
            const description = catalogDescription(entry);
            return (
              <article
                key={entry.name}
                className="flex min-w-0 flex-col gap-4 rounded-[var(--radius-lg)] border border-pc-border bg-pc-surface p-5"
              >
                <div className="space-y-2">
                  <div className="flex items-start justify-between gap-3">
                    <h2 className="min-w-0 truncate text-sm font-medium text-pc-text">
                      {entry.name}
                    </h2>
                    <div className="flex flex-shrink-0 flex-wrap justify-end gap-1.5">
                      {entry.installed && (
                        <Badge tone="neutral">{t("plugins.installed")}</Badge>
                      )}
                      {entry.available && (
                        <Badge tone="neutral">{t("plugins.registry")}</Badge>
                      )}
                    </div>
                  </div>
                  <p className="line-clamp-2 text-sm text-pc-text-muted">
                    {description ?? t("plugins.no_description")}
                  </p>
                </div>

                <dl className="space-y-1 text-xs text-pc-text-muted">
                  {entry.installed && (
                    <div className="flex justify-between gap-3">
                      <dt>{t("plugins.installed_version")}</dt>
                      <dd className="font-mono text-pc-text-secondary">
                        {entry.installed.version}
                      </dd>
                    </div>
                  )}
                  {entry.available && (
                    <div className="flex justify-between gap-3">
                      <dt>{t("plugins.registry_version")}</dt>
                      <dd className="font-mono text-pc-text-secondary">
                        {entry.available.version}
                      </dd>
                    </div>
                  )}
                </dl>

                {capabilities.length > 0 && (
                  <div className="space-y-1.5">
                    <h3 className="text-[11px] font-medium uppercase tracking-wider text-pc-text-faint">
                      {t("plugins.capabilities")}
                    </h3>
                    <div className="flex flex-wrap gap-1.5">
                      {capabilities.map((capability) => (
                        <Badge key={capability} tone="neutral">
                          {displayToken(capability)}
                        </Badge>
                      ))}
                    </div>
                  </div>
                )}

                {entry.installed && entry.installed.permissions.length > 0 && (
                  <div className="space-y-1.5">
                    <h3 className="text-[11px] font-medium uppercase tracking-wider text-pc-text-faint">
                      {t("plugins.permissions")}
                    </h3>
                    <p className="text-xs text-pc-text-muted">
                      {entry.installed.permissions.map(displayToken).join(", ")}
                    </p>
                  </div>
                )}

                {entry.available && (
                  <div className="mt-auto border-t border-pc-border pt-3 text-xs text-pc-text-muted">
                    {t("plugins.install_source")}: {" "}
                    <code className="break-all text-pc-text-secondary">
                      {entry.available.install_source}
                    </code>
                  </div>
                )}
              </article>
            );
          })}
        </div>
      )}
    </div>
  );
}
