import { useState } from "react";
import { t, type Locale } from "../lib/i18n";

/// Scaffold for the "Tier A Typecast → Tier B/C offline secretary"
/// migration wizard (spec §11.3, PR #10).
///
/// The real voice-matching algorithm belongs in a Rust helper
/// (`src/voice/secretary_migrator.rs` — TODO in a follow-up PR). For
/// the MVP we surface a curated mapping table keyed by Typecast voice
/// ID so users who signed up for offline mode get a sensible first
/// suggestion without waiting on the full matching implementation.
///
/// The backend endpoint `/api/voices/secretary-suggest?from=<typecast_id>`
/// is stubbed to return the same mapping; this component can be
/// switched over to fetch() the moment that endpoint lands.

// ── Curated Typecast ID → offline engine recommendation ───────────────
//
// Sourced from the existing VoicePicker catalog + manual listening
// sessions by @chumyin. Rows marked `fallback: true` are the engine's
// default secretary when no specific Typecast voice was configured.

interface OfflineRecommendation {
  engine: "cosyvoice" | "kokoro";
  voiceId: string;
  displayName: string;
  rationale: string;
}

interface MappingRow {
  typecastId: string;
  typecastDisplayName: string;
  offline: OfflineRecommendation;
}

const MAPPING: readonly MappingRow[] = [
  {
    typecastId: "tc-kim-team-lead",
    typecastDisplayName: "김 팀장",
    offline: {
      engine: "cosyvoice",
      voiceId: "cv2-ko-adult-male-01",
      displayName: "박 대리 (오프라인 프로)",
      rationale: "동일 성별·연령대, 한국어 톤 가장 유사",
    },
  },
  {
    typecastId: "tc-park-lawyer",
    typecastDisplayName: "박 변호사",
    offline: {
      engine: "cosyvoice",
      voiceId: "cv2-ko-mid-male-formal",
      displayName: "정 실장 (오프라인 프로)",
      rationale: "전문가 톤·정중한 발화 스타일 매칭",
    },
  },
  {
    typecastId: "tc-emily-en",
    typecastDisplayName: "Emily",
    offline: {
      engine: "kokoro",
      voiceId: "kokoro-en-female-us-02",
      displayName: "Lydia (offline basic)",
      rationale: "Closest US-English female voice in the offline bundle",
    },
  },
  {
    typecastId: "tc-taro-ja",
    typecastDisplayName: "太郎",
    offline: {
      engine: "kokoro",
      voiceId: "kokoro-ja-male-01",
      displayName: "健二 (オフライン)",
      rationale: "同年代・同性別の日本語音声",
    },
  },
];

// Default when nothing matches — Kokoro is the 82M-param model that is
// bundled with every MoA install, so it is always available.
const DEFAULT_FALLBACK: OfflineRecommendation = {
  engine: "kokoro",
  voiceId: "kokoro-ko-female-default",
  displayName: "민지 (오프라인 기본)",
  rationale: "바로 사용 가능한 오프라인 한국어 비서",
};

function lookupMapping(typecastId: string | null): OfflineRecommendation {
  if (!typecastId) return DEFAULT_FALLBACK;
  return MAPPING.find((row) => row.typecastId === typecastId)?.offline ?? DEFAULT_FALLBACK;
}

function currentTypecastIdFromStorage(): string | null {
  // VoicePicker persists the selected voice under this key (see
  // clients/tauri/src/components/VoicePicker.tsx). When the user has
  // never chosen a Typecast voice, this returns null and the
  // recommendation falls back to the Kokoro default.
  return localStorage.getItem("moa_typecast_voice_id");
}

function rememberOfflinePreference(rec: OfflineRecommendation): void {
  localStorage.setItem("moa_offline_secretary_engine", rec.engine);
  localStorage.setItem("moa_offline_secretary_voice_id", rec.voiceId);
}

interface Props {
  locale: Locale;
  /// Optional override — when passed, skip localStorage and show a
  /// recommendation for this specific Typecast voice (used by the
  /// Settings → Voice tab's "migrate to offline" link).
  typecastId?: string;
  onDone?: () => void;
}

export function SecretaryMigrator({ locale, typecastId, onDone }: Props) {
  const resolvedFrom = typecastId ?? currentTypecastIdFromStorage();
  const recommendation = lookupMapping(resolvedFrom);
  const [accepted, setAccepted] = useState<boolean>(false);

  const currentLabel =
    MAPPING.find((r) => r.typecastId === resolvedFrom)?.typecastDisplayName ??
    t("secretary_migrator_no_typecast_voice", locale);

  const handleAccept = () => {
    rememberOfflinePreference(recommendation);
    setAccepted(true);
    onDone?.();
  };

  return (
    <section className="settings-section">
      <h3>{t("secretary_migrator_title", locale)}</h3>
      <p className="settings-description">{t("secretary_migrator_intro", locale)}</p>

      <div className="settings-row">
        <label>{t("secretary_migrator_current", locale)}</label>
        <span>{currentLabel}</span>
      </div>

      <div className="settings-row">
        <label>{t("secretary_migrator_recommended", locale)}</label>
        <span>
          {recommendation.displayName}
          {" · "}
          <em>{engineLabel(recommendation.engine, locale)}</em>
        </span>
      </div>

      <div className="settings-row">
        <label>{t("secretary_migrator_reason", locale)}</label>
        <span>{recommendation.rationale}</span>
      </div>

      <div className="settings-row">
        {accepted ? (
          <span className="settings-badge settings-badge--ok">
            {t("secretary_migrator_saved", locale)}
          </span>
        ) : (
          <button type="button" onClick={handleAccept}>
            {t("secretary_migrator_accept", locale)}
          </button>
        )}
      </div>
    </section>
  );
}

function engineLabel(engine: OfflineRecommendation["engine"], locale: Locale): string {
  if (engine === "cosyvoice") return t("secretary_engine_cosyvoice", locale);
  return t("secretary_engine_kokoro", locale);
}
