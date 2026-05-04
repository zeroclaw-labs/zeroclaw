//! Voice processing pipeline: real-time voice interpretation and conversation.
//!
//! Implements the voice provider trait, language detection, session management,
//! and billing integration for voice sessions.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

// ── Language codes (75 supported languages) ──────────────────────
//
// History
//   * 2026-04: shipped with 25 languages — the ones with reliable
//     Unicode-script auto-detection plus the most common Latin-script
//     European pairs.
//   * 2026-05: extended to 75 in two batches:
//       - new auto-detectable scripts (Bengali, Tamil, Telugu, …,
//         Hebrew, Greek, Armenian, Georgian, Burmese, Khmer, Lao,
//         Sinhala) — both `LanguageCode` and `detect_language`
//         updated together.
//       - common Latin-script languages where the user must select
//         the language explicitly (auto-detection cannot reliably
//         distinguish e.g. Polish from Czech from Slovak by script
//         alone).
//
// Why an enum instead of a String tag
// -----------------------------------
// The compiler-checked `match` exhaustiveness on every call site
// (`as_str`, `display_name`, `lang_to_typecast_iso3`, the Deepgram
// mapping, the chat handler) is the safety net that has caught every
// language-related bug in this codebase. Switching to a String tag
// would lose that net. So we accept the verbosity of N variants and
// the per-site `match` arms in exchange for "the build breaks if
// anyone forgets to map a new language".

/// ISO 639-1 language codes supported by the voice pipeline.
///
/// Every variant must be mapped in:
///   * [`LanguageCode::as_str`]
///   * [`LanguageCode::display_name`]
///   * [`LanguageCode::from_str_code`]
///   * [`LanguageCode::all`]
///   * `crate::voice::typecast_interp::lang_to_typecast_iso3`
///   * `crate::voice::voice_messages` (defaults to English fallback
///     for unknown variants — see that module for the small set
///     with native translations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LanguageCode {
    // ── East Asia ──
    Ko,   // Korean
    Ja,   // Japanese
    Zh,   // Chinese (Simplified)
    ZhTw, // Chinese (Traditional)
    Mn,   // Mongolian (Cyrillic; the script-detector currently maps
          //  Mongolian Cyrillic text to `Ru` because both share the
          //  Cyrillic block — users on Mongolian should pick this
          //  explicitly when starting a session)

    // ── Southeast Asia ──
    Th, // Thai
    Vi, // Vietnamese
    Id, // Indonesian
    Ms, // Malay
    Tl, // Filipino / Tagalog
    My, // Burmese / Myanmar
    Km, // Khmer
    Lo, // Lao

    // ── South Asia ──
    Hi, // Hindi
    Bn, // Bengali / Bangla
    Ta, // Tamil
    Te, // Telugu
    Mr, // Marathi
    Gu, // Gujarati
    Kn, // Kannada
    Ml, // Malayalam
    Pa, // Punjabi (Gurmukhi)
    Or, // Odia / Oriya
    Si, // Sinhala
    Ur, // Urdu (Arabic script — auto-detector returns `Ar`; pick
        //  this explicitly for Urdu prompts)
    Ne, // Nepali
    Sd, // Sindhi

    // ── Europe (Western, Latin script) ──
    En, // English
    Es, // Spanish
    Fr, // French
    De, // German
    It, // Italian
    Pt, // Portuguese
    Nl, // Dutch
    Pl, // Polish
    Cs, // Czech
    Sv, // Swedish
    Da, // Danish
    No, // Norwegian (Bokmål; `nb` callers map here)
    Fi, // Finnish
    Is, // Icelandic
    Ga, // Irish (Gaeilge)
    Cy, // Welsh
    Mt, // Maltese
    Eu, // Basque (Euskara)
    Ca, // Catalan
    Gl, // Galician

    // ── Europe (Central / Southeastern, Latin script) ──
    Hu, // Hungarian
    Ro, // Romanian
    Sk, // Slovak
    Sl, // Slovene / Slovenian
    Hr, // Croatian
    Sr, // Serbian (auto-detector returns `Ru` for Cyrillic Serbian;
        //  pick this explicitly for either Latin or Cyrillic Serbian)
    Bs, // Bosnian
    Sq, // Albanian
    Et, // Estonian
    Lv, // Latvian
    Lt, // Lithuanian

    // ── Europe (East Slavic, Cyrillic) ──
    Ru, // Russian
    Uk, // Ukrainian
    Be, // Belarusian
    Bg, // Bulgarian
    Mk, // Macedonian

    // ── Europe (other scripts) ──
    El, // Greek
    Hy, // Armenian
    Ka, // Georgian
    Tr, // Turkish (Latin since 1928 — auto-detector cannot
        //  distinguish from other Latin scripts)

    // ── Middle East ──
    Ar, // Arabic
    He, // Hebrew
    Fa, // Persian / Farsi (Arabic script)

    // ── Central Asia ──
    Kk, // Kazakh
    Uz, // Uzbek
    Az, // Azerbaijani

    // ── Africa ──
    Sw, // Swahili
    Am, // Amharic
    Yo, // Yoruba
    Ha, // Hausa
    Zu, // Zulu
    Af, // Afrikaans
    So, // Somali
}

impl LanguageCode {
    /// Get the ISO 639-1 code string. Stable wire format — channel,
    /// gateway, and Tauri client all serialize the language as this
    /// string and parse it back via [`LanguageCode::from_str_code`].
    pub fn as_str(self) -> &'static str {
        match self {
            // East Asia
            Self::Ko => "ko",
            Self::Ja => "ja",
            Self::Zh => "zh",
            Self::ZhTw => "zh-TW",
            Self::Mn => "mn",
            // Southeast Asia
            Self::Th => "th",
            Self::Vi => "vi",
            Self::Id => "id",
            Self::Ms => "ms",
            Self::Tl => "tl",
            Self::My => "my",
            Self::Km => "km",
            Self::Lo => "lo",
            // South Asia
            Self::Hi => "hi",
            Self::Bn => "bn",
            Self::Ta => "ta",
            Self::Te => "te",
            Self::Mr => "mr",
            Self::Gu => "gu",
            Self::Kn => "kn",
            Self::Ml => "ml",
            Self::Pa => "pa",
            Self::Or => "or",
            Self::Si => "si",
            Self::Ur => "ur",
            Self::Ne => "ne",
            Self::Sd => "sd",
            // Europe — Western Latin
            Self::En => "en",
            Self::Es => "es",
            Self::Fr => "fr",
            Self::De => "de",
            Self::It => "it",
            Self::Pt => "pt",
            Self::Nl => "nl",
            Self::Pl => "pl",
            Self::Cs => "cs",
            Self::Sv => "sv",
            Self::Da => "da",
            Self::No => "no",
            Self::Fi => "fi",
            Self::Is => "is",
            Self::Ga => "ga",
            Self::Cy => "cy",
            Self::Mt => "mt",
            Self::Eu => "eu",
            Self::Ca => "ca",
            Self::Gl => "gl",
            // Europe — Central / Southeastern Latin
            Self::Hu => "hu",
            Self::Ro => "ro",
            Self::Sk => "sk",
            Self::Sl => "sl",
            Self::Hr => "hr",
            Self::Sr => "sr",
            Self::Bs => "bs",
            Self::Sq => "sq",
            Self::Et => "et",
            Self::Lv => "lv",
            Self::Lt => "lt",
            // Europe — East Slavic Cyrillic
            Self::Ru => "ru",
            Self::Uk => "uk",
            Self::Be => "be",
            Self::Bg => "bg",
            Self::Mk => "mk",
            // Europe — other scripts
            Self::El => "el",
            Self::Hy => "hy",
            Self::Ka => "ka",
            Self::Tr => "tr",
            // Middle East
            Self::Ar => "ar",
            Self::He => "he",
            Self::Fa => "fa",
            // Central Asia
            Self::Kk => "kk",
            Self::Uz => "uz",
            Self::Az => "az",
            // Africa
            Self::Sw => "sw",
            Self::Am => "am",
            Self::Yo => "yo",
            Self::Ha => "ha",
            Self::Zu => "zu",
            Self::Af => "af",
            Self::So => "so",
        }
    }

    /// Get the human-readable language name. Stable English label
    /// for logging / debugging; the user-facing UI should localize
    /// from `as_str()` instead.
    pub fn display_name(self) -> &'static str {
        match self {
            // East Asia
            Self::Ko => "Korean",
            Self::Ja => "Japanese",
            Self::Zh => "Chinese (Simplified)",
            Self::ZhTw => "Chinese (Traditional)",
            Self::Mn => "Mongolian",
            // Southeast Asia
            Self::Th => "Thai",
            Self::Vi => "Vietnamese",
            Self::Id => "Indonesian",
            Self::Ms => "Malay",
            Self::Tl => "Filipino",
            Self::My => "Burmese",
            Self::Km => "Khmer",
            Self::Lo => "Lao",
            // South Asia
            Self::Hi => "Hindi",
            Self::Bn => "Bengali",
            Self::Ta => "Tamil",
            Self::Te => "Telugu",
            Self::Mr => "Marathi",
            Self::Gu => "Gujarati",
            Self::Kn => "Kannada",
            Self::Ml => "Malayalam",
            Self::Pa => "Punjabi",
            Self::Or => "Odia",
            Self::Si => "Sinhala",
            Self::Ur => "Urdu",
            Self::Ne => "Nepali",
            Self::Sd => "Sindhi",
            // Europe — Western Latin
            Self::En => "English",
            Self::Es => "Spanish",
            Self::Fr => "French",
            Self::De => "German",
            Self::It => "Italian",
            Self::Pt => "Portuguese",
            Self::Nl => "Dutch",
            Self::Pl => "Polish",
            Self::Cs => "Czech",
            Self::Sv => "Swedish",
            Self::Da => "Danish",
            Self::No => "Norwegian",
            Self::Fi => "Finnish",
            Self::Is => "Icelandic",
            Self::Ga => "Irish",
            Self::Cy => "Welsh",
            Self::Mt => "Maltese",
            Self::Eu => "Basque",
            Self::Ca => "Catalan",
            Self::Gl => "Galician",
            // Europe — Central / Southeastern Latin
            Self::Hu => "Hungarian",
            Self::Ro => "Romanian",
            Self::Sk => "Slovak",
            Self::Sl => "Slovenian",
            Self::Hr => "Croatian",
            Self::Sr => "Serbian",
            Self::Bs => "Bosnian",
            Self::Sq => "Albanian",
            Self::Et => "Estonian",
            Self::Lv => "Latvian",
            Self::Lt => "Lithuanian",
            // Europe — East Slavic Cyrillic
            Self::Ru => "Russian",
            Self::Uk => "Ukrainian",
            Self::Be => "Belarusian",
            Self::Bg => "Bulgarian",
            Self::Mk => "Macedonian",
            // Europe — other scripts
            Self::El => "Greek",
            Self::Hy => "Armenian",
            Self::Ka => "Georgian",
            Self::Tr => "Turkish",
            // Middle East
            Self::Ar => "Arabic",
            Self::He => "Hebrew",
            Self::Fa => "Persian",
            // Central Asia
            Self::Kk => "Kazakh",
            Self::Uz => "Uzbek",
            Self::Az => "Azerbaijani",
            // Africa
            Self::Sw => "Swahili",
            Self::Am => "Amharic",
            Self::Yo => "Yoruba",
            Self::Ha => "Hausa",
            Self::Zu => "Zulu",
            Self::Af => "Afrikaans",
            Self::So => "Somali",
        }
    }

    /// Parse from string code (case-insensitive). Accepts ISO 639-1
    /// shorts (`ko`, `ja`, …) plus a handful of common aliases
    /// (`zh-tw`/`zh_tw`, `nb` → `No`, `iw` → `He`, `in` → `Id`,
    /// `ji` → `Yo` is NOT done — Yoruba is `yo` only).
    pub fn from_str_code(code: &str) -> Option<Self> {
        match code.to_lowercase().as_str() {
            // East Asia
            "ko" => Some(Self::Ko),
            "ja" => Some(Self::Ja),
            "zh" | "zh-cn" | "zh_cn" => Some(Self::Zh),
            "zh-tw" | "zh_tw" | "zh-hant" => Some(Self::ZhTw),
            "mn" => Some(Self::Mn),
            // Southeast Asia
            "th" => Some(Self::Th),
            "vi" => Some(Self::Vi),
            "id" | "in" => Some(Self::Id),
            "ms" => Some(Self::Ms),
            "tl" | "fil" => Some(Self::Tl),
            "my" => Some(Self::My),
            "km" => Some(Self::Km),
            "lo" => Some(Self::Lo),
            // South Asia
            "hi" => Some(Self::Hi),
            "bn" => Some(Self::Bn),
            "ta" => Some(Self::Ta),
            "te" => Some(Self::Te),
            "mr" => Some(Self::Mr),
            "gu" => Some(Self::Gu),
            "kn" => Some(Self::Kn),
            "ml" => Some(Self::Ml),
            "pa" => Some(Self::Pa),
            "or" => Some(Self::Or),
            "si" => Some(Self::Si),
            "ur" => Some(Self::Ur),
            "ne" => Some(Self::Ne),
            "sd" => Some(Self::Sd),
            // Europe — Western Latin
            "en" => Some(Self::En),
            "es" => Some(Self::Es),
            "fr" => Some(Self::Fr),
            "de" => Some(Self::De),
            "it" => Some(Self::It),
            "pt" => Some(Self::Pt),
            "nl" => Some(Self::Nl),
            "pl" => Some(Self::Pl),
            "cs" => Some(Self::Cs),
            "sv" => Some(Self::Sv),
            "da" => Some(Self::Da),
            "no" | "nb" | "nn" => Some(Self::No),
            "fi" => Some(Self::Fi),
            "is" => Some(Self::Is),
            "ga" => Some(Self::Ga),
            "cy" => Some(Self::Cy),
            "mt" => Some(Self::Mt),
            "eu" => Some(Self::Eu),
            "ca" => Some(Self::Ca),
            "gl" => Some(Self::Gl),
            // Europe — Central / Southeastern Latin
            "hu" => Some(Self::Hu),
            "ro" => Some(Self::Ro),
            "sk" => Some(Self::Sk),
            "sl" => Some(Self::Sl),
            "hr" => Some(Self::Hr),
            "sr" => Some(Self::Sr),
            "bs" => Some(Self::Bs),
            "sq" => Some(Self::Sq),
            "et" => Some(Self::Et),
            "lv" => Some(Self::Lv),
            "lt" => Some(Self::Lt),
            // Europe — East Slavic Cyrillic
            "ru" => Some(Self::Ru),
            "uk" => Some(Self::Uk),
            "be" => Some(Self::Be),
            "bg" => Some(Self::Bg),
            "mk" => Some(Self::Mk),
            // Europe — other scripts
            "el" => Some(Self::El),
            "hy" => Some(Self::Hy),
            "ka" => Some(Self::Ka),
            "tr" => Some(Self::Tr),
            // Middle East
            "ar" => Some(Self::Ar),
            "he" | "iw" => Some(Self::He),
            "fa" => Some(Self::Fa),
            // Central Asia
            "kk" => Some(Self::Kk),
            "uz" => Some(Self::Uz),
            "az" => Some(Self::Az),
            // Africa
            "sw" => Some(Self::Sw),
            "am" => Some(Self::Am),
            "yo" => Some(Self::Yo),
            "ha" => Some(Self::Ha),
            "zu" => Some(Self::Zu),
            "af" => Some(Self::Af),
            "so" => Some(Self::So),
            _ => None,
        }
    }

    /// Return every supported language code, in stable order matching
    /// the variant declaration. Used by tests to enforce coverage of
    /// each enum arm in helper functions.
    pub fn all() -> &'static [LanguageCode] {
        &[
            // East Asia
            Self::Ko,
            Self::Ja,
            Self::Zh,
            Self::ZhTw,
            Self::Mn,
            // Southeast Asia
            Self::Th,
            Self::Vi,
            Self::Id,
            Self::Ms,
            Self::Tl,
            Self::My,
            Self::Km,
            Self::Lo,
            // South Asia
            Self::Hi,
            Self::Bn,
            Self::Ta,
            Self::Te,
            Self::Mr,
            Self::Gu,
            Self::Kn,
            Self::Ml,
            Self::Pa,
            Self::Or,
            Self::Si,
            Self::Ur,
            Self::Ne,
            Self::Sd,
            // Europe — Western Latin
            Self::En,
            Self::Es,
            Self::Fr,
            Self::De,
            Self::It,
            Self::Pt,
            Self::Nl,
            Self::Pl,
            Self::Cs,
            Self::Sv,
            Self::Da,
            Self::No,
            Self::Fi,
            Self::Is,
            Self::Ga,
            Self::Cy,
            Self::Mt,
            Self::Eu,
            Self::Ca,
            Self::Gl,
            // Europe — Central / Southeastern Latin
            Self::Hu,
            Self::Ro,
            Self::Sk,
            Self::Sl,
            Self::Hr,
            Self::Sr,
            Self::Bs,
            Self::Sq,
            Self::Et,
            Self::Lv,
            Self::Lt,
            // Europe — East Slavic Cyrillic
            Self::Ru,
            Self::Uk,
            Self::Be,
            Self::Bg,
            Self::Mk,
            // Europe — other scripts
            Self::El,
            Self::Hy,
            Self::Ka,
            Self::Tr,
            // Middle East
            Self::Ar,
            Self::He,
            Self::Fa,
            // Central Asia
            Self::Kk,
            Self::Uz,
            Self::Az,
            // Africa
            Self::Sw,
            Self::Am,
            Self::Yo,
            Self::Ha,
            Self::Zu,
            Self::Af,
            Self::So,
        ]
    }
}

// ── Language detection via Unicode ranges ─────────────────────────

/// Detect the most likely language from text using Unicode character ranges.
///
/// Auto-detection only resolves languages with a distinct script
/// (Hangul → Ko, Hiragana/Katakana → Ja, CJK → Zh, Devanagari → Hi,
/// Bengali → Bn, Tamil → Ta, Hebrew → He, Greek → El, …). Latin-script
/// languages and Cyrillic-script non-Russian languages cannot be
/// distinguished from each other by script alone — the function
/// returns `default` for those, and the caller is expected to honor
/// the user's explicit language selection.
///
/// Caveats worth remembering when reading the call sites:
///   * Mongolian written in Cyrillic returns `Ru` here.
///   * Serbian written in Cyrillic returns `Ru` too.
///   * Urdu (Arabic script) and Persian both return `Ar`.
/// The voice-chat self-validation pipeline uses the detected language
/// to pick the right re-ask phrasing — those mis-attributions will
/// land the user on the Russian / Arabic re-ask string. That is wrong
/// in the strict sense, but better than no re-ask, and the user can
/// override by explicitly setting source_language in the session.
pub fn detect_language(text: &str, default: LanguageCode) -> LanguageCode {
    if text.is_empty() {
        return default;
    }

    let mut hangul = 0u32;
    let mut kana = 0u32;
    let mut cjk = 0u32;
    let mut arabic = 0u32;
    let mut thai = 0u32;
    let mut devanagari = 0u32;
    let mut cyrillic = 0u32;
    let mut hebrew = 0u32;
    let mut greek = 0u32;
    let mut armenian = 0u32;
    let mut georgian = 0u32;
    let mut bengali = 0u32;
    let mut gurmukhi = 0u32;
    let mut gujarati = 0u32;
    let mut tamil = 0u32;
    let mut telugu = 0u32;
    let mut kannada = 0u32;
    let mut malayalam = 0u32;
    let mut sinhala = 0u32;
    let mut burmese = 0u32;
    let mut khmer = 0u32;
    let mut lao = 0u32;
    let mut ethiopic = 0u32;
    let mut total = 0u32;

    for c in text.chars() {
        if c.is_whitespace() || c.is_ascii_punctuation() {
            continue;
        }
        total += 1;

        match c as u32 {
            // ── Existing scripts (unchanged 2026-04 behavior) ──
            0xAC00..=0xD7AF | 0x1100..=0x11FF | 0x3130..=0x318F => hangul += 1,
            0x3040..=0x309F | 0x30A0..=0x30FF | 0x31F0..=0x31FF => kana += 1,
            0x4E00..=0x9FFF | 0x3400..=0x4DBF => cjk += 1,
            0x0600..=0x06FF | 0x0750..=0x077F | 0x08A0..=0x08FF => arabic += 1,
            0x0E00..=0x0E7F => thai += 1,
            0x0900..=0x097F => devanagari += 1,
            0x0400..=0x052F => cyrillic += 1,
            // ── New scripts (2026-05 expansion) ──
            // Hebrew
            0x0590..=0x05FF => hebrew += 1,
            // Greek + Coptic
            0x0370..=0x03FF | 0x1F00..=0x1FFF => greek += 1,
            // Armenian
            0x0530..=0x058F => armenian += 1,
            // Georgian (modern Mkhedruli + Mtavruli)
            0x10A0..=0x10FF | 0x1C90..=0x1CBF | 0x2D00..=0x2D2F => georgian += 1,
            // Bengali / Bangla
            0x0980..=0x09FF => bengali += 1,
            // Gurmukhi (Punjabi)
            0x0A00..=0x0A7F => gurmukhi += 1,
            // Gujarati
            0x0A80..=0x0AFF => gujarati += 1,
            // Tamil
            0x0B80..=0x0BFF => tamil += 1,
            // Telugu
            0x0C00..=0x0C7F => telugu += 1,
            // Kannada
            0x0C80..=0x0CFF => kannada += 1,
            // Malayalam
            0x0D00..=0x0D7F => malayalam += 1,
            // Sinhala
            0x0D80..=0x0DFF => sinhala += 1,
            // Burmese / Myanmar
            0x1000..=0x109F | 0xAA60..=0xAA7F | 0xA9E0..=0xA9FF => burmese += 1,
            // Khmer
            0x1780..=0x17FF | 0x19E0..=0x19FF => khmer += 1,
            // Lao
            0x0E80..=0x0EFF => lao += 1,
            // Ethiopic (Amharic + Tigrinya etc.)
            0x1200..=0x137F | 0x1380..=0x139F | 0x2D80..=0x2DDF | 0xAB00..=0xAB2F => ethiopic += 1,
            _ => {}
        }
    }

    if total == 0 {
        return default;
    }

    // Require at least 20% of non-space chars to match a script.
    let threshold = total / 5;

    // Order matters: the most distinctive scripts first, then the
    // ambiguous catch-alls (CJK without kana, Cyrillic). A single
    // pass of `if x > threshold { return … }` is fine because no
    // text mixes multiple scripts at >20% each in practice.

    // East Asia
    if hangul > threshold {
        return LanguageCode::Ko;
    }
    if kana > threshold {
        return LanguageCode::Ja;
    }
    if cjk > threshold && kana == 0 {
        return LanguageCode::Zh;
    }
    // South Asia (script-distinct → unambiguous mapping)
    if devanagari > threshold {
        return LanguageCode::Hi;
    }
    if bengali > threshold {
        return LanguageCode::Bn;
    }
    if gurmukhi > threshold {
        return LanguageCode::Pa;
    }
    if gujarati > threshold {
        return LanguageCode::Gu;
    }
    if tamil > threshold {
        return LanguageCode::Ta;
    }
    if telugu > threshold {
        return LanguageCode::Te;
    }
    if kannada > threshold {
        return LanguageCode::Kn;
    }
    if malayalam > threshold {
        return LanguageCode::Ml;
    }
    if sinhala > threshold {
        return LanguageCode::Si;
    }
    // Southeast Asia (script-distinct)
    if thai > threshold {
        return LanguageCode::Th;
    }
    if burmese > threshold {
        return LanguageCode::My;
    }
    if khmer > threshold {
        return LanguageCode::Km;
    }
    if lao > threshold {
        return LanguageCode::Lo;
    }
    // Middle East
    if hebrew > threshold {
        return LanguageCode::He;
    }
    if arabic > threshold {
        // Catches Arabic, Urdu, Persian — re-ask layer accepts the
        // mis-attribution to Ar (see function-level docs).
        return LanguageCode::Ar;
    }
    // Caucasus
    if armenian > threshold {
        return LanguageCode::Hy;
    }
    if georgian > threshold {
        return LanguageCode::Ka;
    }
    // Europe — non-Latin scripts
    if greek > threshold {
        return LanguageCode::El;
    }
    if cyrillic > threshold {
        // Mongolian, Serbian-in-Cyrillic etc. all return Ru here;
        // see function-level docs for the explicit override path.
        return LanguageCode::Ru;
    }
    // Africa
    if ethiopic > threshold {
        return LanguageCode::Am;
    }

    default
}

// ── Formality and Domain ─────────────────────────────────────────

/// Formality level for interpretation output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Formality {
    Formal,
    #[default]
    Neutral,
    Casual,
}

/// Domain specialization for interpretation accuracy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Domain {
    #[default]
    General,
    Business,
    Medical,
    Legal,
    Technical,
}

// ── Voice provider abstraction ───────────────────────────────────

/// Kind of voice provider backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoiceProviderKind {
    /// Gemini 3.1 Flash Live (replaces 2.5 Flash Native Audio).
    ///
    /// ## Breaking changes from 2.5 → 3.1 (2026-04)
    /// - `thinkingBudget` → `thinkingLevel` (minimal/low/medium/high)
    /// - Server event structure changed (event parsing rewritten)
    /// - `send_client_content` rejected after first model turn (error 1007)
    /// - Tool calling: sequential only (NON_BLOCKING removed)
    /// - Affective dialog removed
    /// - Proactive audio removed
    GeminiLive,
    /// OpenAI GPT-4o Realtime.
    OpenAiRealtime,
}

impl VoiceProviderKind {
    /// Get the model identifier string for API calls.
    pub fn model_id(self) -> &'static str {
        match self {
            Self::GeminiLive => "gemini-3.1-flash-live-preview",
            Self::OpenAiRealtime => "gpt-4o-realtime-preview",
        }
    }
}

/// Voice provider trait for real-time audio streaming.
///
/// Implementations handle WebSocket/streaming connections to voice APIs.
#[async_trait]
pub trait VoiceProvider: Send + Sync {
    /// Connect to the voice API for a given user session.
    async fn connect(&self, session_id: &str, config: &InterpreterConfig) -> anyhow::Result<()>;

    /// Send an audio chunk (PCM/opus bytes) to the provider.
    async fn send_audio(&self, session_id: &str, chunk: &[u8]) -> anyhow::Result<()>;

    /// Send a text message to the provider (for text-based interpretation).
    async fn send_text(&self, session_id: &str, text: &str) -> anyhow::Result<String>;

    /// Disconnect a voice session.
    async fn disconnect(&self, session_id: &str) -> anyhow::Result<()>;

    /// Get the provider kind.
    fn kind(&self) -> VoiceProviderKind;
}

// ── Interpreter configuration ────────────────────────────────────

/// User voice profile for matching TTS output to the speaker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VoiceGender {
    #[default]
    Male,
    Female,
}

/// User age group for voice matching.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum VoiceAge {
    Child,
    Teenager,
    YoungAdult,
    #[default]
    MiddleAge,
    Elder,
}

impl VoiceGender {
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "male" | "m" => Some(Self::Male),
            "female" | "f" => Some(Self::Female),
            _ => None,
        }
    }
}

impl VoiceAge {
    pub fn from_str_opt(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "child" => Some(Self::Child),
            "teenager" => Some(Self::Teenager),
            "young_adult" | "young" => Some(Self::YoungAdult),
            "middle_age" | "middle" | "adult" => Some(Self::MiddleAge),
            "elder" | "senior" => Some(Self::Elder),
            _ => None,
        }
    }

    /// Map to Typecast API age parameter.
    pub fn as_typecast_str(self) -> &'static str {
        match self {
            Self::Child => "child",
            Self::Teenager => "teenager",
            Self::YoungAdult => "young_adult",
            Self::MiddleAge => "middle_age",
            Self::Elder => "elder",
        }
    }
}

/// Select the best Gemini prebuilt voice based on user gender and age.
///
/// Gemini 3.1 Flash Live voices:
/// - Aoede: Female, warm/expressive (young-middle)
/// - Kore: Female, neutral/professional (middle)
/// - Charon: Male, deep/mature (middle-elder)
/// - Fenrir: Male, clear/younger (young-middle)
/// - Puck: Male, versatile/neutral (young)
pub fn select_gemini_voice(gender: VoiceGender, age: VoiceAge) -> &'static str {
    match (gender, age) {
        (VoiceGender::Female, VoiceAge::Child | VoiceAge::Teenager) => "Aoede",
        (VoiceGender::Female, VoiceAge::YoungAdult) => "Aoede",
        (VoiceGender::Female, VoiceAge::MiddleAge | VoiceAge::Elder) => "Kore",
        (VoiceGender::Male, VoiceAge::Child | VoiceAge::Teenager) => "Puck",
        (VoiceGender::Male, VoiceAge::YoungAdult) => "Fenrir",
        (VoiceGender::Male, VoiceAge::MiddleAge) => "Charon",
        (VoiceGender::Male, VoiceAge::Elder) => "Charon",
    }
}

/// Configuration for a voice interpretation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpreterConfig {
    /// Source language (input).
    pub source_language: LanguageCode,
    /// Target language (output).
    pub target_language: LanguageCode,
    /// Enable bidirectional auto-switch between source and target.
    pub bidirectional: bool,
    /// Formality level of interpretation output.
    pub formality: Formality,
    /// Domain specialization.
    pub domain: Domain,
    /// Preserve tone and emotion in interpretation.
    pub preserve_tone: bool,
    /// API key override (uses provider default if None).
    pub api_key: Option<String>,
    /// Voice provider to use.
    pub provider: VoiceProviderKind,
    /// User's voice gender for TTS matching.
    pub voice_gender: VoiceGender,
    /// User's voice age group for TTS matching.
    pub voice_age: VoiceAge,
    /// Typecast voice clone ID (speak_resource_id from voice cloning API).
    /// When set, TTS uses the cloned voice; otherwise falls back to
    /// auto-matched voice based on gender/age/language.
    pub voice_clone_id: Option<String>,
}

impl Default for InterpreterConfig {
    fn default() -> Self {
        Self {
            source_language: LanguageCode::Ko,
            target_language: LanguageCode::En,
            bidirectional: false,
            formality: Formality::default(),
            domain: Domain::default(),
            preserve_tone: true,
            api_key: None,
            provider: VoiceProviderKind::GeminiLive,
            voice_gender: VoiceGender::default(),
            voice_age: VoiceAge::default(),
            voice_clone_id: None,
        }
    }
}

impl InterpreterConfig {
    /// Build a system prompt for the interpretation session.
    pub fn build_system_prompt(&self) -> String {
        let formality_instruction = match self.formality {
            Formality::Formal => {
                "Use formal, polite language appropriate for professional or official settings."
            }
            Formality::Neutral => "Use standard, everyday language.",
            Formality::Casual => {
                "Use casual, friendly language appropriate for informal conversations."
            }
        };

        let domain_instruction = match self.domain {
            Domain::General => "",
            Domain::Business => " Specialize in business and corporate terminology.",
            Domain::Medical => " Specialize in medical and healthcare terminology.",
            Domain::Legal => " Specialize in legal and judicial terminology.",
            Domain::Technical => " Specialize in technical and engineering terminology.",
        };

        let tone_instruction = if self.preserve_tone {
            " Preserve the speaker's emotional tone, emphasis, and intent."
        } else {
            ""
        };

        let direction = if self.bidirectional {
            format!(
                "You operate in bidirectional mode between {} and {}. \
                 When the speaker speaks {}, immediately interpret into {}. \
                 When the speaker speaks {}, immediately interpret into {}. \
                 Detect the language automatically from each utterance and always \
                 output in the opposite language. Never repeat the input language.",
                self.source_language.display_name(),
                self.target_language.display_name(),
                self.source_language.display_name(),
                self.target_language.display_name(),
                self.target_language.display_name(),
                self.source_language.display_name(),
            )
        } else {
            format!(
                "Interpret from {} to {} only. All output must be in {}.",
                self.source_language.display_name(),
                self.target_language.display_name(),
                self.target_language.display_name(),
            )
        };

        format!(
            "You are a real-time simultaneous interpreter. {direction} {formality_instruction}{domain_instruction}{tone_instruction} \
             CRITICAL RULES FOR SIMULTANEOUS INTERPRETATION: \
             1. Start translating AS SOON AS you understand a meaningful phrase — do NOT wait for the full sentence to finish. \
             2. When the speaker pauses briefly, use that pause to output the translation of what was just said. \
             3. Translate in phrase-level chunks (clauses, noun phrases, verb phrases) rather than waiting for complete sentences. \
             4. Preserve the speaker's pauses and pacing — if they pause deliberately, reflect that in your output timing. \
             5. Never explain, narrate, or add commentary. Output ONLY the translated speech. \
             6. If the speaker is still talking, output what you have so far and continue with the next chunk seamlessly. \
             7. Maintain natural prosody and flow — each chunk should sound like natural speech, not a fragmented list."
        )
    }
}

// ── Session status ───────────────────────────────────────────────

/// Status of a voice interpretation session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterpreterStatus {
    /// Session created but not connected.
    Idle,
    /// Connecting to voice provider.
    Connecting,
    /// Connected and ready to receive audio.
    Ready,
    /// Actively listening to audio input.
    Listening,
    /// Processing/interpreting audio.
    Interpreting,
    /// Outputting interpreted audio/text.
    Speaking,
    /// Error state.
    Error,
    /// Session closed.
    Closed,
}

// ── Session statistics ───────────────────────────────────────────

/// Statistics for a voice interpretation session.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct InterpreterStats {
    /// Number of utterances processed.
    pub utterance_count: u64,
    /// Total session duration in milliseconds.
    pub total_duration_ms: u64,
    /// Average interpretation latency in milliseconds.
    pub avg_latency_ms: f64,
    /// Total source language word count.
    pub source_words: u64,
    /// Total target language word count.
    pub target_words: u64,
    /// Estimated tokens consumed (for billing).
    pub estimated_tokens: u64,
}

impl InterpreterStats {
    /// Record a completed utterance interpretation.
    pub fn record_utterance(
        &mut self,
        latency_ms: u64,
        source_word_count: u64,
        target_word_count: u64,
    ) {
        self.utterance_count += 1;
        self.source_words += source_word_count;
        self.target_words += target_word_count;

        // Running average for latency
        let prev_total = self.avg_latency_ms * (self.utterance_count - 1) as f64;
        self.avg_latency_ms = (prev_total + latency_ms as f64) / self.utterance_count as f64;

        // Rough token estimate: ~3/4 tokens per word for source + target
        self.estimated_tokens += (source_word_count + target_word_count) * 3 / 4;
    }

    /// Estimate cost in credits based on token consumption.
    /// Voice sessions use a higher rate: 1 credit per 500 estimated tokens.
    pub fn estimated_credits(&self) -> u64 {
        self.estimated_tokens.div_ceil(500)
    }
}

// ── Interpreter session ──────────────────────────────────────────

/// A voice interpretation session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterpreterSession {
    /// Unique session identifier.
    pub id: String,
    /// User who owns this session.
    pub user_id: String,
    /// Session configuration.
    pub config: InterpreterConfig,
    /// Current session status.
    pub status: InterpreterStatus,
    /// Session statistics.
    pub stats: InterpreterStats,
    /// Session creation timestamp (epoch ms).
    pub created_at: u64,
}

impl InterpreterSession {
    /// Create a new session.
    pub fn new(session_id: String, user_id: String, config: InterpreterConfig) -> Self {
        let now_ms = now_epoch_ms();

        Self {
            id: session_id,
            user_id,
            config,
            status: InterpreterStatus::Idle,
            stats: InterpreterStats::default(),
            created_at: now_ms,
        }
    }
}

// ── Session manager ──────────────────────────────────────────────

/// Manages active voice interpretation sessions.
pub struct VoiceSessionManager {
    /// Active sessions indexed by session ID.
    sessions: Arc<Mutex<HashMap<String, InterpreterSession>>>,
    /// Maximum concurrent sessions per user.
    max_sessions_per_user: usize,
    /// Whether voice features are enabled.
    enabled: bool,
    /// Default source language code (from config).
    default_source_language: String,
    /// Default target language code (from config).
    default_target_language: String,
    /// Default voice provider ("gemini" or "openai").
    default_provider: Option<String>,
}

impl VoiceSessionManager {
    /// Create a new session manager.
    pub fn new(enabled: bool, max_sessions_per_user: usize) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            max_sessions_per_user,
            enabled,
            default_source_language: "ko".to_string(),
            default_target_language: "en".to_string(),
            default_provider: None,
        }
    }

    /// Create a new session manager with explicit default languages.
    pub fn with_defaults(
        enabled: bool,
        max_sessions_per_user: usize,
        default_source_language: String,
        default_target_language: String,
        default_provider: Option<String>,
    ) -> Self {
        Self {
            sessions: Arc::new(Mutex::new(HashMap::new())),
            max_sessions_per_user,
            enabled,
            default_source_language,
            default_target_language,
            default_provider,
        }
    }

    /// Get the default source language code.
    pub fn default_source_language(&self) -> &str {
        &self.default_source_language
    }

    /// Get the default target language code.
    pub fn default_target_language(&self) -> &str {
        &self.default_target_language
    }

    /// Get the default voice provider name.
    pub fn default_provider(&self) -> Option<&str> {
        self.default_provider.as_deref()
    }

    /// Create a new interpretation session.
    pub async fn create_session(
        &self,
        user_id: &str,
        config: InterpreterConfig,
    ) -> anyhow::Result<InterpreterSession> {
        if !self.enabled {
            anyhow::bail!("Voice features are disabled");
        }

        let mut sessions = self.sessions.lock().await;

        // Check per-user session limit
        let user_session_count = sessions
            .values()
            .filter(|s| s.user_id == user_id && s.status != InterpreterStatus::Closed)
            .count();

        if user_session_count >= self.max_sessions_per_user {
            anyhow::bail!(
                "Maximum concurrent voice sessions ({}) reached for user",
                self.max_sessions_per_user
            );
        }

        let session_id = uuid::Uuid::new_v4().to_string();
        let session = InterpreterSession::new(session_id.clone(), user_id.to_string(), config);
        sessions.insert(session_id, session.clone());

        Ok(session)
    }

    /// Get a session by ID.
    pub async fn get_session(&self, session_id: &str) -> Option<InterpreterSession> {
        let sessions = self.sessions.lock().await;
        sessions.get(session_id).cloned()
    }

    /// Update session status.
    pub async fn update_status(
        &self,
        session_id: &str,
        status: InterpreterStatus,
    ) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
        session.status = status;
        Ok(())
    }

    /// Record a completed utterance for a session.
    pub async fn record_utterance(
        &self,
        session_id: &str,
        latency_ms: u64,
        source_words: u64,
        target_words: u64,
    ) -> anyhow::Result<()> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;
        session
            .stats
            .record_utterance(latency_ms, source_words, target_words);
        Ok(())
    }

    /// Close a session and return final stats.
    pub async fn close_session(&self, session_id: &str) -> anyhow::Result<InterpreterStats> {
        let mut sessions = self.sessions.lock().await;
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| anyhow::anyhow!("Session not found: {session_id}"))?;

        let now_ms = now_epoch_ms();

        session.status = InterpreterStatus::Closed;
        session.stats.total_duration_ms = now_ms.saturating_sub(session.created_at);

        Ok(session.stats.clone())
    }

    /// List all active sessions for a user.
    pub async fn list_user_sessions(&self, user_id: &str) -> Vec<InterpreterSession> {
        let sessions = self.sessions.lock().await;
        sessions
            .values()
            .filter(|s| s.user_id == user_id && s.status != InterpreterStatus::Closed)
            .cloned()
            .collect()
    }

    /// Clean up closed sessions older than the given age in milliseconds.
    pub async fn cleanup_closed(&self, max_age_ms: u64) {
        let now_ms = now_epoch_ms();

        let mut sessions = self.sessions.lock().await;
        sessions.retain(|_, s| {
            if s.status == InterpreterStatus::Closed {
                now_ms.saturating_sub(s.created_at) < max_age_ms
            } else {
                true
            }
        });
    }

    /// Get total active session count.
    pub async fn active_session_count(&self) -> usize {
        let sessions = self.sessions.lock().await;
        sessions
            .values()
            .filter(|s| s.status != InterpreterStatus::Closed)
            .count()
    }
}

// ── Gemini Live voice provider (stub) ────────────────────────────

/// Gemini 2.5 Flash Native Audio voice provider.
///
/// Connects to Google's Gemini Live API for real-time voice interpretation.
/// Uses WebSocket streaming for bidirectional audio.
pub struct GeminiLiveProvider {
    /// API key for Gemini API.
    api_key: Option<String>,
    /// VAD threshold (default 0.4).
    vad_threshold: f32,
    /// Silence detection timeout in milliseconds (default 300ms).
    silence_timeout_ms: u64,
}

impl GeminiLiveProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self {
            api_key,
            vad_threshold: 0.4,
            silence_timeout_ms: 300,
        }
    }

    /// Get VAD threshold.
    pub fn vad_threshold(&self) -> f32 {
        self.vad_threshold
    }

    /// Get silence timeout.
    pub fn silence_timeout_ms(&self) -> u64 {
        self.silence_timeout_ms
    }
}

#[async_trait]
impl VoiceProvider for GeminiLiveProvider {
    async fn connect(&self, session_id: &str, config: &InterpreterConfig) -> anyhow::Result<()> {
        if self.api_key.is_none() {
            anyhow::bail!("Gemini API key is required for voice sessions");
        }

        tracing::info!(
            session_id = session_id,
            model = VoiceProviderKind::GeminiLive.model_id(),
            source = config.source_language.as_str(),
            target = config.target_language.as_str(),
            "Connecting to Gemini Live voice API"
        );

        // WebSocket connection would be established here in production.
        // The actual WebSocket streaming implementation depends on the
        // runtime environment and is handled by the gateway layer.
        Ok(())
    }

    async fn send_audio(&self, session_id: &str, chunk: &[u8]) -> anyhow::Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }

        tracing::trace!(
            session_id = session_id,
            chunk_size = chunk.len(),
            "Sending audio chunk to Gemini Live"
        );

        Ok(())
    }

    async fn send_text(&self, session_id: &str, text: &str) -> anyhow::Result<String> {
        tracing::debug!(
            session_id = session_id,
            text_len = text.len(),
            "Sending text to Gemini Live for interpretation"
        );

        // In production, this would send the text to the Gemini API
        // and return the interpreted text.
        Ok(format!("[Gemini interpretation of: {}]", text))
    }

    async fn disconnect(&self, session_id: &str) -> anyhow::Result<()> {
        tracing::info!(session_id = session_id, "Disconnecting Gemini Live session");
        Ok(())
    }

    fn kind(&self) -> VoiceProviderKind {
        VoiceProviderKind::GeminiLive
    }
}

// ── OpenAI Realtime voice provider (stub) ────────────────────────

/// OpenAI GPT-4o Realtime voice provider.
///
/// Fallback voice provider using OpenAI's Realtime API.
pub struct OpenAiRealtimeProvider {
    /// API key for OpenAI API.
    api_key: Option<String>,
}

impl OpenAiRealtimeProvider {
    pub fn new(api_key: Option<String>) -> Self {
        Self { api_key }
    }
}

#[async_trait]
impl VoiceProvider for OpenAiRealtimeProvider {
    async fn connect(&self, session_id: &str, config: &InterpreterConfig) -> anyhow::Result<()> {
        if self.api_key.is_none() {
            anyhow::bail!("OpenAI API key is required for voice sessions");
        }

        tracing::info!(
            session_id = session_id,
            model = VoiceProviderKind::OpenAiRealtime.model_id(),
            source = config.source_language.as_str(),
            target = config.target_language.as_str(),
            "Connecting to OpenAI Realtime voice API"
        );

        Ok(())
    }

    async fn send_audio(&self, session_id: &str, chunk: &[u8]) -> anyhow::Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }

        tracing::trace!(
            session_id = session_id,
            chunk_size = chunk.len(),
            "Sending audio chunk to OpenAI Realtime"
        );

        Ok(())
    }

    async fn send_text(&self, session_id: &str, text: &str) -> anyhow::Result<String> {
        tracing::debug!(
            session_id = session_id,
            text_len = text.len(),
            "Sending text to OpenAI Realtime for interpretation"
        );

        Ok(format!("[OpenAI interpretation of: {}]", text))
    }

    async fn disconnect(&self, session_id: &str) -> anyhow::Result<()> {
        tracing::info!(
            session_id = session_id,
            "Disconnecting OpenAI Realtime session"
        );
        Ok(())
    }

    fn kind(&self) -> VoiceProviderKind {
        VoiceProviderKind::OpenAiRealtime
    }
}

// ── Factory ──────────────────────────────────────────────────────

/// Create a voice provider by kind.
pub fn create_voice_provider(
    kind: VoiceProviderKind,
    api_key: Option<String>,
) -> Box<dyn VoiceProvider> {
    match kind {
        VoiceProviderKind::GeminiLive => Box::new(GeminiLiveProvider::new(api_key)),
        VoiceProviderKind::OpenAiRealtime => Box::new(OpenAiRealtimeProvider::new(api_key)),
    }
}

/// Get current time in epoch milliseconds.
fn now_epoch_ms() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis(),
    )
    .unwrap_or(u64::MAX)
}

// ── Tests ────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_code_roundtrip() {
        for lang in LanguageCode::all() {
            let code = lang.as_str();
            let parsed = LanguageCode::from_str_code(code);
            assert_eq!(parsed, Some(*lang), "Roundtrip failed for {code}");
        }
    }

    #[test]
    fn language_code_count() {
        // Updated 2026-05: 25 -> 80 (expansion to cover South-Asian
        // scripts, Hebrew/Greek/Armenian/Georgian, Burmese/Khmer/Lao,
        // and the long tail of Latin-script European + Central Asian
        // + African languages). If you add a new variant, bump this
        // number and add a matching arm in `as_str` / `display_name`
        // / `from_str_code` / `lang_to_typecast_iso3`
        // (typecast_interp.rs) / `language_code_to_deepgram`
        // (deepgram_stt.rs); the build will refuse to compile until
        // you do.
        assert_eq!(LanguageCode::all().len(), 80);
    }

    #[test]
    fn language_code_display_names() {
        assert_eq!(LanguageCode::Ko.display_name(), "Korean");
        assert_eq!(LanguageCode::En.display_name(), "English");
        assert_eq!(LanguageCode::Ja.display_name(), "Japanese");
        assert_eq!(LanguageCode::Ar.display_name(), "Arabic");
    }

    #[test]
    fn language_code_case_insensitive_parse() {
        assert_eq!(LanguageCode::from_str_code("KO"), Some(LanguageCode::Ko));
        assert_eq!(LanguageCode::from_str_code("En"), Some(LanguageCode::En));
        assert_eq!(
            LanguageCode::from_str_code("ZH-TW"),
            Some(LanguageCode::ZhTw)
        );
        assert_eq!(
            LanguageCode::from_str_code("zh_tw"),
            Some(LanguageCode::ZhTw)
        );
    }

    #[test]
    fn language_code_unknown_returns_none() {
        assert_eq!(LanguageCode::from_str_code("xx"), None);
        assert_eq!(LanguageCode::from_str_code(""), None);
    }

    #[test]
    fn detect_korean() {
        let text = "안녕하세요 세계";
        assert_eq!(detect_language(text, LanguageCode::En), LanguageCode::Ko);
    }

    #[test]
    fn detect_japanese() {
        let text = "こんにちは世界";
        assert_eq!(detect_language(text, LanguageCode::En), LanguageCode::Ja);
    }

    #[test]
    fn detect_chinese() {
        let text = "你好世界";
        assert_eq!(detect_language(text, LanguageCode::En), LanguageCode::Zh);
    }

    #[test]
    fn detect_arabic() {
        let text = "مرحبا بالعالم";
        assert_eq!(detect_language(text, LanguageCode::En), LanguageCode::Ar);
    }

    #[test]
    fn detect_thai() {
        let text = "สวัสดีชาวโลก";
        assert_eq!(detect_language(text, LanguageCode::En), LanguageCode::Th);
    }

    #[test]
    fn detect_hindi() {
        let text = "नमस्ते दुनिया";
        assert_eq!(detect_language(text, LanguageCode::En), LanguageCode::Hi);
    }

    #[test]
    fn detect_cyrillic() {
        let text = "Привет мир";
        assert_eq!(detect_language(text, LanguageCode::En), LanguageCode::Ru);
    }

    #[test]
    fn detect_empty_returns_default() {
        assert_eq!(detect_language("", LanguageCode::Ko), LanguageCode::Ko);
    }

    #[test]
    fn detect_ascii_returns_default() {
        // Pure ASCII text can't be distinguished between Latin-script languages
        assert_eq!(
            detect_language("hello world", LanguageCode::En),
            LanguageCode::En
        );
    }

    // ── 2026-05 expansion: new script detection coverage ──

    #[test]
    fn detect_bengali() {
        // "Hello world" in Bengali.
        assert_eq!(
            detect_language("ওহে বিশ্ব", LanguageCode::En),
            LanguageCode::Bn
        );
    }

    #[test]
    fn detect_tamil() {
        assert_eq!(
            detect_language("வணக்கம் உலகம்", LanguageCode::En),
            LanguageCode::Ta
        );
    }

    #[test]
    fn detect_telugu() {
        assert_eq!(
            detect_language("హలో ప్రపంచం", LanguageCode::En),
            LanguageCode::Te
        );
    }

    #[test]
    fn detect_hebrew() {
        assert_eq!(
            detect_language("שלום עולם", LanguageCode::En),
            LanguageCode::He
        );
    }

    #[test]
    fn detect_greek() {
        assert_eq!(
            detect_language("Γεια σου κόσμε", LanguageCode::En),
            LanguageCode::El
        );
    }

    #[test]
    fn detect_armenian() {
        assert_eq!(
            detect_language("Բարեւ աշխարհ", LanguageCode::En),
            LanguageCode::Hy
        );
    }

    #[test]
    fn detect_georgian() {
        assert_eq!(
            detect_language("გამარჯობა მსოფლიო", LanguageCode::En),
            LanguageCode::Ka
        );
    }

    #[test]
    fn detect_burmese() {
        assert_eq!(
            detect_language("မင်္ဂလာပါ ကမ္ဘာ", LanguageCode::En),
            LanguageCode::My
        );
    }

    #[test]
    fn detect_khmer() {
        assert_eq!(
            detect_language("សួស្ដី ពិភពលោក", LanguageCode::En),
            LanguageCode::Km
        );
    }

    #[test]
    fn detect_lao() {
        assert_eq!(
            detect_language("ສະບາຍດີ ໂລກ", LanguageCode::En),
            LanguageCode::Lo
        );
    }

    #[test]
    fn detect_amharic_via_ethiopic_script() {
        // "Hello world" in Amharic (Ge'ez script).
        assert_eq!(
            detect_language("ሰላም ዓለም", LanguageCode::En),
            LanguageCode::Am
        );
    }

    #[test]
    fn detect_sinhala() {
        assert_eq!(
            detect_language("ආයුබෝවන් ලෝකය", LanguageCode::En),
            LanguageCode::Si
        );
    }

    #[test]
    fn detect_punjabi_gurmukhi() {
        // Hello in Punjabi (Gurmukhi script). The script is unambiguous,
        // so this resolves to Pa rather than falling back to default.
        assert_eq!(
            detect_language("ਸਤ ਸ੍ਰੀ ਅਕਾਲ ਦੁਨੀਆ", LanguageCode::En),
            LanguageCode::Pa
        );
    }

    #[test]
    fn detect_gujarati() {
        assert_eq!(
            detect_language("હેલો વર્લ્ડ", LanguageCode::En),
            LanguageCode::Gu
        );
    }

    #[test]
    fn detect_kannada() {
        assert_eq!(
            detect_language("ಹಲೋ ಜಗತ್ತು", LanguageCode::En),
            LanguageCode::Kn
        );
    }

    #[test]
    fn detect_malayalam() {
        assert_eq!(
            detect_language("ഹലോ ലോകം", LanguageCode::En),
            LanguageCode::Ml
        );
    }

    // ── Documented limitations: confirm we still return the expected
    //    fallback behavior so future readers can see what's intentional.
    //    Each of these would ideally resolve to a different language,
    //    but distinguishing requires per-language script-free analysis
    //    and is out of scope for the Unicode-range detector.

    #[test]
    fn detect_mongolian_cyrillic_resolves_to_russian_documented_limitation() {
        // Mongolian text written in Cyrillic — the detector can't
        // distinguish from Russian by script alone. Documented in
        // the function's doc comment.
        assert_eq!(
            detect_language("Сайн байна уу дэлхий", LanguageCode::En),
            LanguageCode::Ru
        );
    }

    #[test]
    fn detect_urdu_resolves_to_arabic_documented_limitation() {
        // Urdu text — Arabic script — resolves to Ar. Documented.
        assert_eq!(
            detect_language("ہیلو دنیا", LanguageCode::En),
            LanguageCode::Ar
        );
    }

    // ── Coverage harness across new variants ──

    #[test]
    fn language_code_all_new_variants_round_trip_through_str() {
        // Belt and suspenders: every variant in `all()` must round-trip
        // through `as_str()` -> `from_str_code()`. The 25 original
        // variants already had this test (`language_code_roundtrip`);
        // this duplicates it specifically for the variants the 2026-05
        // expansion added so a later edit to the new arms shows up
        // here rather than in the catch-all roundtrip.
        for lang in [
            LanguageCode::Mn,
            LanguageCode::My,
            LanguageCode::Km,
            LanguageCode::Lo,
            LanguageCode::Bn,
            LanguageCode::Ta,
            LanguageCode::Te,
            LanguageCode::Mr,
            LanguageCode::Gu,
            LanguageCode::Kn,
            LanguageCode::Ml,
            LanguageCode::Pa,
            LanguageCode::Or,
            LanguageCode::Si,
            LanguageCode::Ur,
            LanguageCode::Ne,
            LanguageCode::Sd,
            LanguageCode::No,
            LanguageCode::Fi,
            LanguageCode::Is,
            LanguageCode::Ga,
            LanguageCode::Cy,
            LanguageCode::Mt,
            LanguageCode::Eu,
            LanguageCode::Ca,
            LanguageCode::Gl,
            LanguageCode::Hu,
            LanguageCode::Ro,
            LanguageCode::Sk,
            LanguageCode::Sl,
            LanguageCode::Hr,
            LanguageCode::Sr,
            LanguageCode::Bs,
            LanguageCode::Sq,
            LanguageCode::Et,
            LanguageCode::Lv,
            LanguageCode::Lt,
            LanguageCode::Be,
            LanguageCode::Bg,
            LanguageCode::Mk,
            LanguageCode::El,
            LanguageCode::Hy,
            LanguageCode::Ka,
            LanguageCode::He,
            LanguageCode::Fa,
            LanguageCode::Kk,
            LanguageCode::Uz,
            LanguageCode::Az,
            LanguageCode::Sw,
            LanguageCode::Am,
            LanguageCode::Yo,
            LanguageCode::Ha,
            LanguageCode::Zu,
            LanguageCode::Af,
            LanguageCode::So,
        ] {
            let code = lang.as_str();
            assert_eq!(
                LanguageCode::from_str_code(code),
                Some(lang),
                "round-trip failed for {code}",
            );
        }
    }

    #[test]
    fn from_str_code_handles_common_aliases() {
        assert_eq!(LanguageCode::from_str_code("nb"), Some(LanguageCode::No));
        assert_eq!(LanguageCode::from_str_code("nn"), Some(LanguageCode::No));
        assert_eq!(LanguageCode::from_str_code("iw"), Some(LanguageCode::He));
        assert_eq!(LanguageCode::from_str_code("in"), Some(LanguageCode::Id));
        assert_eq!(
            LanguageCode::from_str_code("zh-cn"),
            Some(LanguageCode::Zh)
        );
        assert_eq!(LanguageCode::from_str_code("fil"), Some(LanguageCode::Tl));
    }

    #[test]
    fn formality_default() {
        assert_eq!(Formality::default(), Formality::Neutral);
    }

    #[test]
    fn domain_default() {
        assert_eq!(Domain::default(), Domain::General);
    }

    #[test]
    fn interpreter_config_default() {
        let config = InterpreterConfig::default();
        assert_eq!(config.source_language, LanguageCode::Ko);
        assert_eq!(config.target_language, LanguageCode::En);
        assert!(!config.bidirectional);
        assert!(config.preserve_tone);
        assert_eq!(config.provider, VoiceProviderKind::GeminiLive);
    }

    #[test]
    fn interpreter_config_system_prompt_unidirectional() {
        let config = InterpreterConfig {
            source_language: LanguageCode::Ko,
            target_language: LanguageCode::En,
            bidirectional: false,
            formality: Formality::Formal,
            domain: Domain::Business,
            preserve_tone: true,
            ..Default::default()
        };

        let prompt = config.build_system_prompt();
        assert!(prompt.contains("Korean"));
        assert!(prompt.contains("English"));
        assert!(prompt.contains("formal"));
        assert!(prompt.contains("business"));
        assert!(prompt.contains("tone"));
        assert!(!prompt.contains("bidirectional mode"));
    }

    #[test]
    fn interpreter_config_system_prompt_bidirectional() {
        let config = InterpreterConfig {
            source_language: LanguageCode::Ja,
            target_language: LanguageCode::Ko,
            bidirectional: true,
            ..Default::default()
        };

        let prompt = config.build_system_prompt();
        assert!(prompt.contains("bidirectional mode between Japanese and Korean"));
        // Should instruct detection and output in the opposite language
        assert!(prompt.contains("Detect the language automatically"));
        assert!(prompt.contains("output in the opposite language"));
    }

    #[test]
    fn voice_provider_kind_model_ids() {
        assert_eq!(
            VoiceProviderKind::GeminiLive.model_id(),
            "gemini-3.1-flash-live-preview"
        );
        assert_eq!(
            VoiceProviderKind::OpenAiRealtime.model_id(),
            "gpt-4o-realtime-preview"
        );
    }

    #[test]
    fn interpreter_stats_record_utterance() {
        let mut stats = InterpreterStats::default();

        stats.record_utterance(100, 10, 12);
        assert_eq!(stats.utterance_count, 1);
        assert_eq!(stats.source_words, 10);
        assert_eq!(stats.target_words, 12);
        assert!((stats.avg_latency_ms - 100.0).abs() < 0.01);

        stats.record_utterance(200, 20, 25);
        assert_eq!(stats.utterance_count, 2);
        assert_eq!(stats.source_words, 30);
        assert_eq!(stats.target_words, 37);
        assert!((stats.avg_latency_ms - 150.0).abs() < 0.01);
    }

    #[test]
    fn interpreter_stats_estimated_credits() {
        let mut stats = InterpreterStats::default();
        assert_eq!(stats.estimated_credits(), 0);

        stats.estimated_tokens = 1000;
        assert_eq!(stats.estimated_credits(), 2);

        stats.estimated_tokens = 500;
        assert_eq!(stats.estimated_credits(), 1);

        stats.estimated_tokens = 501;
        assert_eq!(stats.estimated_credits(), 2);
    }

    #[test]
    fn interpreter_session_creation() {
        let config = InterpreterConfig::default();
        let session = InterpreterSession::new(
            "session-001".to_string(),
            "zeroclaw_user".to_string(),
            config,
        );

        assert_eq!(session.id, "session-001");
        assert_eq!(session.user_id, "zeroclaw_user");
        assert_eq!(session.status, InterpreterStatus::Idle);
        assert_eq!(session.stats.utterance_count, 0);
        assert!(session.created_at > 0);
    }

    #[tokio::test]
    async fn session_manager_create_and_get() {
        let manager = VoiceSessionManager::new(true, 3);

        let session = manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await
            .unwrap();

        let retrieved = manager.get_session(&session.id).await;
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap().user_id, "zeroclaw_user");
    }

    #[tokio::test]
    async fn session_manager_enforces_limit() {
        let manager = VoiceSessionManager::new(true, 2);

        manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await
            .unwrap();
        manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await
            .unwrap();

        let result = manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Maximum concurrent voice sessions"));
    }

    #[tokio::test]
    async fn session_manager_disabled() {
        let manager = VoiceSessionManager::new(false, 3);
        let result = manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("disabled"));
    }

    #[tokio::test]
    async fn session_manager_close_returns_stats() {
        let manager = VoiceSessionManager::new(true, 3);

        let session = manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await
            .unwrap();

        manager
            .record_utterance(&session.id, 150, 10, 12)
            .await
            .unwrap();

        let stats = manager.close_session(&session.id).await.unwrap();
        assert_eq!(stats.utterance_count, 1);
        assert_eq!(stats.source_words, 10);
        // Duration may be 0 if test completes within the same millisecond
        let _ = stats.total_duration_ms;

        // Verify session is closed
        let session = manager.get_session(&session.id).await.unwrap();
        assert_eq!(session.status, InterpreterStatus::Closed);
    }

    #[tokio::test]
    async fn session_manager_list_user_sessions() {
        let manager = VoiceSessionManager::new(true, 5);

        manager
            .create_session("user_a", InterpreterConfig::default())
            .await
            .unwrap();
        manager
            .create_session("user_a", InterpreterConfig::default())
            .await
            .unwrap();
        manager
            .create_session("user_b", InterpreterConfig::default())
            .await
            .unwrap();

        let user_a_sessions = manager.list_user_sessions("user_a").await;
        assert_eq!(user_a_sessions.len(), 2);

        let user_b_sessions = manager.list_user_sessions("user_b").await;
        assert_eq!(user_b_sessions.len(), 1);
    }

    #[tokio::test]
    async fn session_manager_active_count() {
        let manager = VoiceSessionManager::new(true, 5);

        let s1 = manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await
            .unwrap();
        manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await
            .unwrap();

        assert_eq!(manager.active_session_count().await, 2);

        manager.close_session(&s1.id).await.unwrap();
        assert_eq!(manager.active_session_count().await, 1);
    }

    #[tokio::test]
    async fn session_manager_update_status() {
        let manager = VoiceSessionManager::new(true, 3);

        let session = manager
            .create_session("zeroclaw_user", InterpreterConfig::default())
            .await
            .unwrap();

        manager
            .update_status(&session.id, InterpreterStatus::Listening)
            .await
            .unwrap();

        let updated = manager.get_session(&session.id).await.unwrap();
        assert_eq!(updated.status, InterpreterStatus::Listening);
    }

    #[test]
    fn gemini_provider_defaults() {
        let provider = GeminiLiveProvider::new(Some("test-key".to_string()));
        assert_eq!(provider.vad_threshold(), 0.4);
        assert_eq!(provider.silence_timeout_ms(), 300);
        assert_eq!(provider.kind(), VoiceProviderKind::GeminiLive);
    }

    #[test]
    fn openai_provider_kind() {
        let provider = OpenAiRealtimeProvider::new(Some("test-key".to_string()));
        assert_eq!(provider.kind(), VoiceProviderKind::OpenAiRealtime);
    }

    #[tokio::test]
    async fn gemini_provider_connect_requires_key() {
        let provider = GeminiLiveProvider::new(None);
        let config = InterpreterConfig::default();
        let result = provider.connect("session-1", &config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key"));
    }

    #[tokio::test]
    async fn openai_provider_connect_requires_key() {
        let provider = OpenAiRealtimeProvider::new(None);
        let config = InterpreterConfig::default();
        let result = provider.connect("session-1", &config).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("API key"));
    }

    #[tokio::test]
    async fn gemini_provider_send_text() {
        let provider = GeminiLiveProvider::new(Some("test-key".to_string()));
        let result = provider.send_text("session-1", "hello").await.unwrap();
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn openai_provider_send_text() {
        let provider = OpenAiRealtimeProvider::new(Some("test-key".to_string()));
        let result = provider.send_text("session-1", "hello").await.unwrap();
        assert!(result.contains("hello"));
    }

    #[test]
    fn create_voice_provider_gemini() {
        let provider = create_voice_provider(VoiceProviderKind::GeminiLive, None);
        assert_eq!(provider.kind(), VoiceProviderKind::GeminiLive);
    }

    #[test]
    fn create_voice_provider_openai() {
        let provider = create_voice_provider(VoiceProviderKind::OpenAiRealtime, None);
        assert_eq!(provider.kind(), VoiceProviderKind::OpenAiRealtime);
    }
}
