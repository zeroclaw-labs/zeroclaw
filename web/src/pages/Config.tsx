// Schema-driven config editor (#6175). Curated section explorer
// but lands on a per-section overview: pick a section in the sidebar, see
// what's currently configured under it, click an item to edit, click +Add
// to instantiate a new entry.
//
// URL structure:
//   /config/:section             — section overview (configured items list)
//   /config/:section/:type       — alias list for a provider/channel type
//   /config/:section/:type/:alias — field form for a specific alias
//
// All section list / picker / field rendering comes from the shared
// SectionPicker + FieldForm components. NO hardcoded section names, field
// labels, dropdown options, or provider lists.

import { useEffect, useMemo, useState } from "react";
import { Link, useLocation, useNavigate, useParams } from "react-router-dom";
import { ArrowLeft, ChevronRight, Plus, Sparkles, Trash2 } from "lucide-react";
import {
  ApiError,
  deleteMapKey,
  getDrift,
  getMapKeys,
  getSections,
  listProps,
  patchConfig,
  selectSectionItem,
  type DriftEntry,
  type ListResponseEntry,
  type PickerItem,
  type SectionInfo,
} from "../lib/api";
import FieldForm, {
  clearFieldFormCatalogCaches,
} from "../components/sections/FieldForm";
import PersonalityEditor from "../components/sections/PersonalityEditor";
import SkillsBundleEditor from "../components/sections/SkillsBundleEditor";
import ReloadDaemonButton from "../components/sections/ReloadDaemonButton";
import SectionPicker from "../components/sections/SectionPicker";
import SectionTabs, {
  type SectionTabSpec,
} from "../components/sections/SectionTabs";
import CostRatesEditor, {
  type CostRatesCategory,
} from "../components/sections/CostRatesEditor";
import { Badge, Button, Card, PageHeader } from "@/components/ui";
import { t } from "@/lib/i18n";

// Display order for the curated sidebar groups. Each `SectionInfo.group`
// from the gateway lands in one of these buckets (anything else falls
// into "Other"). Schema-attribute-driven grouping replaces this in v3 /
// #5947.
//
// Foundation leads — Workspace / Providers / Channels / Memory /
// Hardware / Tunnel are the most-edited sections, surfaced first inside
// the Config explorer instead of as duplicate top-level nav entries.
// The Quickstart flow walks the same six (reachable via the
// "Run setup again" link in the breadcrumb row).
const GROUP_ORDER = [
  "Foundation",
  "Agent",
  "Multi-agent",
  "Tools",
  "Integrations",
  "Network",
  "Storage",
  "Operations",
  "Other",
] as const;

// Foundation order is gateway-provided: the server returns sections
// pre-ordered by `zeroclaw_config::sections::QUICKSTART_SECTIONS`
// (single canonical source). The dashboard preserves response order for
// the Foundation group instead of carrying its own copy of the list.

export default function Config() {
  // URL params drive the view. No internal mode state for picker/form —
  // the address bar is the source of truth.
  //   :section              → section overview
  //   :section/:type        → alias list (providers/channels) or picker (others)
  //   :section/:type/:alias → field form
  const {
    section: sectionParam,
    type: typeParam,
    alias: aliasParam,
  } = useParams<{ section?: string; type?: string; alias?: string }>();
  const location = useLocation();
  const navigate = useNavigate();
  const lockedSection = location.pathname.startsWith("/setup/")
    ? sectionParam
    : undefined;
  const [sections, setSections] = useState<SectionInfo[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [drifted, setDrifted] = useState<DriftEntry[]>([]);
  const fetchDrift = () => {
    void getDrift()
      .then((r) => setDrifted(r.drifted ?? []))
      .catch(() => undefined);
  };
  useEffect(fetchDrift, [activeKey]);

  const [reloadKey, setReloadKey] = useState(0);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getSections()
      .then((resp) => {
        if (cancelled) return;
        setSections(resp.sections);
        const initialKey =
          sectionParam && resp.sections.find((s) => s.key === sectionParam)
            ? sectionParam
            : (resp.sections[0]?.key ?? null);
        setActiveKey(initialKey);
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof ApiError) {
          setError(`[${e.envelope.code}] ${e.envelope.message}`);
        } else {
          setError(
            `Couldn't load sections: ${e instanceof Error ? e.message : String(e)}`,
          );
        }
      })
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!sectionParam || sections.length === 0) return;
    if (
      sections.some((s) => s.key === sectionParam) &&
      sectionParam !== activeKey
    ) {
      setActiveKey(sectionParam);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sectionParam, sections]);

  // Bust FieldForm's per-provider model catalog cache on section change so a
  // model alias just added under e.g. `providers.models.anthropic` shows up
  // the next time the user opens an agent form, without a hard refresh.
  useEffect(() => {
    clearFieldFormCatalogCaches();
  }, [sectionParam, typeParam, aliasParam]);

  const activeSection = useMemo(
    () => sections.find((s) => s.key === activeKey) ?? null,
    [sections, activeKey],
  );

  const goToSection = (key: string) => {
    setActiveKey(key);
    if (!lockedSection) {
      navigate(`/config/${encodeURIComponent(key)}`);
    }
  };

  // Navigate to alias list for a provider/channel type.
  const goToType = (sectionKey: string, typeKey: string) => {
    navigate(
      `/config/${encodeURIComponent(sectionKey)}/${encodeURIComponent(typeKey)}`,
    );
  };

  // Navigate to the form for a specific alias. Calls selectSectionItem
  // to instantiate the entry if needed, then navigates to the alias URL.
  const goToAlias = async (
    sectionKey: string,
    typeKey: string,
    alias: string,
  ) => {
    try {
      await selectSectionItem(sectionKey, typeKey, alias);
      navigate(
        `/config/${encodeURIComponent(sectionKey)}/${encodeURIComponent(typeKey)}/${encodeURIComponent(alias)}`,
      );
    } catch (e) {
      if (e instanceof ApiError) {
        setError(`[${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setError(e instanceof Error ? e.message : String(e));
      }
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{
            borderColor: "var(--pc-border)",
            borderTopColor: "var(--pc-accent)",
          }}
        />
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-6">
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-4 text-sm text-status-error">
          {error}
        </div>
      </div>
    );
  }

  // Determine what to render in the main pane based on URL params.
  // Two-tier alias sections route /config/<section>/<type>/<alias>.
  // Server-emitted shape (from `WizardSection::shape()` in the Rust
  // config crate) decides whether this section needs a type→alias picker
  // or a flat alias list — no hardcoded section keys on the client.
  const needsAliasTier = activeSection?.shape === "typed_family_map";
  const isOneTierAliasSection = activeSection?.shape === "one_tier_alias_map";

  const mainContent = (() => {
    if (!activeSection) return null;

    if (!activeSection.has_picker) {
      return (
        <WireTabForm
          key={`${reloadKey}-${activeSection.key}`}
          prefix={activeSection.key}
          title={activeSection.label}
          reloadKey={reloadKey}
          onSaved={fetchDrift}
          drift={drifted}
        />
      );
    }

    // /config/:section/:type/:alias — field form
    if (typeParam && aliasParam) {
      const fieldsPrefix = needsAliasTier
        ? activeSection.key === "channels"
          ? `channels.${typeParam}.${aliasParam}`
          : `${activeSection.key}.${typeParam}.${aliasParam}`
        : typeParam;
      return (
        <div className="flex flex-col gap-3 flex-1 min-h-0">
          <Button
            variant="ghost"
            size="sm"
            onClick={() => navigate(-1)}
            className="self-start"
          >
            <ArrowLeft className="h-4 w-4" />
            Back
          </Button>
          <WireTabForm
            key={`${reloadKey}-${fieldsPrefix}`}
            prefix={fieldsPrefix}
            title={`${typeParam} / ${aliasParam}`}
            reloadKey={reloadKey}
            onSaved={fetchDrift}
            drift={drifted}
          />
        </div>
      );
    }

    // /config/:section/:alias — one-tier alias section field form
    // (agents). The URL's :type slot carries the alias directly.
    if (typeParam && isOneTierAliasSection) {
      const fieldsPrefix = `${activeSection.key}.${typeParam}`;
      const isAgent = activeSection.key === "agents";
      const isSkillBundle = activeSection.key === "skill_bundles";

      // Composite tabs that sit alongside the wire-driven field tabs.
      const extraTabs: SectionTabSpec[] = [];
      if (isAgent) {
        extraTabs.push(
          {
            key: "peer_groups",
            label: "Peer Groups",
            render: () => (
              <AgentPeerGroupsTab
                key={`${reloadKey}-${typeParam}-peer_groups`}
                agentAlias={typeParam}
                onSaved={fetchDrift}
              />
            ),
          },
          {
            key: "personality",
            label: "Personality",
            render: () => (
              <PersonalityEditor
                key={`${reloadKey}-${typeParam}-personality`}
                agent={typeParam}
              />
            ),
          },
        );
      } else if (isSkillBundle) {
        extraTabs.push({
          key: "skills",
          label: "Skills",
          render: () => (
            <SkillsBundleEditor
              key={`${reloadKey}-${typeParam}-skills`}
              bundle={typeParam}
            />
          ),
        });
      }

      return (
        <div className="flex flex-col gap-3 flex-1 min-h-0">
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              navigate(`/config/${encodeURIComponent(activeSection.key)}`)
            }
            className="self-start"
          >
            <ArrowLeft className="h-4 w-4" />
            Back to {activeSection.label}
          </Button>
          <WireTabForm
            key={`${reloadKey}-${fieldsPrefix}`}
            prefix={fieldsPrefix}
            title={typeParam}
            reloadKey={reloadKey}
            onSaved={fetchDrift}
            drift={drifted}
            extraTabs={extraTabs.length > 0 ? extraTabs : undefined}
          />
        </div>
      );
    }

    // /config/:section/:type — alias list (providers/channels) or direct form
    if (typeParam && needsAliasTier) {
      const aliasListPane = (
        <AliasListView
          sectionKey={activeSection.key}
          typeKey={typeParam}
          sectionHelp={activeSection.help}
          onSelectAlias={async (alias) => {
            await selectSectionItem(activeSection.key, typeParam, alias);
            navigate(
              `/config/${encodeURIComponent(activeSection.key)}/${encodeURIComponent(typeParam)}/${encodeURIComponent(alias)}`,
            );
          }}
          onBack={() =>
            navigate(`/config/${encodeURIComponent(activeSection.key)}`)
          }
        />
      );
      const costsCategory = costCategoryForSection(activeSection.key);
      if (costsCategory) {
        return (
          <SectionTabs
            tabs={[
              { key: "aliases", label: "Aliases", render: () => aliasListPane },
              {
                key: "costs",
                label: "Costs",
                render: () => (
                  <CostRatesEditor
                    category={costsCategory}
                    providerType={typeParam}
                    onSaved={fetchDrift}
                  />
                ),
              },
            ]}
          />
        );
      }
      return aliasListPane;
    }

    // /config/:section — section overview (configured items) + picker
    if (typeParam) {
      // Non-alias-tiered section with a type in the URL: treat as form
      return (
        <div className="flex flex-col gap-3 flex-1 min-h-0">
          <Button
            variant="ghost"
            size="sm"
            onClick={() =>
              navigate(`/config/${encodeURIComponent(activeSection.key)}`)
            }
            className="self-start"
          >
            <ArrowLeft className="h-4 w-4" />
            Back to {activeSection.label}
          </Button>
          <FieldForm
            key={`${reloadKey}-${typeParam}`}
            prefix={typeParam}
            title={typeParam}
            onSaved={fetchDrift}
            drift={drifted}
          />
        </div>
      );
    }

    // /config/agents (or any one-tier alias section) — direct alias list with
    // inline + Add affordance. Mirrors the two-tier AliasListView pattern but
    // skips the type-selection step since the section IS the type.
    if (isOneTierAliasSection) {
      return (
        <AliasListView
          sectionKey={activeSection.key}
          sectionHelp={activeSection.help}
          onSelectAlias={async (alias) => {
            await selectSectionItem(activeSection.key, alias);
            navigate(
              `/config/${encodeURIComponent(activeSection.key)}/${encodeURIComponent(alias)}`,
            );
          }}
          onBack={() => navigate("/config")}
        />
      );
    }

    // /config/:section — overview + picker
    return (
      <SectionOverview
        section={activeSection}
        onPickType={(typeKey) => {
          if (needsAliasTier) {
            goToType(activeSection.key, typeKey);
          } else {
            void (async () => {
              try {
                const resp = await selectSectionItem(
                  activeSection.key,
                  typeKey,
                );
                // BackendPicker sections (Memory, Tunnel) collapse the
                // pick into a single field on the section root
                // (memory.backend, tunnel.tunnel-provider). The form
                // renders against the section's own prefix, so the URL
                // is `/config/<section>` with no trailing type segment.
                // Two-tier paths (providers/channels) still navigate
                // through the type slot because their alias forms live
                // under `<section>.<type>.<alias>`.
                const target = resp.fields_prefix.includes(".")
                  ? `/config/${resp.fields_prefix.split(".").map(encodeURIComponent).join("/")}`
                  : `/config/${encodeURIComponent(resp.fields_prefix)}`;
                navigate(target, {
                  state: { fieldsPrefix: resp.fields_prefix },
                });
              } catch (e) {
                setError(e instanceof Error ? e.message : String(e));
              }
            })();
          }
        }}
        onPickAlias={(typeKey, alias) =>
          void goToAlias(activeSection.key, typeKey, alias)
        }
        sectionUrl={`/config/${encodeURIComponent(activeSection.key)}`}
        reloadKey={reloadKey}
        fetchDrift={fetchDrift}
        drifted={drifted}
      />
    );
  })();

  // Breadcrumb segments
  const crumbs: Array<{ label: string; url?: string }> = [
    { label: "Config", url: "/config" },
    {
      label: activeSection?.label ?? "",
      url: activeSection
        ? `/config/${encodeURIComponent(activeSection.key)}`
        : undefined,
    },
  ];
  if (typeParam)
    crumbs.push({
      label: typeParam,
      url:
        typeParam && aliasParam
          ? `/config/${encodeURIComponent(sectionParam ?? "")}/${encodeURIComponent(typeParam)}`
          : undefined,
    });
  if (aliasParam) crumbs.push({ label: aliasParam });

  return (
    <div className="flex h-full overflow-hidden">
      {!lockedSection && (
        // Secondary master pane. Deliberately lighter than the global app
        // sidebar: no surface fill, faint uppercase group headings, and a
        // subtle accent-tinted active row (not a heavy fill) so it reads as
        // a section list inside Config — not a second top-level nav.
        <aside className="w-52 flex-shrink-0 border-r border-pc-border overflow-y-auto py-2">
          <nav className="flex flex-col">
            {GROUP_ORDER.map((groupName) => {
              const known = new Set(GROUP_ORDER);
              const items = sections
                .filter((s) =>
                  groupName === "Other"
                    ? s.group === "Other" ||
                      !known.has(s.group as (typeof GROUP_ORDER)[number])
                    : s.group === groupName,
                )
                .sort((a, b) => {
                  // Foundation: preserve server-provided canonical order
                  // (driven by `QUICKSTART_SECTIONS` in the Rust config
                  // crate). Other groups: alphabetize by label.
                  if (groupName === "Foundation") {
                    return sections.indexOf(a) - sections.indexOf(b);
                  }
                  return a.label.localeCompare(b.label);
                });
              if (items.length === 0) return null;
              return (
                <div key={groupName} className="mb-1">
                  <div className="px-3 pt-3 pb-1 text-[10px] font-medium uppercase tracking-wider text-pc-text-faint">
                    {groupName}
                  </div>
                  {items.map((s) => {
                    const active = s.key === activeKey;
                    return (
                      <button
                        key={s.key}
                        type="button"
                        onClick={() => goToSection(s.key)}
                        aria-current={active ? "page" : undefined}
                        className={[
                          "mx-1.5 flex items-center justify-between gap-2 rounded-[var(--radius-sm)]",
                          "px-2.5 py-1.5 text-sm text-left transition-colors",
                          active
                            ? "bg-pc-accent/10 text-pc-accent font-medium"
                            : "text-pc-text-secondary hover:bg-pc-elevated/60 hover:text-pc-text",
                        ].join(" ")}
                      >
                        <span className="truncate">{s.label}</span>
                        {active && (
                          <ChevronRight className="h-3.5 w-3.5 flex-shrink-0" />
                        )}
                      </button>
                    );
                  })}
                </div>
              );
            })}
          </nav>
        </aside>
      )}

      <main className="flex-1 overflow-y-auto p-6">
        {activeSection && (
          <div className="flex flex-col gap-4 max-w-3xl min-h-full">
            {/* Layout note: every wrapper between <main> (the scroll
                container) and FieldForm's save bar uses flex-1 + min-h-0
                so the form stretches to the viewport bottom. Without
                that chain, the save bar's `sticky bottom-0` anchors
                to a content-height column and floats mid-viewport
                instead of pinning to the bottom of the scroll area. */}
            {/* Config header: section title + breadcrumb trail (as the
                description slot) + the page-level actions. ReloadDaemonButton
                keeps its own confirm modal — only the surrounding chrome is
                restyled. */}
            <PageHeader
              title={activeSection.label}
              description={
                <span className="flex items-center gap-1.5 flex-wrap text-pc-text-muted">
                  {crumbs.map((crumb, i) => (
                    <span key={i} className="flex items-center gap-1.5">
                      {i > 0 && (
                        <ChevronRight className="h-3 w-3 text-pc-text-faint" />
                      )}
                      {crumb.url && i < crumbs.length - 1 ? (
                        <button
                          type="button"
                          onClick={() => navigate(crumb.url!)}
                          className="text-pc-text-secondary hover:text-pc-text transition-colors"
                        >
                          {crumb.label}
                        </button>
                      ) : (
                        <span className="text-pc-text font-medium">
                          {crumb.label}
                        </span>
                      )}
                    </span>
                  ))}
                </span>
              }
              actions={
                <>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => navigate("/quickstart")}
                    title={t("cfg.header.quickstart")}
                  >
                    <Sparkles className="h-3.5 w-3.5" />
                    {t("cfg.header.quickstart")}
                  </Button>
                  <ReloadDaemonButton
                    onReloaded={() => {
                      goToSection(activeSection.key);
                      fetchDrift();
                      setReloadKey((n) => n + 1);
                    }}
                  />
                </>
              }
            />

            <div className="flex-1 min-h-0 flex flex-col">{mainContent}</div>
          </div>
        )}
      </main>
    </div>
  );
}

// Alias list page: /config/:section/:type
// Shows existing aliases as clickable rows + an inline "new alias" input.
/// Help block shown above every alias-input field. Mirrors the wizard's
/// `AliasHelpBox` text — keep both in sync if the validator's rules
/// (`zeroclaw_config::helpers::validate_alias_key`) ever change.
function ConfigAliasHelpBox() {
  return (
    <div
      className="rounded-[var(--radius-md)] border border-pc-border px-3 py-2 text-xs text-pc-text-secondary"
      style={{ background: "var(--pc-bg-surface-subtle)" }}
    >
      <p className="mb-1">
        <strong>Alias.</strong> A short stable name you’ll use everywhere else
        in config to point at this entry (agents, routes, and per-channel
        bindings reference it as{" "}
        <code>
          {"<type>"}.{"<alias>"}
        </code>
        ). Aliases let you have several entries of the same type — a{" "}
        <code>work</code> credential and a <code>personal</code> one, for
        example.
      </p>
      <p className="mb-0">
        Rules: lowercase letters, digits, single underscores; 1–63 chars; no
        leading/trailing/double underscores, no dots, hyphens, or spaces.{" "}
        <strong>Aliases can’t be renamed in v0.8.0</strong> — pick something
        you’ll keep, or delete and recreate.
      </p>
    </div>
  );
}

function suggestConfigAlias(aliases: string[]): string {
  const used = new Set(aliases);
  if (!used.has("default")) return "default";
  for (let i = 2; i < 100; i += 1) {
    const candidate = `default_${i}`;
    if (!used.has(candidate)) return candidate;
  }
  return "default_100";
}

function validateConfigAlias(alias: string): string | null {
  if (/^(?!_)(?!.*__)(?!.*_$)[a-z0-9_]{1,63}$/.test(alias)) return null;
  return "Alias must use lowercase letters, digits, or single underscores only; no hyphens, dots, spaces, leading/trailing underscores, or double underscores.";
}

function AliasListView({
  sectionKey,
  typeKey,
  sectionHelp,
  onSelectAlias,
  onBack,
}: {
  sectionKey: string;
  /** Channel/provider type for two-tier sections; omitted for one-tier
   *  alias sections like agents that have no `<type>` segment. */
  typeKey?: string;
  /** Section's help blurb from the gateway. Renders above the
   *  generic alias-name help so operators see what the section is
   *  before being asked to name an entry inside it. */
  sectionHelp?: string;
  onSelectAlias: (alias: string) => Promise<void>;
  onBack: () => void;
}) {
  const [aliases, setAliases] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [newAlias, setNewAlias] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [aliasError, setAliasError] = useState<string | null>(null);

  // Two-tier sections (providers, channels) put the type in the path;
  // one-tier sections (agents, risk_profiles, etc.) just use the section
  // key as-is. The map-keys endpoint then returns the alias names directly.
  const mapPath = typeKey ? `${sectionKey}.${typeKey}` : sectionKey;

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getMapKeys(mapPath)
      .then((r) => {
        if (!cancelled) setAliases(r.keys);
      })
      .catch((e) => {
        if (!cancelled) {
          setAliases([]);
          setError(e instanceof Error ? e.message : String(e));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [mapPath]);

  const submit = async () => {
    const trimmed = newAlias.trim() || suggestConfigAlias(aliases);
    setAliasError(null);
    const validationError = validateConfigAlias(trimmed);
    if (validationError) {
      setAliasError(validationError);
      return;
    }
    try {
      await onSelectAlias(trimmed);
    } catch (e) {
      setAliasError(
        e instanceof ApiError
          ? e.envelope.message
          : e instanceof Error
            ? e.message
            : String(e),
      );
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <Button
        variant="ghost"
        size="sm"
        onClick={onBack}
        className="self-start"
      >
        <ArrowLeft className="h-4 w-4" />
        Back
      </Button>

      {sectionHelp && (
        <p className="text-sm leading-relaxed text-pc-text-secondary">
          {sectionHelp}
        </p>
      )}

      <ConfigAliasHelpBox />

      {error && (
        <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-3 text-sm text-status-error">
          {error}
        </div>
      )}

      {loading ? (
        <div className="flex items-center justify-center py-12">
          <div
            className="h-8 w-8 border-2 rounded-full animate-spin"
            style={{
              borderColor: "var(--pc-border)",
              borderTopColor: "var(--pc-accent)",
            }}
          />
        </div>
      ) : (
        <Card padded={false} className="divide-y divide-pc-border overflow-hidden">
          {aliases.map((alias) => (
            <AliasRow
              key={alias}
              alias={alias}
              mapPath={mapPath}
              onSelect={() =>
                onSelectAlias(alias).catch((e) => {
                  setError(
                    e instanceof ApiError
                      ? `[${e.envelope.code}] ${e.envelope.message}`
                      : e instanceof Error
                        ? e.message
                        : String(e),
                  );
                })
              }
              onDeleted={() => {
                setAliases((prev) => prev.filter((a) => a !== alias));
              }}
              onDeleteError={(msg) => setError(msg)}
            />
          ))}

          {/* Inline new alias row */}
          <div className="flex flex-col gap-1 px-4 py-3">
            <div className="flex items-center gap-2">
              <input
                type="text"
                className="input-electric flex-1 px-3 py-1.5 text-sm"
                placeholder={suggestConfigAlias(aliases)}
                value={newAlias}
                onChange={(e) => {
                  setNewAlias(e.target.value);
                  setAliasError(null);
                }}
                onKeyDown={(e) => {
                  if (e.key === "Enter") void submit();
                }}
              />
              <Button
                variant="primary"
                size="sm"
                onClick={() => void submit()}
                className="flex-shrink-0"
              >
                Add
              </Button>
            </div>
            {aliasError && (
              <p
                className="text-xs"
                style={{ color: "var(--color-status-error)" }}
              >
                {aliasError}
              </p>
            )}
          </div>
        </Card>
      )}
    </div>
  );
}

// BackendPicker sections have a discriminator field that the top picker
// sets; the settings form below excludes it to avoid the duplicate input.
const BACKEND_PICKER_FIELD: Record<string, string> = {
  tunnel: "tunnel.tunnel-provider",
  memory: "memory.backend",
};

function costCategoryForSection(sectionKey: string): CostRatesCategory | null {
  if (sectionKey === "providers.models") return "models";
  if (sectionKey === "providers.tts") return "tts";
  if (sectionKey === "providers.transcription") return "transcription";
  return null;
}

/**
 * Build `SectionTabSpec[]` from the `tab` field on wire entries.
 *
 * Each distinct non-empty `tab` value becomes one tab whose `FieldForm`
 * filters via `includePath` on the set of paths belonging to that tab.
 * Tab order follows first-occurrence in the entries array (which matches
 * field-declaration order from the Rust schema). Returns `null` when no
 * entries carry a `tab` value (flat display, no tab bar).
 */
function wireTabSpecs(
  entries: ListResponseEntry[],
  prefix: string,
  ctx: {
    reloadKey: number;
    title: string;
    onSaved: () => void;
    drifted: DriftEntry[];
  },
): SectionTabSpec[] | null {
  // Group paths by tab, preserving first-occurrence order.
  const tabOrder: string[] = [];
  const tabPaths = new Map<string, Set<string>>();
  for (const e of entries) {
    const t = e.tab;
    if (!t) continue;
    if (!tabPaths.has(t)) {
      tabOrder.push(t);
      tabPaths.set(t, new Set());
    }
    tabPaths.get(t)!.add(e.path);
  }
  if (tabOrder.length === 0) return null;

  return tabOrder.map((tab) => {
    const paths = tabPaths.get(tab)!;
    return {
      key: tab.toLowerCase().replace(/\s+/g, "-"),
      label: tab,
      render: () => (
        <FieldForm
          key={`${ctx.reloadKey}-${prefix}-${tab}`}
          prefix={prefix}
          title={ctx.title}
          onSaved={ctx.onSaved}
          drift={ctx.drifted}
          includePath={(p) => paths.has(p)}
        />
      ),
    };
  });
}

/**
 * Self-contained component: fetches entries for `prefix`, groups by `tab`,
 * and renders a `SectionTabs` when tabs are present or a plain `FieldForm`
 * when they aren't. Extra tabs (e.g. Personality, PeerGroups) can be
 * appended via `extraTabs`.
 */
function WireTabForm({
  prefix,
  title,
  reloadKey,
  onSaved,
  drift,
  extraTabs,
}: {
  prefix: string;
  title: string;
  reloadKey: number;
  onSaved: () => void;
  drift: DriftEntry[];
  extraTabs?: SectionTabSpec[];
}) {
  const [entries, setEntries] = useState<ListResponseEntry[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    void listProps(prefix).then((resp) => {
      if (!cancelled) setEntries(resp.entries);
    });
    return () => {
      cancelled = true;
    };
  }, [prefix, reloadKey]);

  if (!entries) return null; // loading

  const ctx = { reloadKey, title, onSaved, drifted: drift };
  const tabs = wireTabSpecs(entries, prefix, ctx);

  if (tabs || extraTabs) {
    const all = [...(tabs ?? []), ...(extraTabs ?? [])];
    return <SectionTabs tabs={all} />;
  }

  return (
    <FieldForm
      key={reloadKey}
      prefix={prefix}
      title={title}
      onSaved={onSaved}
      drift={drift}
    />
  );
}

/**
 * Peer Groups tab on the agent edit page. Walks `peer_groups.*` for
 * groups containing the bound agent, then embeds the SAME FieldForm
 * used at `/config/peer_groups/<alias>` — no duplicated authoring
 * surface. Plus an "Add to group" picker that appends this agent to a
 * group's `agents` array via patchConfig.
 */
function AgentPeerGroupsTab({
  agentAlias,
  onSaved,
}: {
  agentAlias: string;
  onSaved: () => void;
}) {
  const [memberOf, setMemberOf] = useState<string[]>([]);
  const [nonMembers, setNonMembers] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [pickerValue, setPickerValue] = useState("");

  const reload = async () => {
    setLoading(true);
    setError(null);
    try {
      const { keys } = await getMapKeys("peer_groups");
      const memberships: string[] = [];
      const others: string[] = [];
      for (const pg of keys) {
        const { entries } = await listProps(`peer_groups.${pg}`);
        const agentsEntry = entries.find(
          (e) => e.path === `peer_groups.${pg}.agents`,
        );
        const list = parseAgentsList(agentsEntry?.value);
        if (list.includes(agentAlias)) memberships.push(pg);
        else others.push(pg);
      }
      setMemberOf(memberships);
      setNonMembers(others);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  };

  useEffect(() => {
    void reload();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [agentAlias]);

  const addToGroup = async () => {
    if (!pickerValue) return;
    setAdding(true);
    setError(null);
    try {
      const { entries } = await listProps(`peer_groups.${pickerValue}`);
      const agentsEntry = entries.find(
        (e) => e.path === `peer_groups.${pickerValue}.agents`,
      );
      const list = parseAgentsList(agentsEntry?.value);
      if (!list.includes(agentAlias)) {
        const next = [...list, agentAlias];
        await patchConfig([
          {
            op: "replace",
            path: `peer_groups.${pickerValue}.agents`,
            value: next,
          },
        ]);
      }
      setPickerValue("");
      await reload();
      onSaved();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setAdding(false);
    }
  };

  const removeFromGroup = async (pg: string) => {
    setError(null);
    try {
      const { entries } = await listProps(`peer_groups.${pg}`);
      const agentsEntry = entries.find(
        (e) => e.path === `peer_groups.${pg}.agents`,
      );
      const list = parseAgentsList(agentsEntry?.value).filter(
        (a) => a !== agentAlias,
      );
      await patchConfig([
        { op: "replace", path: `peer_groups.${pg}.agents`, value: list },
      ]);
      await reload();
      onSaved();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  };

  if (loading) {
    return (
      <p className="text-sm" style={{ color: "var(--pc-text-muted)" }}>
        Loading peer groups…
      </p>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      {error && (
        <div
          className="rounded-xl border p-3 text-sm"
          style={{
            background: "var(--color-status-error-alpha-08)",
            borderColor: "var(--color-status-error-alpha-20)",
            color: "var(--color-status-error)",
          }}
        >
          {error}
        </div>
      )}

      <div
        className="flex items-center gap-2 rounded-xl p-3"
        style={{ background: "var(--pc-bg-elevated)" }}
      >
        <span className="text-xs" style={{ color: "var(--pc-text-muted)" }}>
          Add this agent to:
        </span>
        <select
          value={pickerValue}
          onChange={(e) => setPickerValue(e.target.value)}
          disabled={adding || nonMembers.length === 0}
          className="input-electric text-xs px-2 py-1 appearance-none cursor-pointer"
        >
          <option value="">
            {nonMembers.length === 0 ? "no other groups" : "select a group…"}
          </option>
          {nonMembers.map((g) => (
            <option key={g} value={g}>
              {g}
            </option>
          ))}
        </select>
        <button
          type="button"
          onClick={addToGroup}
          disabled={!pickerValue || adding}
          className="btn-electric text-xs px-3 py-1 rounded-lg disabled:opacity-50"
        >
          {adding ? "Adding…" : "Add"}
        </button>
        <Link
          to="/config/peer_groups"
          className="text-xs ml-auto hover:underline"
          style={{ color: "var(--pc-text-muted)" }}
        >
          Create new →
        </Link>
      </div>

      {memberOf.length === 0 ? (
        <p
          className="text-sm rounded-xl p-4 text-center"
          style={{
            color: "var(--pc-text-muted)",
            background: "var(--pc-bg-elevated)",
          }}
        >
          {agentAlias} is not a member of any peer group.
        </p>
      ) : (
        memberOf.map((pg) => (
          <div
            key={pg}
            className="rounded-xl border"
            style={{ borderColor: "var(--pc-border)" }}
          >
            <div
              className="flex items-center justify-between px-4 py-2 border-b"
              style={{ borderColor: "var(--pc-border)" }}
            >
              <Link
                to={`/config/peer_groups/${encodeURIComponent(pg)}`}
                className="text-sm font-mono hover:underline"
                style={{ color: "var(--pc-text-primary)" }}
              >
                peer_groups.{pg}
              </Link>
              <button
                type="button"
                onClick={() => removeFromGroup(pg)}
                className="text-xs hover:underline"
                style={{ color: "var(--color-status-error)" }}
                title={`Remove ${agentAlias} from peer_groups.${pg}`}
              >
                Remove from group
              </button>
            </div>
            <div className="p-4">
              <FieldForm
                key={`peer_groups-embed-${pg}`}
                prefix={`peer_groups.${pg}`}
                onSaved={onSaved}
                showDelete={false}
              />
            </div>
          </div>
        ))
      )}
    </div>
  );
}

function parseAgentsList(raw: unknown): string[] {
  if (Array.isArray(raw)) return raw.map(String);
  if (typeof raw !== "string" || raw.length === 0) return [];
  try {
    const parsed = JSON.parse(raw);
    if (Array.isArray(parsed)) return parsed.map(String);
  } catch {
    // fall through
  }
  return raw
    .replace(/^\[|\]$/g, "")
    .split(/[,\n]/)
    .map((s) => s.trim().replace(/^"|"$/g, ""))
    .filter(Boolean);
}

function AliasRow({
  alias,
  mapPath,
  onSelect,
  onDeleted,
  onDeleteError,
}: {
  alias: string;
  mapPath: string;
  onSelect: () => void;
  onDeleted: () => void;
  onDeleteError: (msg: string) => void;
}) {
  // Two-stage confirm avoids accidental deletes without a modal: first
  // click arms the trash (3-second window), second click commits.
  const [armed, setArmed] = useState(false);
  const [deleting, setDeleting] = useState(false);

  useEffect(() => {
    if (!armed) return;
    const timer = setTimeout(() => setArmed(false), 3000);
    return () => clearTimeout(timer);
  }, [armed]);

  const onTrashClick = (e: React.MouseEvent) => {
    e.stopPropagation();
    if (!armed) {
      setArmed(true);
      return;
    }
    setDeleting(true);
    deleteMapKey(mapPath, alias)
      .then(() => {
        onDeleted();
      })
      .catch((err) => {
        onDeleteError(
          err instanceof ApiError
            ? `[${err.envelope.code}] ${err.envelope.message}`
            : err instanceof Error
              ? err.message
              : String(err),
        );
      })
      .finally(() => {
        setDeleting(false);
        setArmed(false);
      });
  };

  return (
    <div className="w-full flex items-center justify-between gap-3 px-4 py-3 text-sm transition-colors hover:bg-pc-elevated/50">
      <button
        type="button"
        onClick={onSelect}
        className="flex-1 min-w-0 flex items-center justify-between gap-3 text-left"
      >
        <div className="min-w-0">
          <span className="font-medium text-pc-text">{alias}</span>
          <code className="block text-xs mt-0.5 text-pc-text-faint">
            {mapPath}.{alias}
          </code>
        </div>
        <ChevronRight className="h-4 w-4 flex-shrink-0 text-pc-text-muted" />
      </button>
      <button
        type="button"
        onClick={onTrashClick}
        disabled={deleting}
        title={armed ? "Click again to confirm delete" : "Delete this alias"}
        className={[
          "btn-icon flex-shrink-0",
          armed ? "text-status-error border-status-error/40" : "",
        ].join(" ")}
      >
        {armed ? (
          <span className="text-xs px-1">Confirm</span>
        ) : (
          <Trash2 className="h-4 w-4" />
        )}
      </button>
    </div>
  );
}

interface SectionOverviewProps {
  section: SectionInfo;
  onPickType: (typeKey: string) => void;
  onPickAlias: (typeKey: string, alias: string) => void;
  sectionUrl: string;
  reloadKey: number;
  fetchDrift: () => void;
  drifted: DriftEntry[];
}

function SectionOverview({
  section,
  onPickType,
  onPickAlias,
  sectionUrl,
}: SectionOverviewProps) {
  const [showPicker, setShowPicker] = useState(false);

  // BackendPicker sections (Memory, Tunnel) pick ONE backend; +Add
  // and the "configured items" list don't fit single-choice semantics.
  // Render the picker plus the section's own fields (memory.auto_save,
  // hygiene, etc.) inline.
  const isBackendPicker = section.shape === "backend_picker";
  if (isBackendPicker) {
    // The discriminator field is the picker; rendering it again in the
    // settings form below is a duplicate input that confuses users.
    const pickerPath = BACKEND_PICKER_FIELD[section.key];
    const excludePicker = pickerPath
      ? (path: string) => path !== pickerPath
      : undefined;
    return (
      <div className="flex flex-col gap-4">
        <SectionPicker
          sectionKey={section.key}
          help={section.help}
          onPick={(item) => onPickType(item.key)}
        />
        <FieldForm
          key={`${section.key}-fields`}
          prefix={section.key}
          title={`${section.label} settings`}
          includePath={excludePicker}
        />
      </div>
    );
  }

  if (showPicker) {
    return (
      <div className="flex flex-col gap-3">
        <Button
          variant="ghost"
          size="sm"
          onClick={() => setShowPicker(false)}
          className="self-start"
        >
          <ArrowLeft className="h-4 w-4" />
          Back to {section.label}
        </Button>
        <SectionPicker
          sectionKey={section.key}
          help={section.help}
          onPick={(item) => {
            setShowPicker(false);
            onPickType(item.key);
          }}
          onSkip={() => setShowPicker(false)}
        />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-4">
      <div className="flex items-center justify-between gap-3">
        <p className="text-sm text-pc-text-secondary">{section.help}</p>
        <Button
          variant="primary"
          size="md"
          onClick={() => setShowPicker(true)}
          className="flex-shrink-0"
        >
          <Plus className="h-4 w-4" />
          Add
        </Button>
      </div>
      <ConfiguredOnlyPicker
        section={section}
        onPickType={onPickType}
        onPickAlias={onPickAlias}
        sectionUrl={sectionUrl}
      />
    </div>
  );
}

interface ConfiguredOnlyPickerProps {
  section: SectionInfo;
  onPickType: (typeKey: string) => void;
  onPickAlias: (typeKey: string, alias: string) => void;
  sectionUrl: string;
}

function ConfiguredOnlyPicker({
  section,
  onPickType,
}: ConfiguredOnlyPickerProps) {
  const [items, setItems] = useState<PickerItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    import("../lib/api").then(({ getSectionPicker }) =>
      getSectionPicker(section.key)
        .then((resp) => {
          if (cancelled) return;
          setItems(
            resp.items.filter(
              (i) => i.badge === "configured" || i.badge === "active",
            ),
          );
        })
        .catch((e) => {
          if (cancelled) return;
          if (e instanceof ApiError) {
            setError(`[${e.envelope.code}] ${e.envelope.message}`);
          } else {
            setError(
              `Couldn't load configured items: ${e instanceof Error ? e.message : String(e)}`,
            );
          }
        })
        .finally(() => !cancelled && setLoading(false)),
    );
    return () => {
      cancelled = true;
    };
  }, [section.key]);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{
            borderColor: "var(--pc-border)",
            borderTopColor: "var(--pc-accent)",
          }}
        />
      </div>
    );
  }

  if (error) {
    return (
      <div className="rounded-[var(--radius-md)] border border-status-error/25 bg-status-error/10 p-3 text-sm text-status-error">
        {error}
      </div>
    );
  }

  if (items.length === 0) {
    return (
      <Card className="p-8 text-center text-sm text-pc-text-muted">
        Nothing configured under <strong>{section.label}</strong> yet. Click{" "}
        <strong>+ Add</strong> to get started.
      </Card>
    );
  }

  return (
    <Card padded={false} className="divide-y divide-pc-border overflow-hidden">
      {items.map((item) => (
        <button
          key={item.key}
          type="button"
          onClick={() => onPickType(item.key)}
          className="w-full flex items-center justify-between gap-3 px-4 py-3 text-left transition-colors hover:bg-pc-elevated/50"
        >
          <div className="flex-1 min-w-0">
            <div className="text-sm font-medium text-pc-text">
              {item.label}
            </div>
            <code className="block text-xs mt-0.5 text-pc-text-faint">
              {item.key}
            </code>
          </div>
          <div className="flex items-center gap-2 flex-shrink-0">
            {item.badge && (
              <Badge tone={item.badge === "active" ? "ok" : "neutral"}>
                {item.badge}
              </Badge>
            )}
            <ChevronRight className="h-4 w-4 text-pc-text-muted" />
          </div>
        </button>
      ))}
    </Card>
  );
}
