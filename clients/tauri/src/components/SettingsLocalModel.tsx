import { useCallback, useEffect, useState } from "react";
import { apiClient } from "../lib/api";
import { t, type Locale } from "../lib/i18n";

// ── Types matching src/gateway/local_llm_api.rs ───────────────────────

interface GpuType {
  Nvidia?: { name: string; vram_mb: number };
  Amd?: { name: string; vram_mb: number };
  AppleSilicon?: { chip: string };
  Integrated?: Record<string, never>;
  None?: Record<string, never>;
}

interface HardwareProfile {
  os: string;
  arch: string;
  total_ram_mb: number;
  available_ram_mb: number;
  cpu_cores: number;
  gpu: GpuType;
  disk_free_mb: number;
  recommended_tier: "T1E2B" | "T2E4B" | "T3MoE26B" | "T4Dense31B";
  downgraded: boolean;
  probed_at: string;
}

interface InstalledModel {
  name: string;
  size_bytes: number;
  modified_at: string;
}

interface StatusResponse {
  default_model: string | null;
  daemon_running: boolean;
  model_installed: boolean;
  hardware: HardwareProfile | null;
  installed_models: InstalledModel[];
  offline_force_local: boolean;
  primary_cloud: string | null;
}

interface ReprobeResponse {
  profile: HardwareProfile;
  downgraded: boolean;
  persisted: boolean;
}

// ── Tier catalog (kept in the component so the dropdown renders even
//    when /status is down) ────────────────────────────────────────────

type TierKey = HardwareProfile["recommended_tier"];

interface TierSpec {
  key: TierKey;
  tag: string;
  displayName: string;
  downloadGb: number;
  memoryGb: number;
  supportsAudio: boolean;
  // Minimum effective memory (GB) required to run this tier on desktop —
  // aligns with `host_probe::compute_effective_memory_gb` boundaries.
  minEffectiveMemoryGb: number;
}

const TIERS: readonly TierSpec[] = [
  {
    key: "T1E2B",
    tag: "gemma4:e2b",
    displayName: "T1 Minimum · Gemma 4 E2B",
    downloadGb: 2.0,
    memoryGb: 4.0,
    supportsAudio: true,
    minEffectiveMemoryGb: 0,
  },
  {
    key: "T2E4B",
    tag: "gemma4:e4b",
    displayName: "T2 Standard · Gemma 4 E4B",
    downloadGb: 3.0,
    memoryGb: 5.5,
    supportsAudio: true,
    // E4B needs ~5.5 GB at Q4_K_M. Setting the gate to 5.5 (exact
    // requirement, not a buffer over it) means M1 8 GB machines with
    // our 0.75 unified-memory factor = 6 GB effective — just above
    // the line. Real hardware reports confirm E4B runs there (slow
    // but usable). If future quantization shrinks the model, lower
    // this; if we see OOM crashes in the wild, raise it.
    minEffectiveMemoryGb: 5.5,
  },
  {
    key: "T3MoE26B",
    tag: "gemma4:26b",
    displayName: "T3 High-perf · Gemma 4 26B MoE",
    downloadGb: 8.0,
    memoryGb: 15.0,
    supportsAudio: false,
    minEffectiveMemoryGb: 10,
  },
  {
    key: "T4Dense31B",
    tag: "gemma4:31b",
    displayName: "T4 Workstation · Gemma 4 31B Dense",
    downloadGb: 10.0,
    memoryGb: 17.0,
    supportsAudio: false,
    minEffectiveMemoryGb: 20,
  },
];

const AUTO_UPGRADE_PREF_KEY = "moa_local_llm_auto_upgrade_notify";

// ── Helpers ───────────────────────────────────────────────────────────

function effectiveMemoryGb(hw: HardwareProfile | null): number {
  if (!hw) return 0;
  const nvidia = hw.gpu.Nvidia;
  const amd = hw.gpu.Amd;
  if (nvidia) return nvidia.vram_mb / 1024;
  if (amd) return amd.vram_mb / 1024;
  if (hw.gpu.AppleSilicon) {
    // Apple Silicon unified memory: macOS can dynamically allocate up
    // to ~75% of RAM to the GPU on demand (increased from 70% in prior
    // versions). Using 0.75 lets M1 8 GB users unlock T2E4B (5.5 GB
    // effective memory required, with 0.75 factor: 6 GB — right at the
    // minimum for T2E4B which works in practice per field reports).
    // Go higher and we risk OOM under concurrent load; stay lower and
    // M1 8 GB users get stuck on T1E2B despite E4B being usable.
    return (hw.total_ram_mb / 1024) * 0.75;
  }
  // iGPU or no GPU: system RAM minus OS overhead.
  return Math.max(0, hw.total_ram_mb / 1024 - 4);
}

function gpuSummary(hw: HardwareProfile | null): string {
  if (!hw) return "—";
  if (hw.gpu.Nvidia) return `NVIDIA ${hw.gpu.Nvidia.name} (${hw.gpu.Nvidia.vram_mb} MB VRAM)`;
  if (hw.gpu.Amd) return `AMD ${hw.gpu.Amd.name} (${hw.gpu.Amd.vram_mb} MB VRAM)`;
  if (hw.gpu.AppleSilicon) return `${hw.gpu.AppleSilicon.chip} (unified memory)`;
  if (hw.gpu.Integrated) return "Integrated GPU";
  return "No dedicated GPU";
}

function isMobileOs(os: string): boolean {
  return os === "iOS" || os === "Android";
}

function formatBytes(n: number): string {
  const gb = n / (1024 * 1024 * 1024);
  return `${gb.toFixed(2)} GB`;
}

// ── Component ─────────────────────────────────────────────────────────

interface Props {
  locale: Locale;
}

export function SettingsLocalModel({ locale }: Props) {
  const [status, setStatus] = useState<StatusResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [autoUpgradeNotify, setAutoUpgradeNotify] = useState<boolean>(
    () => localStorage.getItem(AUTO_UPGRADE_PREF_KEY) !== "off",
  );

  const baseUrl = apiClient.getServerUrl();

  const refresh = useCallback(async () => {
    setError(null);
    try {
      const resp = await fetch(`${baseUrl}/api/local-llm/status`);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const body = (await resp.json()) as StatusResponse;
      setStatus(body);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, [baseUrl]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleReprobe = useCallback(async () => {
    setBusy("reprobe");
    setError(null);
    try {
      const resp = await fetch(`${baseUrl}/api/local-llm/reprobe`, { method: "POST" });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      const body = (await resp.json()) as ReprobeResponse;
      setStatus((prev: StatusResponse | null) =>
        prev
          ? {
              ...prev,
              hardware: body.profile,
            }
          : prev,
      );
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [baseUrl]);

  const handleToggleOfflineOnly = useCallback(async () => {
    if (!status) return;
    setBusy("offline");
    const next = !status.offline_force_local;
    try {
      const resp = await fetch(`${baseUrl}/api/local-llm/offline-only`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ enabled: next }),
      });
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      setStatus({ ...status, offline_force_local: next });
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(null);
    }
  }, [baseUrl, status]);

  const handleUninstall = useCallback(
    async (tag?: string) => {
      if (!confirm(t("local_llm_uninstall_confirm", locale))) return;
      setBusy("uninstall");
      try {
        const resp = await fetch(`${baseUrl}/api/local-llm/uninstall`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ tag }),
        });
        if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
        await refresh();
      } catch (e) {
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        setBusy(null);
      }
    },
    [baseUrl, locale, refresh],
  );

  const handleAutoUpgradeToggle = useCallback(() => {
    setAutoUpgradeNotify((prev: boolean) => {
      const next = !prev;
      localStorage.setItem(AUTO_UPGRADE_PREF_KEY, next ? "on" : "off");
      return next;
    });
  }, []);

  const handleSwitchTier = useCallback(
    (targetTag: string) => {
      // The actual re-install runs via `zeroclaw setup local-llm --tier X`;
      // this UI exposes it by firing the setup endpoint once wired. For
      // now we surface the command users can run manually.
      alert(
        t("local_llm_switch_manual_hint", locale).replace(
          "{command}",
          `zeroclaw setup local-llm --tier ${targetTag.replace("gemma4:", "")}`,
        ),
      );
    },
    [locale],
  );

  if (loading) {
    return (
      <section className="settings-section">
        <h3>{t("local_llm_section_title", locale)}</h3>
        <p>{t("local_llm_loading", locale)}</p>
      </section>
    );
  }

  const hw = status?.hardware ?? null;
  const effMem = effectiveMemoryGb(hw);
  const mobile = hw ? isMobileOs(hw.os) : false;

  return (
    <section className="settings-section">
      <h3>{t("local_llm_section_title", locale)}</h3>

      {error && (
        <div role="alert" className="settings-error">
          {error}
        </div>
      )}

      {/* Current model */}
      <div className="settings-row">
        <label>{t("local_llm_current_model", locale)}</label>
        <span>
          {status?.default_model ?? t("local_llm_not_installed", locale)}
          {status?.daemon_running ? (
            status.model_installed ? (
              <span className="settings-badge settings-badge--ok">
                {" "}
                · {t("local_llm_status_ok", locale)}
              </span>
            ) : (
              <span className="settings-badge settings-badge--warn">
                {" "}
                · {t("local_llm_status_pull_pending", locale)}
              </span>
            )
          ) : (
            <span className="settings-badge settings-badge--error">
              {" "}
              · {t("local_llm_daemon_down", locale)}
            </span>
          )}
        </span>
      </div>

      {/* Tier switch dropdown */}
      <div className="settings-row">
        <label htmlFor="local-llm-tier">{t("local_llm_change_tier", locale)}</label>
        <select
          id="local-llm-tier"
          value={status?.default_model ?? ""}
          onChange={(e: { target: { value: string } }) => handleSwitchTier(e.target.value)}
          disabled={busy !== null}
        >
          <option value="" disabled>
            {t("local_llm_pick_tier", locale)}
          </option>
          {TIERS.map((tier) => {
            // Mobile cap: never offer T3/T4 on iOS/Android.
            const disallowedOnMobile =
              mobile && (tier.key === "T3MoE26B" || tier.key === "T4Dense31B");
            const tooLittleMemory = effMem > 0 && effMem < tier.minEffectiveMemoryGb;
            const disabled = disallowedOnMobile || tooLittleMemory;
            const reason = disallowedOnMobile
              ? t("local_llm_mobile_cap", locale)
              : tooLittleMemory
                ? t("local_llm_insufficient_vram", locale).replace(
                    "{need}",
                    tier.memoryGb.toFixed(1),
                  )
                : "";
            return (
              <option key={tier.key} value={tier.tag} disabled={disabled} title={reason}>
                {tier.displayName} · ~{tier.downloadGb.toFixed(1)} GB
                {disabled ? ` — ${reason}` : ""}
                {tier.supportsAudio ? ` · ${t("local_llm_audio_native", locale)}` : ""}
              </option>
            );
          })}
        </select>
      </div>

      {/* Hardware re-probe */}
      <div className="settings-row">
        <label>{t("local_llm_hardware_summary", locale)}</label>
        <span>
          {hw ? (
            <>
              {hw.os} / {hw.arch} · {gpuSummary(hw)} · {t("local_llm_ram", locale)}{" "}
              {(hw.total_ram_mb / 1024).toFixed(1)} GB · {t("local_llm_disk_free", locale)}{" "}
              {(hw.disk_free_mb / 1024).toFixed(1)} GB
            </>
          ) : (
            t("local_llm_hardware_unknown", locale)
          )}
        </span>
      </div>
      <div className="settings-row">
        <button
          type="button"
          onClick={handleReprobe}
          disabled={busy !== null}
        >
          {busy === "reprobe"
            ? t("local_llm_reprobing", locale)
            : t("local_llm_reprobe", locale)}
        </button>
      </div>

      {/* Auto-upgrade-notify toggle */}
      <div className="settings-row">
        <label>
          <input
            type="checkbox"
            checked={autoUpgradeNotify}
            onChange={handleAutoUpgradeToggle}
          />{" "}
          {t("local_llm_auto_upgrade_notify", locale)}
        </label>
      </div>

      {/* Offline-only toggle */}
      <div className="settings-row">
        <label>
          <input
            type="checkbox"
            checked={status?.offline_force_local ?? false}
            onChange={handleToggleOfflineOnly}
            disabled={busy !== null}
          />{" "}
          {t("local_llm_offline_only", locale)}
        </label>
      </div>

      {/* Model uninstall */}
      <div className="settings-row">
        <label>{t("local_llm_uninstall_label", locale)}</label>
        <button
          type="button"
          className="settings-button--danger"
          onClick={() => handleUninstall()}
          disabled={busy !== null || !status?.default_model}
        >
          {busy === "uninstall"
            ? t("local_llm_uninstalling", locale)
            : t("local_llm_uninstall", locale)}
        </button>
      </div>

      {/* Installed inventory (collapsed detail) */}
      {status && status.installed_models.length > 0 && (
        <details className="settings-details">
          <summary>
            {t("local_llm_installed_inventory", locale).replace(
              "{count}",
              String(status.installed_models.length),
            )}
          </summary>
          <ul>
            {status.installed_models.map((m: InstalledModel) => (
              <li key={m.name}>
                {m.name} — {formatBytes(m.size_bytes)}
              </li>
            ))}
          </ul>
        </details>
      )}
    </section>
  );
}
