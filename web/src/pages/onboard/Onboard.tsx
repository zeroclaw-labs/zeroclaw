// Schema-driven onboarding wizard mirroring `zeroclaw onboard` (#6175).
//
// Layout:
//   ┌─ Sidebar ────┐ ┌─ Breadcrumb (Onboard › Section › ?picked) ─┐
//   │ Workspace ✓  │ │ Help text                                   │
//   │ Providers ▶  │ │                                             │
//   │ Channels     │ │  Either: <SectionPicker> (catalog list)     │
//   │ Memory       │ │     Or:  <FieldForm>     (the picked item)  │
//   │ Hardware     │ │                                             │
//   │ Tunnel       │ │  [ Back ]              [ Done — next ▶ ]    │
//   └──────────────┘ └─────────────────────────────────────────────┘
//
// Section list comes from /api/onboard/sections (single source of truth).
// Picker items come from /api/onboard/sections/<key>. Picking POSTs
// /api/onboard/sections/<key>/items/<picked> which instantiates the entry
// and returns the dotted prefix to render fields under. FieldForm reads
// /api/config/list?prefix=<that> and PATCHes on save. Provider model
// fields auto-fetch /api/onboard/catalog/models for the datalist.

import { forwardRef, useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AlertTriangle, Check, ChevronRight } from 'lucide-react';
import {
  ApiError,
  getMapKeys,
  getOnboardStatus,
  getProp,
  getSections,
  patchConfig,
  reloadDaemon,
  selectSectionItem,
  type PickerItem,
  type SectionInfo,
} from '../../lib/api';
import { isLocalModelProviderName } from '../../lib/modelProviders';
import FieldForm, { type FieldFormHandle } from '../../components/onboard/FieldForm';
import SectionPicker from '../../components/onboard/SectionPicker';

// Personality pulls in CodeMirror + markdown rendering (~270KB gzipped).
// Config's top-level nested field is exposed through the usual prop-path
// kebab form even though the persisted TOML table is `[onboard_state]`.
const COMPLETED_SECTIONS_PATH = 'onboard-state.completed-sections';

// Section list + its canonical order both come from the gateway,
// which derives them from `zeroclaw_config::sections::ONBOARDING_SECTIONS`
// (single source of truth, also used by the CLI runtime). The frontend
// filters by `is_onboarding`. First-run Browser onboarding presents the
// small happy path in dependency order. Agent-first setup needs a more
// explicit "configure this agent" flow, so keep agent creation at the end
// for now.
const FIRST_RUN_SECTION_ORDER = [
  'providers.models',
  'risk-profiles',
  'runtime-profiles',
  'storage',
  'memory',
  'agents',
] as const;
const FIRST_RUN_SECTION_KEYS = new Set<string>(FIRST_RUN_SECTION_ORDER);

export default function Onboard() {
  const navigate = useNavigate();
  const [sections, setSections] = useState<SectionInfo[]>([]);
  const [activeKey, setActiveKey] = useState<string | null>(null);
  const [picked, setPicked] = useState<{ item: PickerItem; fieldsPrefix: string } | null>(null);
  // When a provider/channel type is selected, show alias list inline before opening form.
  const [pickedType, setPickedType] = useState<{ item: PickerItem; sectionKey: string } | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [finishing, setFinishing] = useState(false);
  const [advancing, setAdvancing] = useState(false);
  const [showAdvanced, setShowAdvanced] = useState(false);
  const [canFinish, setCanFinish] = useState(false);
  const [finishIssues, setFinishIssues] = useState<string[] | null>(null);
  const [issueTitle, setIssueTitle] = useState('Complete this step before continuing.');
  const [applyIssue, setApplyIssue] = useState<string | null>(null);
  const [editingProfile, setEditingProfile] = useState(false);
  const [selectedAgentAlias, setSelectedAgentAlias] = useState<string | null>(null);
  // Ref into the currently-rendered FieldForm (direct-form sections like
  // Workspace, or the post-pick form for Providers/Channels/Tunnel) so
  // breadcrumb Next/Finish can flush unsaved edits before advancing.
  const formRef = useRef<FieldFormHandle | null>(null);

  const refreshReadiness = useCallback(async () => {
    try {
      const resp = await getSections();
      const onboardingSections = resp.sections.filter((s) => s.is_onboarding);
      setSections(onboardingSections);
      const status = await getOnboardStatus();
      const readyToFinish = !status.needs_onboarding && firstRunRequiredSectionsReady(onboardingSections);
      setCanFinish(readyToFinish);
      if (readyToFinish) setFinishIssues(null);
      const agents = await getMapKeys('agents').catch(() => null);
      const onlyAgent = agents?.keys.length === 1 ? agents.keys[0] : null;
      if (onlyAgent) {
        setSelectedAgentAlias((current) => current ?? onlyAgent);
      }
    } catch {
      // Keep the prior readiness state on transient auth/network errors.
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getSections()
      .then((resp) => {
        if (cancelled) return;
        // Filter to wizard sections; trust gateway-provided order.
        const ordered = resp.sections.filter((s) => s.is_onboarding);
        setSections(ordered);
        getMapKeys('agents')
          .then((agents) => {
            const onlyAgent = agents.keys.length === 1 ? agents.keys[0] : null;
            if (!cancelled && onlyAgent) {
              setSelectedAgentAlias((current) => current ?? onlyAgent);
            }
          })
          .catch(() => {});
        const firstRun = orderFirstRunSections(ordered.filter((s) => FIRST_RUN_SECTION_KEYS.has(s.key)));
        // Open the first not-yet-ready first-run section. A section can be
        // marked completed by navigation while still missing required setup.
        const next = firstRun.find((s) => !s.ready);
        setActiveKey(next?.key ?? firstRun[0]?.key ?? ordered[0]?.key ?? null);
        void refreshReadiness();
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof ApiError) {
          setError(`[${e.envelope.code}] ${e.envelope.message}`);
        } else {
          setError(`Couldn't load sections: ${e instanceof Error ? e.message : String(e)}`);
        }
      })
      .finally(() => !cancelled && setLoading(false));
    return () => {
      cancelled = true;
    };
  }, [refreshReadiness]);

  const activeSection = useMemo(
    () => sectionByKey(sections, activeKey, selectedAgentAlias, canFinish),
    [sections, activeKey, selectedAgentAlias, canFinish],
  );
  const firstRunSections = useMemo(
    () => firstRunSectionsFor(sections, selectedAgentAlias, canFinish),
    [canFinish, sections, selectedAgentAlias],
  );
  const advancedSections = useMemo(
    () => sections.filter((s) => !FIRST_RUN_SECTION_KEYS.has(s.key)),
    [sections],
  );
  const sidebarSections = useMemo(
    () =>
      showAdvanced
        ? [...firstRunSections, ...advancedSections]
        : firstRunSections.length > 0
          ? firstRunSections
          : sections,
    [advancedSections, firstRunSections, sections, showAdvanced],
  );
  const navigationSections = useMemo(
    () => {
      if (!activeSection) return sidebarSections;
      if (FIRST_RUN_SECTION_KEYS.has(activeSection.key) && firstRunSections.length > 0) {
        return firstRunSections;
      }
      if (showAdvanced && advancedSections.length > 0) return advancedSections;
      return firstRunSections.length > 0 ? firstRunSections : sections;
    },
    [activeSection, advancedSections, firstRunSections, sections, showAdvanced, sidebarSections],
  );

  const goToSection = (key: string) => {
    setActiveKey(key);
    setPicked(null);
    setPickedType(null);
    setEditingProfile(false);
    setFinishIssues(null);
    setApplyIssue(null);
  };

  const bindSelectionToSelectedAgent = useCallback(async (sectionKey: string, fieldsPrefix: string) => {
    if (!selectedAgentAlias) return;
    const binding = agentBindingForSelection(sectionKey, fieldsPrefix);
    if (!binding) return;

    const path = `agents.${selectedAgentAlias}.${binding.field}`;
    try {
      const current = await getProp(path).catch(() => null);
      const currentValue = current?.value;
      const currentText = typeof currentValue === 'string' ? currentValue.trim() : '';
      if (currentText && currentText !== '<unset>') return;
      await patchConfig([{ op: 'replace', path, value: binding.value }]);
    } catch (e) {
      // Keep selection usable even if auto-binding fails; Finish readiness will
      // still show the missing agent assignment.
      // eslint-disable-next-line no-console
      console.warn('Failed to bind onboarding selection to selected agent:', e);
    }
  }, [selectedAgentAlias]);

  const bindMemoryToSelectedAgent = useCallback(async (backend: string) => {
    if (!selectedAgentAlias) return;
    try {
      await patchConfig([
        { op: 'replace', path: `agents.${selectedAgentAlias}.memory.backend`, value: backend },
      ]);
    } catch (e) {
      // The global memory choice is still saved; the final assignment check
      // keeps the agent visible if the per-agent write fails.
      // eslint-disable-next-line no-console
      console.warn('Failed to bind memory backend to selected agent:', e);
    }
  }, [selectedAgentAlias]);

  const openWithAlias = async (item: PickerItem, sectionKey: string, alias: string) => {
    setFinishIssues(null);
    setApplyIssue(null);
    const resp = await selectSectionItem(sectionKey, item.key, alias);
    setPickedType(null);
    setPicked({ item, fieldsPrefix: resp.fields_prefix });
    setEditingProfile(false);
    await bindSelectionToSelectedAgent(sectionKey, resp.fields_prefix);
    await refreshReadiness();
  };

  const handlePick = async (item: PickerItem) => {
    if (!activeSection) return;
    setFinishIssues(null);
    setApplyIssue(null);
    // Two-tier `<type>.<alias>` sections (typed-family providers and
    // channels) flow into the type→alias picker; everything else picks
    // its item directly. Server-emitted shape drives the branch — no
    // hardcoded section keys.
    if (activeSection.shape === 'typed_family_map') {
      setPickedType({ item, sectionKey: activeSection.key });
      return;
    }
    try {
      const resp = await selectSectionItem(activeSection.key, item.key);
      setPicked({ item, fieldsPrefix: resp.fields_prefix });
      if (activeSection.key === 'memory') {
        await bindMemoryToSelectedAgent(item.key);
      }
      await refreshReadiness();
    } catch (e) {
      if (e instanceof ApiError) {
        setError(`Couldn't open ${item.label}: [${e.envelope.code}] ${e.envelope.message}`);
      } else {
        setError(`Couldn't open ${item.label}: ${e instanceof Error ? e.message : String(e)}`);
      }
    }
  };

  // Save any pending form edits first; refuse to advance if the save
  // failed (validator rejected something), so the user can fix it.
  const flushActiveForm = async (): Promise<boolean> => {
    if (!formRef.current) return true;
    try {
      return await formRef.current.flushSave();
    } catch {
      return false;
    }
  };

  const blockAdvance = (issues: string[]) => {
    setIssueTitle('Complete this step before continuing.');
    setFinishIssues(issues);
    return false;
  };

  const validateAdvance = async (): Promise<boolean> => {
    if (!activeSection) return false;
    setFinishIssues(null);

    if (activeSection.key === 'agents' && !selectedAgentAlias) {
      return blockAdvance(['Create or choose the agent you want to set up.']);
    }

    if (activeSection.key === 'providers.models') {
      if (!picked && !activeSection.ready) {
        return blockAdvance(['Choose a model provider, then set its required model and credential/auth.']);
      }
      if (picked) {
        const providerIssues = await modelProviderStepIssues(picked);
        if (providerIssues.length > 0) return blockAdvance(providerIssues);
      }
    }

    if (activeSection.key === 'storage' && !picked && !activeSection.ready) {
      return blockAdvance(['Choose a storage backend. SQLite is the safe default for single-node setup.']);
    }

    if (activeSection.key === 'memory' && !picked && !activeSection.ready) {
      return blockAdvance(['Choose or confirm the persistent memory backend. SQLite is the default; choose none to disable long-term memory.']);
    }

    return true;
  };

  const advanceSection = async () => {
    if (!activeSection) return;
    setAdvancing(true);
    try {
      if (!(await flushActiveForm())) return;
      if (!(await validateAdvance())) return;
      // Mark current section completed server-side, then jump to the next.
      try {
        const current = await getProp(COMPLETED_SECTIONS_PATH).catch(() => ({ value: '[]' }));
        const existing = parseCompleted(current.value);
        const completedKey = completionKeyFor(activeSection.key);
        if (!existing.includes(completedKey)) existing.push(completedKey);
        await patchConfig([
          { op: 'replace', path: COMPLETED_SECTIONS_PATH, value: existing },
        ]);
        setSections((prev) =>
          prev.map((s) =>
            s.key === completedKey ? { ...s, completed: true } : s,
          ),
        );
      } catch (e) {
        // Don't fail the flow on a marker failure — log and proceed.
        // eslint-disable-next-line no-console
        console.warn('Failed to persist completion marker:', e);
      }
      await refreshReadiness();
      const idx = navigationSections.findIndex((s) => s.key === activeSection.key);
      const next = navigationSections[idx + 1];
      if (next) {
        setActiveKey(next.key);
        setPicked(null);
        setPickedType(null);
        setEditingProfile(false);
      } else {
        setPicked(null);
        setPickedType(null);
        setEditingProfile(false);
      }
    } finally {
      setAdvancing(false);
    }
  };

  // Finish: save the current form (if any), mark the active section
  // completed, run a backend readiness check, then apply the finished config.
  // If the agent cannot reply yet, stay in onboarding and show exact missing
  // pieces instead of returning to the dashboard with an opaque chat error.
  const finishOnboarding = async () => {
    if (!activeSection) return;
    setFinishing(true);
    setFinishIssues(null);
    setApplyIssue(null);
    try {
      if (!(await flushActiveForm())) return;
      try {
        const current = await getProp(COMPLETED_SECTIONS_PATH).catch(() => ({ value: '[]' }));
        const existing = parseCompleted(current.value);
        const completedKey = completionKeyFor(activeSection.key);
        if (!existing.includes(completedKey)) existing.push(completedKey);
        await patchConfig([
          { op: 'replace', path: COMPLETED_SECTIONS_PATH, value: existing },
        ]);
      } catch (e) {
        // eslint-disable-next-line no-console
        console.warn('Failed to persist completion marker on finish:', e);
      }
      const status = await getOnboardStatus();
      const resp = await getSections();
      const onboardingSections = resp.sections.filter((s) => s.is_onboarding);
      setSections(onboardingSections);
      const wizardIssues = firstRunReadinessIssues(onboardingSections);
      const readyToFinish = !status.needs_onboarding && wizardIssues.length === 0;
      setCanFinish(readyToFinish);
      if (!readyToFinish) {
        setIssueTitle('Finish needs a runnable agent first.');
        setFinishIssues(
          status.missing.length > 0 || wizardIssues.length > 0
            ? [...status.missing, ...wizardIssues]
            : ['Complete the required setup steps before finishing onboarding.'],
        );
        return;
      }
      try {
        await reloadDaemon();
        await new Promise((r) => setTimeout(r, 400));
      } catch (e) {
        // eslint-disable-next-line no-console
        console.warn('Daemon reload failed after onboarding; user can retry from /config:', e);
        setApplyIssue(
          'Setup is saved. This gateway could not apply the changes automatically. Restart the standalone gateway process, then open the dashboard again.',
        );
        return;
      }
      navigate('/');
    } finally {
      setFinishing(false);
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center h-64">
        <div
          className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }}
        />
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-6">
        <div
          className="rounded-xl border p-4 text-sm"
          style={{
            background: 'rgba(239, 68, 68, 0.08)',
            borderColor: 'rgba(239, 68, 68, 0.2)',
            color: '#f87171',
          }}
        >
          {error}
        </div>
      </div>
    );
  }

  const activeBreadcrumbDetail = activeSection
    ? breadcrumbDetail(activeSection, picked, pickedType)
    : null;

  return (
    <div className="flex h-full overflow-hidden">
      {/* Sidebar */}
      <aside
        className="w-56 flex-shrink-0 border-r overflow-y-auto"
        style={{
          borderColor: 'var(--pc-border)',
          background: 'var(--pc-bg-surface)',
        }}
      >
        <div
          className="px-4 py-3 text-xs font-semibold uppercase tracking-wider"
          style={{ color: 'var(--pc-text-secondary)' }}
        >
          Sections
        </div>
        <nav className="flex flex-col">
          {sidebarSections.map((s) => (
            <button
              key={s.key}
              type="button"
              onClick={() => goToSection(s.key)}
              className="flex items-center justify-between gap-2 px-4 py-2.5 text-sm text-left transition-colors"
              style={{
                background:
                  s.key === activeKey ? 'var(--pc-accent-glow)' : 'transparent',
                color:
                  s.key === activeKey
                    ? 'var(--pc-accent)'
                    : 'var(--pc-text-primary)',
                fontWeight: s.key === activeKey ? 600 : 400,
                borderLeft:
                  s.key === activeKey
                    ? '2px solid var(--pc-accent)'
                    : '2px solid transparent',
              }}
            >
              <span className="flex items-center gap-2">
                {s.ready && (
                  <Check
                    className="h-3.5 w-3.5"
                    style={{ color: 'var(--color-status-success)' }}
                  />
                )}
                {sidebarLabel(s, selectedAgentAlias)}
              </span>
              {s.key === activeKey && <ChevronRight className="h-3.5 w-3.5" />}
            </button>
          ))}
          {advancedSections.length > 0 && (
            <button
              type="button"
              onClick={() => setShowAdvanced((show) => !show)}
              className="px-4 py-2.5 text-sm text-left transition-colors"
              style={{ color: 'var(--pc-text-secondary)' }}
            >
              {showAdvanced ? 'Hide advanced setup' : 'Show advanced setup'}
            </button>
          )}
        </nav>
      </aside>

      {/* Main pane */}
      <main className="flex-1 overflow-y-auto p-6">
        {activeSection && (
          <div className="flex flex-col gap-4 max-w-3xl">
            {/* Breadcrumb + always-available Next/Done. The form's own Save
                bar advances the flow on save, but users editing nothing
                (Hardware defaults, e.g.) still need a way out — this gives
                them one regardless of dirty state. */}
            <div className="flex items-center justify-between gap-3 flex-wrap">
              <div
                className="text-sm flex items-center gap-1.5 flex-wrap"
                style={{ color: 'var(--pc-text-muted)' }}
              >
                <span style={{ color: 'var(--pc-text-secondary)' }}>Onboard</span>
                <ChevronRight className="h-3 w-3" />
                <span
                  style={{
                    color: activeBreadcrumbDetail ? 'var(--pc-text-secondary)' : 'var(--pc-accent)',
                    cursor: activeBreadcrumbDetail ? 'pointer' : 'default',
                    fontWeight: activeBreadcrumbDetail ? 400 : 600,
                  }}
                  onClick={() => { setPicked(null); setPickedType(null); setEditingProfile(false); }}
                >
                  {activeSection.label}
                </span>
                {activeBreadcrumbDetail && (
                  <>
                    <ChevronRight className="h-3 w-3" />
                    <span style={{ color: 'var(--pc-accent)', fontWeight: 600 }}>
                      {activeBreadcrumbDetail}
                    </span>
                  </>
                )}
              </div>
              <div className="flex items-center gap-2 flex-shrink-0">
                {canFinish && (
                  <button
                    type="button"
                    disabled={finishing || advancing}
                    onClick={() => void finishOnboarding()}
                    className="btn-secondary inline-flex items-center gap-1.5 text-sm px-3 py-2"
                    title="Apply the completed setup"
                  >
                    {finishing ? 'Finishing…' : 'Finish'}
                  </button>
                )}
                {!isLastSection(navigationSections, activeSection.key) && (
                  <button
                    type="button"
                    disabled={finishing || advancing}
                    onClick={() => void advanceSection()}
                    className="btn-electric inline-flex items-center gap-1.5 text-sm px-4 py-2"
                    title="Save and move to the next section"
                  >
                    {advancing ? 'Saving…' : 'Next ▶'}
                  </button>
                )}
              </div>
            </div>
            {finishIssues && (
              <div
                className="rounded-xl border p-4 text-sm flex items-start gap-3"
                style={{
                  background: 'rgba(239, 68, 68, 0.08)',
                  borderColor: 'rgba(239, 68, 68, 0.2)',
                  color: '#fca5a5',
                }}
              >
                <AlertTriangle className="h-4 w-4 flex-shrink-0 mt-0.5" />
                <div>
                  <p className="font-medium mb-2" style={{ color: 'var(--pc-text-primary)' }}>
                    {issueTitle}
                  </p>
                  <ul className="list-disc pl-5 space-y-1">
                    {finishIssues.map((issue) => (
                      <li key={issue}>{issue}</li>
                    ))}
                  </ul>
                </div>
              </div>
            )}
            {applyIssue && (
              <div
                className="rounded-xl border p-4 text-sm flex items-start gap-3"
                style={{
                  background: 'rgba(245, 180, 0, 0.08)',
                  borderColor: 'rgba(245, 180, 0, 0.25)',
                  color: '#fbbf24',
                }}
              >
                <AlertTriangle className="h-4 w-4 flex-shrink-0 mt-0.5" />
                <div>
                  <p className="font-medium mb-1" style={{ color: 'var(--pc-text-primary)' }}>
                    Setup is saved but not applied yet.
                  </p>
                  <p>{applyIssue}</p>
                </div>
              </div>
            )}

            {/* Picker / form dispatch — driven by the server-emitted
                `shape` flag so /onboard and /config render identically
                for the same section. */}
            {activeSection.key === 'agents' && selectedAgentAlias ? (
              <>
                <AgentFirstRunForm
                  ref={formRef}
                  prefix={`agents.${selectedAgentAlias}`}
                  title={`Agent: ${selectedAgentAlias}`}
                  onSaved={() => {
                    void refreshReadiness();
                  }}
                />
                <FirstRunCompleteActions
                  canFinish={canFinish}
                  finishing={finishing}
                  onFinish={() => void finishOnboarding()}
                  onAdvanced={() => {
                    setShowAdvanced(true);
                    if (advancedSections[0]) goToSection(advancedSections[0].key);
                  }}
                />
              </>
            ) : !activeSection.has_picker ? (
              <>
                <OnboardingFormGuide sectionKey={activeSection.key} prefix={activeSection.key} />
                <FieldForm
                  ref={formRef}
                  prefix={activeSection.key}
                  title={activeSection.label}
                  onSaved={() => void refreshReadiness()}
                />
              </>
            ) : picked && isDefaultProfileSection(activeSection.key) && !editingProfile ? (
              <DefaultProfileSummary
                sectionKey={activeSection.key}
                prefix={picked.fieldsPrefix}
                onEdit={() => setEditingProfile(true)}
                onContinue={() => void advanceSection()}
                onPresetApplied={() => void refreshReadiness()}
              />
            ) : picked && activeSection.key === 'memory' && !editingProfile ? (
              <MemoryBackendSummary
                item={picked.item}
                onEdit={() => setEditingProfile(true)}
                onContinue={() => void advanceSection()}
              />
            ) : picked ? (
              <>
                <OnboardingFormGuide sectionKey={activeSection.key} prefix={picked.fieldsPrefix} />
                <FieldForm
                  ref={formRef}
                  prefix={picked.fieldsPrefix}
                  title={formTitleFor(activeSection.key, picked)}
                  onSaved={() => {
                    setPicked(null);
                    void refreshReadiness();
                  }}
                />
              </>
            ) : pickedType ? (
              <OnboardAliasListView
                sectionKey={pickedType.sectionKey}
                typeKey={pickedType.item.key}
                typeLabel={pickedType.item.label}
                onSelectAlias={(alias) => openWithAlias(pickedType.item, pickedType.sectionKey, alias)}
              />
            ) : activeSection.shape === 'one_tier_alias_map' ? (
              // Flat alias map (agents). Same UX as /config/<section>:
              // alias list with Create. Picking an alias opens its form.
              <OnboardOneTierAliasView
                sectionKey={activeSection.key}
                onSelectAlias={async (alias) => {
                  try {
                    const resp = await selectSectionItem(activeSection.key, alias);
                    if (activeSection.key === 'agents') {
                      setSelectedAgentAlias(alias);
                    }
                    setPicked({
                      item: { key: alias, label: alias },
                      fieldsPrefix: resp.fields_prefix,
                    });
                    setEditingProfile(false);
                    await bindSelectionToSelectedAgent(activeSection.key, resp.fields_prefix);
                    await refreshReadiness();
                  } catch (e) {
                    setError(
                      e instanceof ApiError
                        ? `[${e.envelope.code}] ${e.envelope.message}`
                        : `Couldn't open ${alias}: ${e instanceof Error ? e.message : String(e)}`,
                    );
                  }
                }}
              />
            ) : (
              <SectionPicker
                sectionKey={activeSection.key}
                help={activeSection.key === 'storage' ? '' : activeSection.help}
                onPick={(item) => void handlePick(item)}
                onSkip={() => void advanceSection()}
              />
            )}
          </div>
        )}
      </main>

    </div>
  );
}

function breadcrumbDetail(
  section: SectionInfo,
  picked: { item: PickerItem; fieldsPrefix: string } | null,
  pickedType: { item: PickerItem; sectionKey: string } | null,
): string | null {
  if (picked) {
    const alias = picked.fieldsPrefix.split('.').slice(-1)[0] ?? picked.item.label;
    return `${entityLabel(section.key, picked.item.label)}: ${alias}`;
  }
  if (pickedType) return `${pickedType.item.label} aliases`;
  return null;
}

function firstRunRequiredSectionsReady(sections: SectionInfo[]): boolean {
  return firstRunReadinessIssues(sections).length === 0;
}

function firstRunReadinessIssues(sections: SectionInfo[]): string[] {
  const labels = new Map(sections.map((section) => [section.key, section.label]));
  const byKey = new Map(sections.map((section) => [section.key, section]));
  return FIRST_RUN_SECTION_ORDER
    .filter((key) => !byKey.get(key)?.ready)
    .map((key) => `${labels.get(key) ?? key} is not complete yet.`);
}

function sectionByKey(
  sections: SectionInfo[],
  key: string | null,
  _selectedAgentAlias: string | null,
  _canFinish: boolean,
): SectionInfo | null {
  if (!key) return null;
  return sections.find((s) => s.key === key) ?? null;
}

function firstRunSectionsFor(
  sections: SectionInfo[],
  selectedAgentAlias: string | null,
  canFinish: boolean,
): SectionInfo[] {
  const real = sections.filter((s) => FIRST_RUN_SECTION_KEYS.has(s.key));
  return orderFirstRunSections(real).map((section) =>
    section.key === 'agents' && selectedAgentAlias
      ? { ...section, completed: canFinish, ready: canFinish }
      : section,
  );
}

function completionKeyFor(sectionKey: string): string {
  return sectionKey;
}

function sidebarLabel(section: SectionInfo, selectedAgentAlias: string | null): string {
  if (section.key === 'agents') return selectedAgentAlias ? `Agent: ${selectedAgentAlias}` : 'Agent';
  return section.label;
}

function orderFirstRunSections(sections: SectionInfo[]): SectionInfo[] {
  const order = new Map<string, number>(FIRST_RUN_SECTION_ORDER.map((key, index) => [key, index]));
  return [...sections].sort((a, b) => {
    const aRank = order.get(a.key) ?? Number.MAX_SAFE_INTEGER;
    const bRank = order.get(b.key) ?? Number.MAX_SAFE_INTEGER;
    return aRank - bRank;
  });
}

function agentBindingForSelection(
  sectionKey: string,
  fieldsPrefix: string,
): { field: string; value: string } | null {
  const parts = fieldsPrefix.split('.');
  if (sectionKey === 'providers.models' && parts[0] === 'providers' && parts[1] === 'models') {
    const providerType = parts[2];
    const providerAlias = parts[3];
    if (providerType && providerAlias) {
      return { field: 'model-provider', value: `${canonicalProviderRefSegment(providerType)}.${providerAlias}` };
    }
  }
  if (sectionKey === 'risk-profiles') {
    const alias = parts[1];
    return alias ? { field: 'risk-profile', value: alias } : null;
  }
  if (sectionKey === 'runtime-profiles') {
    const alias = parts[1];
    return alias ? { field: 'runtime-profile', value: alias } : null;
  }
  return null;
}

function providerRefForFieldsPrefix(fieldsPrefix: string): string | null {
  const parts = fieldsPrefix.split('.');
  if (parts[0] !== 'providers' || parts[1] !== 'models') return null;
  const providerType = parts[2];
  const providerAlias = parts[3];
  return providerType && providerAlias ? `${canonicalProviderRefSegment(providerType)}.${providerAlias}` : null;
}

async function modelProviderStepIssues(picked: { item: PickerItem; fieldsPrefix: string }): Promise<string[]> {
  const providerRef = providerRefForFieldsPrefix(picked.fieldsPrefix) ?? picked.item.label;
  const model = await getProp(`${picked.fieldsPrefix}.model`).catch(() => null);
  if (!hasTextValue(model?.value)) {
    return [`Choose a model for model provider \`${providerRef}\`.`];
  }

  if (isLocalPickerItem(picked.item)) return [];

  const apiKey = await getProp(`${picked.fieldsPrefix}.api-key`).catch(() => null);
  const openAiAuth = await getProp(`${picked.fieldsPrefix}.requires-openai-auth`).catch(() => null);
  if (apiKey?.populated || isTruthyValue(openAiAuth?.value)) return [];
  return [`Set credential/auth for model provider \`${providerRef}\`.`];
}

function hasTextValue(value: unknown): boolean {
  return typeof value === 'string' && value.trim().length > 0 && value.trim() !== '<unset>';
}

function isTruthyValue(value: unknown): boolean {
  if (typeof value === 'boolean') return value;
  if (typeof value === 'string') return value.trim().toLowerCase() === 'true';
  return false;
}

function isLocalPickerItem(item: PickerItem): boolean {
  return item.description?.toLowerCase().includes('local') ?? false;
}

function canonicalProviderRefSegment(providerType: string): string {
  return providerType.replace(/-/g, '_');
}

function formTitleFor(sectionKey: string, picked: { item: PickerItem; fieldsPrefix: string }): string {
  const alias = picked.fieldsPrefix.split('.').slice(-1)[0] ?? picked.item.label;
  return `${entityLabel(sectionKey, picked.item.label)}: ${alias}`;
}

function entityLabel(sectionKey: string, itemLabel: string): string {
  switch (sectionKey) {
    case 'providers.models':
      return `${itemLabel} provider`;
    case 'providers.tts':
      return `${itemLabel} TTS provider`;
    case 'providers.transcription':
      return `${itemLabel} transcription provider`;
    case 'risk-profiles':
      return 'Risk profile';
    case 'runtime-profiles':
      return 'Runtime profile';
    case 'storage':
      return `${capitalize(itemLabel)} storage`;
    case 'agents':
      return 'Agent';
    default:
      return itemLabel;
  }
}

function OnboardingFormGuide({ sectionKey, prefix }: { sectionKey: string; prefix: string }) {
  const guide = guideFor(sectionKey, prefix);
  if (!guide) return null;
  return (
    <div
      className="rounded-xl border p-4 text-sm"
      style={{
        background: 'var(--pc-bg-surface-subtle)',
        borderColor: 'var(--pc-border)',
        color: 'var(--pc-text-secondary)',
      }}
    >
      <p className="font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>
        {guide.title}
      </p>
      <p>{guide.body}</p>
      {guide.items && (
        <ul className="list-disc pl-5 mt-2 space-y-1">
          {guide.items.map((item) => (
            <li key={item}>{item}</li>
          ))}
        </ul>
      )}
    </div>
  );
}

function guideFor(sectionKey: string, prefix: string): { title: string; body: string; items?: string[] } | null {
  if (prefix.startsWith('providers.models.')) {
    const provider = prefix.split('.')[2] ?? '';
    const local = isLocalModelProvider(provider);
    return {
      title: 'Set up this provider',
      body: local
        ? 'For a local provider, choose the model you want this alias to use and confirm the local server or CLI endpoint is available. Most other fields are advanced tuning.'
        : 'For a hosted provider, choose a model and set the API key or supported auth mode. Most request, formatting, and cost fields can stay at their defaults.',
      items: local
        ? ['Required before chat: model and reachable local endpoint or CLI.', 'Optional: timeout, temperature, request-format, and pricing fields.']
        : ['Required before chat: model and credential/auth.', 'Optional: timeout, temperature, request-format, and pricing fields.'],
    };
  }
  if (sectionKey === 'risk-profiles') {
    return {
      title: 'Reusable safety profile',
      body: 'The default risk profile is usable as-is for first-run setup. Edit it if you want to change tool, command, path, or approval rules before creating an agent.',
    };
  }
  if (sectionKey === 'runtime-profiles') {
    return {
      title: 'Reusable runtime profile',
      body: 'The default runtime profile is usable as-is for first-run setup. Edit it if you want to change agentic mode, iteration limits, timeouts, cost limits, or context behavior.',
    };
  }
  if (sectionKey === 'storage') {
    return {
      title: 'Storage backend instance',
      body: 'Most single-node setups can use one SQLite instance called default. Create extra aliases only when you need multiple storage backends for different agents or environments.',
    };
  }
  if (sectionKey === 'memory') {
    return {
      title: 'Persistent memory backend',
      body: 'SQLite is the default for local first-run setup. Pick none only if you want to disable long-term memory entirely.',
    };
  }
  if (sectionKey === 'agents') {
    return {
      title: 'Runnable agent checklist',
      body: 'This is the assistant you will chat with. Before Finish appears, the agent must be enabled and point at the provider and profiles you set up earlier.',
    };
  }
  return null;
}

const AGENT_FIRST_RUN_FIELDS = ['enabled', 'model-provider', 'risk-profile', 'runtime-profile'];

function isAgentFirstRunPath(prefix: string, path: string): boolean {
  return AGENT_FIRST_RUN_FIELDS.some((field) => path === `${prefix}.${field}`);
}

function FirstRunCompleteActions({
  canFinish,
  finishing,
  onFinish,
  onAdvanced,
}: {
  canFinish: boolean;
  finishing: boolean;
  onFinish: () => void;
  onAdvanced: () => void;
}) {
  return (
    <div
      className="rounded-xl border p-4 text-sm flex flex-col gap-3"
      style={{
        background: 'var(--pc-bg-surface-subtle)',
        borderColor: 'var(--pc-border)',
        color: 'var(--pc-text-secondary)',
      }}
    >
      <div>
        <p className="font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>
          {canFinish ? 'Basic setup is ready to apply' : 'Finish the required assignments'}
        </p>
        <p>
          {canFinish
            ? 'Optional advanced setup includes skills, skill bundles, MCP, channels, peer groups, cron, tunnel, TTS, and transcription providers.'
            : 'Choose the required model provider, risk profile, and runtime profile above. Advanced setup can wait until the agent can reply.'}
        </p>
      </div>
      <div className="flex flex-wrap items-center gap-2">
        {canFinish ? (
          <button
            type="button"
            className="btn-electric text-sm px-4 py-2"
            disabled={finishing}
            onClick={onFinish}
          >
            {finishing ? 'Finishing…' : 'Finish'}
          </button>
        ) : (
          <p className="text-sm" style={{ color: 'var(--color-status-error)' }}>
            Finish appears once the required agent assignments are complete.
          </p>
        )}
        <button
          type="button"
          className="btn-secondary text-sm px-4 py-2"
          onClick={onAdvanced}
        >
          Continue advanced setup
        </button>
      </div>
    </div>
  );
}

const AgentFirstRunForm = forwardRef<FieldFormHandle, {
  prefix: string;
  title: string;
  onSaved: () => void;
}>(function AgentFirstRunForm({ prefix, title, onSaved }, ref) {
  const [showAdvanced, setShowAdvanced] = useState(false);
  return (
    <div className="flex flex-col gap-4">
      <div
        className="rounded-xl border p-4 text-sm"
        style={{
          background: 'var(--pc-bg-surface-subtle)',
          borderColor: 'var(--pc-border)',
          color: 'var(--pc-text-secondary)',
        }}
      >
        <p className="font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>
          Agent assignments
        </p>
        <p>
          Review the model provider, risk profile, and runtime profile this
          agent uses. First-run setup preselects the defaults when they exist.
        </p>
      </div>
      <FieldForm
        ref={ref}
        prefix={prefix}
        title={title}
        showDelete={false}
        includePath={(path) => isAgentFirstRunPath(prefix, path)}
        onSaved={onSaved}
      />
      <div>
        <button
          type="button"
          className="btn-secondary text-sm px-4 py-2"
          onClick={() => setShowAdvanced((show) => !show)}
        >
          {showAdvanced ? 'Hide advanced agent settings' : 'Show advanced agent settings'}
        </button>
      </div>
      {showAdvanced && (
        <FieldForm
          prefix={prefix}
          title="Advanced agent settings"
          includePath={(path) => !isAgentFirstRunPath(prefix, path)}
          onSaved={onSaved}
        />
      )}
    </div>
  );
});

function isDefaultProfileSection(sectionKey: string): boolean {
  return sectionKey === 'risk-profiles' || sectionKey === 'runtime-profiles';
}

function DefaultProfileSummary({
  sectionKey,
  prefix,
  onEdit,
  onContinue,
  onPresetApplied,
}: {
  sectionKey: string;
  prefix: string;
  onEdit: () => void;
  onContinue: () => void;
  onPresetApplied: () => void;
}) {
  const [savingPreset, setSavingPreset] = useState<string | null>(null);
  const [appliedPreset, setAppliedPreset] = useState<string | null>(null);
  const isRisk = sectionKey === 'risk-profiles';
  const alias = prefix.split('.').slice(-1)[0] ?? 'default';
  const applyRiskPreset = async (preset: RiskPreset) => {
    setSavingPreset(preset.key);
    try {
      await patchConfig(riskPresetOps(prefix, preset));
      setAppliedPreset(preset.key);
      onPresetApplied();
    } finally {
      setSavingPreset(null);
    }
  };

  return (
    <div className="flex flex-col gap-4">
      <div
        className="rounded-xl border p-4 text-sm"
        style={{
          background: 'var(--pc-bg-surface-subtle)',
          borderColor: 'var(--pc-border)',
          color: 'var(--pc-text-secondary)',
        }}
      >
        <p className="font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>
          {isRisk ? 'Safety profile created' : 'Runtime profile created'}
        </p>
        <p>
          {isRisk
            ? `Agent setup can use risk profile ${alias} as-is. Edit it if you want a different safety posture.`
            : `Agent setup can use runtime profile ${alias} as-is. Edit it if you want to change agentic mode, iteration limits, timeouts, cost limits, or context behavior.`}
        </p>
      </div>

      {isRisk && (
        <div className="grid gap-3 sm:grid-cols-2 xl:grid-cols-4">
          {RISK_PRESETS.map((preset) => (
            <button
              key={preset.key}
              type="button"
              onClick={() => void applyRiskPreset(preset)}
              disabled={savingPreset !== null}
              className="rounded-xl border p-3 text-left transition-colors hover:opacity-90"
              style={{
                borderColor:
                  appliedPreset === preset.key ? 'var(--pc-accent)' : 'var(--pc-border)',
                background:
                  appliedPreset === preset.key ? 'var(--pc-accent-glow)' : 'var(--pc-bg-surface)',
                color: 'var(--pc-text-secondary)',
              }}
            >
              <span className="flex items-center justify-between gap-2 text-sm font-semibold" style={{ color: 'var(--pc-text-primary)' }}>
                <span>{savingPreset === preset.key ? 'Applying…' : preset.label}</span>
                {appliedPreset === preset.key && (
                  <span className="text-xs" style={{ color: 'var(--pc-accent)' }}>
                    selected
                  </span>
                )}
              </span>
              <span className="block text-xs mt-1">{preset.description}</span>
              {preset.warning && (
                <span className="block text-xs mt-2" style={{ color: 'var(--color-status-error)' }}>
                  {preset.warning}
                </span>
              )}
            </button>
          ))}
        </div>
      )}

      {isRisk && appliedPreset && (
        <p className="text-sm" style={{ color: 'var(--color-status-success)' }}>
          {RISK_PRESETS.find((preset) => preset.key === appliedPreset)?.label ?? 'Preset'} applied to {alias}.
        </p>
      )}

      <div className="flex items-center gap-2">
        <button type="button" className="btn-electric text-sm px-4 py-2" onClick={onContinue}>
          Continue
        </button>
        <button type="button" className="btn-secondary text-sm px-4 py-2" onClick={onEdit}>
          Edit profile
        </button>
      </div>
    </div>
  );
}

function MemoryBackendSummary({
  item,
  onEdit,
  onContinue,
}: {
  item: PickerItem;
  onEdit: () => void;
  onContinue: () => void;
}) {
  const disabled = item.key === 'none';
  return (
    <div className="flex flex-col gap-4">
      <div
        className="rounded-xl border p-4 text-sm"
        style={{
          background: 'var(--pc-bg-surface-subtle)',
          borderColor: 'var(--pc-border)',
          color: 'var(--pc-text-secondary)',
        }}
      >
        <p className="font-semibold mb-1" style={{ color: 'var(--pc-text-primary)' }}>
          Memory backend selected
        </p>
        <p>
          {disabled
            ? 'Persistent memory is disabled for this setup. You can enable a memory backend later from Config.'
            : `${item.label} is selected for persistent memory. Most first-run setups can use this as-is.`}
        </p>
      </div>
      <div className="flex items-center gap-2">
        <button type="button" className="btn-electric text-sm px-4 py-2" onClick={onContinue}>
          Continue
        </button>
        <button type="button" className="btn-secondary text-sm px-4 py-2" onClick={onEdit}>
          Edit memory settings
        </button>
      </div>
    </div>
  );
}

interface RiskPreset {
  key: string;
  label: string;
  description: string;
  warning?: string;
  level: 'readonly' | 'supervised' | 'full';
  allowedCommands: string[];
  requireApprovalForMediumRisk: boolean;
  blockHighRiskCommands: boolean;
}

const RISK_PRESETS: RiskPreset[] = [
  {
    key: 'read_only',
    label: 'Read-only',
    description: 'Inspection-oriented: shell commands are limited and medium-risk actions still need approval.',
    level: 'readonly',
    allowedCommands: ['git', 'ls', 'pwd', 'cat', 'head', 'tail', 'rg', 'sed'],
    requireApprovalForMediumRisk: true,
    blockHighRiskCommands: true,
  },
  {
    key: 'balanced',
    label: 'Balanced default',
    description: 'Good first-run posture: common development tools, approval for medium risk, high-risk commands blocked.',
    level: 'supervised',
    allowedCommands: [
      'git',
      'npm',
      'cargo',
      'ls',
      'cat',
      'grep',
      'rg',
      'sed',
      'head',
      'tail',
      'find',
      'mkdir',
      'touch',
      'python',
      'python3',
      'node',
      'curl',
      'tar',
      'unzip',
      'which',
      'pwd',
      'date',
    ],
    requireApprovalForMediumRisk: true,
    blockHighRiskCommands: true,
  },
  {
    key: 'local_dev',
    label: 'Local dev',
    description: 'More permissive for a trusted local workspace, while still blocking high-risk command patterns.',
    level: 'full',
    allowedCommands: [
      'git',
      'gh',
      'npm',
      'npx',
      'node',
      'cargo',
      'rustc',
      'python',
      'python3',
      'uv',
      'ls',
      'cat',
      'grep',
      'rg',
      'sed',
      'head',
      'tail',
      'find',
      'mkdir',
      'touch',
      'curl',
      'tar',
      'unzip',
      'which',
      'pwd',
      'date',
    ],
    requireApprovalForMediumRisk: false,
    blockHighRiskCommands: true,
  },
  {
    key: 'yolo',
    label: 'YOLO',
    description: 'Maximum local autonomy for a trusted disposable workspace.',
    warning: 'Not recommended unless you understand the risk.',
    level: 'full',
    allowedCommands: [
      'git',
      'gh',
      'npm',
      'npx',
      'node',
      'cargo',
      'rustc',
      'python',
      'python3',
      'uv',
      'bash',
      'sh',
      'zsh',
      'make',
      'docker',
      'curl',
      'tar',
      'unzip',
      'ls',
      'cat',
      'grep',
      'rg',
      'sed',
      'head',
      'tail',
      'find',
      'mkdir',
      'touch',
      'cp',
      'mv',
      'rm',
      'chmod',
      'which',
      'pwd',
      'date',
    ],
    requireApprovalForMediumRisk: false,
    blockHighRiskCommands: false,
  },
];

function riskPresetOps(prefix: string, preset: RiskPreset) {
  return [
    { op: 'replace' as const, path: `${prefix}.level`, value: preset.level },
    { op: 'replace' as const, path: `${prefix}.allowed-commands`, value: preset.allowedCommands },
    {
      op: 'replace' as const,
      path: `${prefix}.require-approval-for-medium-risk`,
      value: preset.requireApprovalForMediumRisk,
    },
    {
      op: 'replace' as const,
      path: `${prefix}.block-high-risk-commands`,
      value: preset.blockHighRiskCommands,
    },
  ];
}

function isLocalModelProvider(provider: string): boolean {
  return isLocalModelProviderName(provider);
}

function capitalize(value: string): string {
  return value.length === 0 ? value : value.charAt(0).toUpperCase() + value.slice(1);
}

function OnboardAliasListView({
  sectionKey,
  typeKey,
  typeLabel,
  onSelectAlias,
}: {
  sectionKey: string;
  typeKey: string;
  typeLabel: string;
  onSelectAlias: (alias: string) => Promise<void>;
}) {
  const [aliases, setAliases] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [newAlias, setNewAlias] = useState('');
  const [aliasError, setAliasError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  const mapPath = `${sectionKey}.${typedMapPathSegment(sectionKey, typeKey)}`;
  const aliasHelpLabel = typedAliasHelpLabel(sectionKey, typeLabel);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getMapKeys(mapPath)
      .then((r) => { if (!cancelled) setAliases(r.keys); })
      .catch(() => { if (!cancelled) setAliases([]); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [mapPath]);

  const submit = async () => {
    const trimmed = newAlias.trim() || suggestAlias(aliases);
    setAliasError(null);
    const validationError = validateAlias(trimmed);
    if (validationError) {
      setAliasError(validationError);
      return;
    }
    try {
      await onSelectAlias(trimmed);
    } catch (e) {
      setAliasError(
        e instanceof ApiError ? e.envelope.message : (e instanceof Error ? e.message : String(e)),
      );
    }
  };

  return (
    <div className="flex flex-col gap-3">
      <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
        {typeLabel} — select an existing alias or create one
      </p>
      <AliasHelpBox what={aliasHelpLabel} />
      {loading ? (
        <div className="flex items-center justify-center py-12">
          <div className="h-8 w-8 border-2 rounded-full animate-spin"
            style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
        </div>
      ) : (
        <>
          {error && (
            <div
              className="rounded-xl border p-3 text-sm"
              style={{ background: 'rgba(239,68,68,0.08)', borderColor: 'rgba(239,68,68,0.2)', color: '#f87171' }}
            >
              {error}
            </div>
          )}
          <div className="surface-panel divide-y" style={{ borderColor: 'var(--pc-border)' }}>
          {aliases.map((alias) => (
            <button
              key={alias}
              type="button"
              onClick={() => {
                onSelectAlias(alias).catch((e) => {
                  setError(
                    e instanceof ApiError
                      ? `[${e.envelope.code}] ${e.envelope.message}`
                      : (e instanceof Error ? e.message : String(e)),
                  );
                });
              }}
              className="w-full flex items-center justify-between gap-3 px-4 py-3 text-left text-sm transition-colors hover:opacity-90"
            >
              <div>
                <span style={{ color: 'var(--pc-text-primary)', fontWeight: 500 }}>{alias}</span>
                <code className="block text-xs mt-0.5" style={{ color: 'var(--pc-text-faint)' }}>
                  {mapPath}.{alias}
                </code>
              </div>
              <ChevronRight className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-text-muted)' }} />
            </button>
          ))}
          <div className="flex flex-col gap-1 px-4 py-3">
            <div className="flex items-center gap-2">
              <input
                type="text"
                className="input-electric flex-1 px-3 py-1.5 text-sm"
                placeholder={suggestAlias(aliases)}
                value={newAlias}
                onChange={(e) => { setNewAlias(e.target.value); setAliasError(null); }}
                onKeyDown={(e) => { if (e.key === 'Enter') void submit(); }}
                // eslint-disable-next-line jsx-a11y/no-autofocus
                autoFocus={aliases.length === 0}
              />
              <button type="button" onClick={() => void submit()} className="btn-electric text-sm px-3 py-1.5 flex-shrink-0">
                Create
              </button>
            </div>
            {aliasError && (
              <p className="text-xs" style={{ color: 'var(--color-status-error)' }}>{aliasError}</p>
            )}
          </div>
        </div>
        </>
      )}
    </div>
  );
}

/// Help block shown above every alias-input field (one-tier and typed-family
/// alike) so the user knows what they're naming and what the rules are.
/// Constraints come from `validate_alias_key` in zeroclaw-config — keep this
/// blurb in sync with that validator's rules if they ever loosen.
function AliasHelpBox({ what }: { what: string }) {
  return (
    <div
      className="rounded-md border px-3 py-2 text-xs"
      style={{
        borderColor: 'var(--pc-border)',
        background: 'var(--pc-bg-surface-subtle)',
        color: 'var(--pc-text-secondary)',
      }}
    >
      <p className="mb-1">
        <strong>{what} alias.</strong> {aliasHelpText(what)}
      </p>
      <p className="mb-0">
        Rules: lowercase letters, digits, single underscores; 1–63 chars; no
        leading/trailing/double underscores, no dots, hyphens, or spaces.{' '}
        <strong>Aliases can’t be renamed in v0.8.0</strong> — pick something
        you’ll keep, or delete and recreate.
      </p>
    </div>
  );
}

function OnboardOneTierAliasView({
  sectionKey,
  onSelectAlias,
}: {
  sectionKey: string;
  onSelectAlias: (alias: string) => Promise<void>;
}) {
  const [aliases, setAliases] = useState<string[]>([]);
  const [loading, setLoading] = useState(true);
  const [newAlias, setNewAlias] = useState('');
  const [aliasError, setAliasError] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getMapKeys(sectionKey)
      .then((r) => { if (!cancelled) setAliases(r.keys); })
      .catch(() => { if (!cancelled) setAliases([]); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [sectionKey]);

  const submit = async () => {
    const trimmed = newAlias.trim() || suggestAlias(aliases);
    setAliasError(null);
    const validationError = validateAlias(trimmed);
    if (validationError) {
      setAliasError(validationError);
      return;
    }
    try {
      await onSelectAlias(trimmed);
    } catch (e) {
      setAliasError(
        e instanceof ApiError ? e.envelope.message : (e instanceof Error ? e.message : String(e)),
      );
    }
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <div className="h-8 w-8 border-2 rounded-full animate-spin"
          style={{ borderColor: 'var(--pc-border)', borderTopColor: 'var(--pc-accent)' }} />
      </div>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      <AliasHelpBox what={oneTierAliasHelpLabel(sectionKey)} />
      {sectionKey === 'agents' && (
        <p className="text-sm" style={{ color: 'var(--pc-text-secondary)' }}>
          Create or choose the agent you want to chat with. The provider and
          profiles you already configured will be preselected when possible.
        </p>
      )}
      {error && (
        <div
          className="rounded-xl border p-3 text-sm"
          style={{ background: 'rgba(239,68,68,0.08)', borderColor: 'rgba(239,68,68,0.2)', color: '#f87171' }}
        >
          {error}
        </div>
      )}
      <div className="surface-panel divide-y" style={{ borderColor: 'var(--pc-border)' }}>
        {aliases.map((alias) => (
          <button
            key={alias}
            type="button"
            onClick={() => {
              onSelectAlias(alias).catch((e) => {
                setError(
                  e instanceof ApiError
                    ? `[${e.envelope.code}] ${e.envelope.message}`
                    : (e instanceof Error ? e.message : String(e)),
                );
              });
            }}
            className="w-full flex items-center justify-between gap-3 px-4 py-3 text-left text-sm transition-colors hover:opacity-90"
          >
            <div>
              <span style={{ color: 'var(--pc-text-primary)', fontWeight: 500 }}>{alias}</span>
              <code className="block text-xs mt-0.5" style={{ color: 'var(--pc-text-faint)' }}>
                {sectionKey}.{alias}
              </code>
            </div>
            <ChevronRight className="h-4 w-4 flex-shrink-0" style={{ color: 'var(--pc-text-muted)' }} />
          </button>
        ))}
        <div className="flex flex-col gap-1 px-4 py-3">
          <div className="flex items-center gap-2">
            <input
              type="text"
              className="input-electric flex-1 px-3 py-1.5 text-sm"
              placeholder={suggestAlias(aliases)}
              value={newAlias}
              onChange={(e) => { setNewAlias(e.target.value); setAliasError(null); }}
              onKeyDown={(e) => { if (e.key === 'Enter') void submit(); }}
              // eslint-disable-next-line jsx-a11y/no-autofocus
              autoFocus={aliases.length === 0}
            />
            <button type="button" onClick={() => void submit()} className="btn-electric text-sm px-3 py-1.5 flex-shrink-0">
              Create
            </button>
          </div>
          {aliasError && (
            <p className="text-xs" style={{ color: 'var(--color-status-error)' }}>{aliasError}</p>
          )}
        </div>
      </div>
    </div>
  );
}

function isLastSection(sections: SectionInfo[], key: string): boolean {
  return sections[sections.length - 1]?.key === key;
}

function suggestAlias(aliases: string[]): string {
  const used = new Set(aliases);
  if (!used.has('default')) return 'default';
  for (let i = 2; i < 100; i += 1) {
    const candidate = `default_${i}`;
    if (!used.has(candidate)) return candidate;
  }
  return 'default_100';
}

function validateAlias(alias: string): string | null {
  if (/^(?!_)(?!.*__)(?!.*_$)[a-z0-9_]{1,63}$/.test(alias)) return null;
  return 'Alias must use lowercase letters, digits, or single underscores only; no hyphens, dots, spaces, leading/trailing underscores, or double underscores.';
}

function aliasHelpText(what: string): string {
  const normalized = what.toLowerCase();
  if (normalized.includes('agent')) {
    return 'This names the assistant you will chat with. Most first-run setups only need one agent called default.';
  }
  if (normalized.includes('risk')) {
    return 'This names a reusable safety profile. Most first-run setups only need default; agents reference it later.';
  }
  if (normalized.includes('runtime')) {
    return 'This names a reusable runtime profile for tool limits, timeouts, and agent behavior. Most first-run setups only need default.';
  }
  if (normalized.includes('provider')) {
    return 'This names one provider credential or endpoint, such as default, work, or local. Agents reference it as provider.alias.';
  }
  if (normalized.includes('storage')) {
    return 'This names one backend instance. Most first-run setups only need one instance called default.';
  }
  if (normalized.includes('channel')) {
    return 'This names one channel connection. Agents can reference this channel alias later.';
  }
  return 'This names a reusable config entry. Most first-run setups only need default; add more aliases later when you need multiple entries.';
}

function typedAliasHelpLabel(sectionKey: string, typeLabel: string): string {
  switch (sectionKey) {
    case 'providers.models':
      return `${typeLabel} provider`;
    case 'providers.tts':
      return `${typeLabel} TTS provider`;
    case 'providers.transcription':
      return `${typeLabel} transcription provider`;
    case 'storage':
      return `${capitalize(typeLabel)} storage`;
    case 'channels':
      return `${typeLabel} channel`;
    default:
      return typeLabel;
  }
}

function oneTierAliasHelpLabel(sectionKey: string): string {
  switch (sectionKey) {
    case 'agents':
      return 'Agent';
    case 'risk-profiles':
      return 'Risk profile';
    case 'runtime-profiles':
      return 'Runtime profile';
    case 'skill-bundles':
      return 'Skill bundle';
    case 'mcp-bundles':
      return 'MCP bundle';
    case 'knowledge-bundles':
      return 'Knowledge bundle';
    case 'peer-groups':
      return 'Peer group';
    default:
      return 'Entry';
  }
}

function typedMapPathSegment(sectionKey: string, typeKey: string): string {
  return sectionKey.startsWith('providers.') ? typeKey.replace(/_/g, '-') : typeKey;
}

function parseCompleted(v: unknown): string[] {
  if (Array.isArray(v)) return v.filter((x): x is string => typeof x === 'string');
  if (typeof v !== 'string' || !v.length || v === '<unset>') return [];
  try {
    const parsed = JSON.parse(v);
    if (Array.isArray(parsed)) {
      return parsed.filter((x): x is string => typeof x === 'string');
    }
  } catch {
    // CLI-display fallback: comma-separated.
  }
  return v.split(',').map((s) => s.trim()).filter(Boolean);
}
