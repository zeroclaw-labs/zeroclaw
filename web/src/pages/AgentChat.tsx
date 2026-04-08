import { useState, useEffect, useRef, useMemo, useCallback } from 'react';
import {
  Send, Bot, User, AlertCircle, Copy, Check, SquarePen,
  FileText, FileDown, Volume2, VolumeX, Mic, MicOff,
  Paperclip, FolderOpen, Github,
} from 'lucide-react';
import { marked } from 'marked';
import type { WsMessage } from '@/types/api';
import { WebSocketClient } from '@/lib/ws';
import { getToken } from '@/lib/auth';

// Configure marked for safe rendering
marked.setOptions({
  breaks: true,
  gfm: true,
});

interface ChatMessage {
  id: string;
  role: 'user' | 'agent';
  content: string;
  timestamp: Date;
}

let fallbackMessageIdCounter = 0;
const EMPTY_DONE_FALLBACK =
  'Tool execution completed, but no final response text was returned.';

function makeMessageId(): string {
  const uuid = globalThis.crypto?.randomUUID?.();
  if (uuid) return uuid;

  fallbackMessageIdCounter += 1;
  return `msg_${Date.now().toString(36)}_${fallbackMessageIdCounter.toString(36)}_${Math.random()
    .toString(36)
    .slice(2, 10)}`;
}

/** Render markdown string to sanitized HTML */
function renderMarkdown(content: string): string {
  try {
    return marked.parse(content, { async: false }) as string;
  } catch {
    // Fallback: escape HTML and preserve whitespace
    return content
      .replace(/&/g, '&amp;')
      .replace(/</g, '&lt;')
      .replace(/>/g, '&gt;')
      .replace(/\n/g, '<br>');
  }
}

// ---------------------------------------------------------------------------
// Language detection — covers all languages supported by modern AI models
// ---------------------------------------------------------------------------

const LANG_PREF_MEMORY_KEY = 'user_profile_language';
const LANG_PREF_LOCAL_KEY = 'zeroclaw.user.lang';
const UNDETECTED_LANG = '__undetected__';

/** Save language preference to MoA long-term memory (fire-and-forget). */
function persistLangToMemory(lang: string): void {
  const token = getToken();
  const headers: Record<string, string> = { 'Content-Type': 'application/json' };
  if (token) headers['Authorization'] = `Bearer ${token}`;
  fetch('/api/memory', {
    method: 'POST',
    headers,
    body: JSON.stringify({
      key: LANG_PREF_MEMORY_KEY,
      content: `User's primary language: ${lang}`,
      category: 'core',
    }),
  }).catch(() => { /* best-effort */ });
  try { localStorage.setItem(LANG_PREF_LOCAL_KEY, lang); } catch { /* ok */ }
}

/** Load language preference from MoA memory. Returns null if not found. */
async function loadLangFromMemory(): Promise<string | null> {
  // Fast path: localStorage cache
  try {
    const cached = localStorage.getItem(LANG_PREF_LOCAL_KEY);
    if (cached && cached.length >= 2) return cached;
  } catch { /* ok */ }

  // Slow path: ask backend memory
  try {
    const token = getToken();
    const headers: Record<string, string> = {};
    if (token) headers['Authorization'] = `Bearer ${token}`;
    const res = await fetch(`/api/memory?query=${encodeURIComponent(LANG_PREF_MEMORY_KEY)}`, { headers });
    if (!res.ok) return null;
    const data = await res.json() as { entries?: Array<{ key: string; content: string }> };
    const entry = data.entries?.find((e) => e.key === LANG_PREF_MEMORY_KEY);
    if (!entry) return null;
    // Extract lang code from content like "User's primary language: ko-KR"
    const match = entry.content.match(/:\s*([a-z]{2,3}(?:-[A-Za-z]{2,4})?)\s*$/);
    if (match && match[1]) {
      try { localStorage.setItem(LANG_PREF_LOCAL_KEY, match[1]); } catch { /* ok */ }
      return match[1] ?? null;
    }
  } catch { /* ok */ }
  return null;
}

/**
 * Comprehensive language detection using Unicode script analysis + word-level
 * heuristics. Covers 70+ languages across all major script families.
 * Returns BCP-47 tag for Web Speech API, or UNDETECTED_LANG if uncertain.
 */
function detectLanguage(text: string): string {
  const clean = text
    .replace(/```[\s\S]*?```/g, '')
    .replace(/`[^`]+`/g, '')
    .replace(/https?:\/\/\S+/g, '')
    .replace(/[#*_~>\[\]()!|`]/g, '')
    .replace(/\s+/g, ' ')
    .trim();

  if (!clean || clean.length < 2) return UNDETECTED_LANG;

  // ── Unicode script counters ──
  let ko = 0, ja = 0, zh = 0;
  let cyrillic = 0, arabic = 0, hebrew = 0;
  let thai = 0, lao = 0, khmer = 0, myanmar = 0;
  let devanagari = 0, bengali = 0, gujarati = 0, gurmukhi = 0;
  let tamil = 0, telugu = 0, kannada = 0, malayalam = 0, sinhala = 0, odia = 0;
  let greek = 0, armenian = 0, georgian = 0;
  let ethiopic = 0, tibetan = 0, mongolian = 0;
  let latin = 0, vietnamese = 0;
  // Additional Indic scripts
  let meeteiMayek = 0, olChiki = 0, lepcha = 0, limbu = 0, chakma = 0;
  // Additional CJK markers
  let bopomofo = 0, yi = 0;
  // Thaana (Dhivehi/Maldivian)
  let thaana = 0;
  // N'Ko (West African)
  let nko = 0;
  // Javanese
  let javanese = 0;
  // Balinese
  let balinese = 0;
  // Sundanese
  let sundanese = 0;
  // Tai Le / New Tai Lue / Tai Tham (Dai languages)
  let taiLe = 0;
  // Cherokee
  let cherokee = 0;

  for (const ch of clean) {
    const cp = ch.codePointAt(0) ?? 0;
    // Hangul
    if ((cp >= 0xAC00 && cp <= 0xD7AF) || (cp >= 0x1100 && cp <= 0x11FF) ||
        (cp >= 0x3130 && cp <= 0x318F)) { ko++; continue; }
    // Japanese Hiragana / Katakana
    if ((cp >= 0x3040 && cp <= 0x309F) || (cp >= 0x30A0 && cp <= 0x30FF) ||
        (cp >= 0x31F0 && cp <= 0x31FF) || (cp >= 0xFF65 && cp <= 0xFF9F)) { ja++; continue; }
    // CJK Unified
    if ((cp >= 0x4E00 && cp <= 0x9FFF) || (cp >= 0x3400 && cp <= 0x4DBF) ||
        (cp >= 0x20000 && cp <= 0x2A6DF)) { zh++; continue; }
    // Cyrillic + Extended
    if ((cp >= 0x0400 && cp <= 0x04FF) || (cp >= 0x0500 && cp <= 0x052F)) { cyrillic++; continue; }
    // Arabic + Extended
    if ((cp >= 0x0600 && cp <= 0x06FF) || (cp >= 0x0750 && cp <= 0x077F) ||
        (cp >= 0x08A0 && cp <= 0x08FF) || (cp >= 0xFB50 && cp <= 0xFDFF) ||
        (cp >= 0xFE70 && cp <= 0xFEFF)) { arabic++; continue; }
    // Hebrew
    if ((cp >= 0x0590 && cp <= 0x05FF) || (cp >= 0xFB1D && cp <= 0xFB4F)) { hebrew++; continue; }
    // Thai
    if (cp >= 0x0E00 && cp <= 0x0E7F) { thai++; continue; }
    // Lao
    if (cp >= 0x0E80 && cp <= 0x0EFF) { lao++; continue; }
    // Khmer
    if ((cp >= 0x1780 && cp <= 0x17FF) || (cp >= 0x19E0 && cp <= 0x19FF)) { khmer++; continue; }
    // Myanmar (Burmese)
    if ((cp >= 0x1000 && cp <= 0x109F) || (cp >= 0xAA60 && cp <= 0xAA7F)) { myanmar++; continue; }
    // Devanagari (Hindi, Marathi, Nepali, Sanskrit)
    if ((cp >= 0x0900 && cp <= 0x097F) || (cp >= 0xA8E0 && cp <= 0xA8FF)) { devanagari++; continue; }
    // Bengali / Assamese
    if (cp >= 0x0980 && cp <= 0x09FF) { bengali++; continue; }
    // Gujarati
    if (cp >= 0x0A80 && cp <= 0x0AFF) { gujarati++; continue; }
    // Gurmukhi (Punjabi)
    if (cp >= 0x0A00 && cp <= 0x0A7F) { gurmukhi++; continue; }
    // Tamil
    if (cp >= 0x0B80 && cp <= 0x0BFF) { tamil++; continue; }
    // Telugu
    if (cp >= 0x0C00 && cp <= 0x0C7F) { telugu++; continue; }
    // Kannada
    if (cp >= 0x0C80 && cp <= 0x0CFF) { kannada++; continue; }
    // Malayalam
    if (cp >= 0x0D00 && cp <= 0x0D7F) { malayalam++; continue; }
    // Sinhala
    if (cp >= 0x0D80 && cp <= 0x0DFF) { sinhala++; continue; }
    // Odia (Oriya)
    if (cp >= 0x0B00 && cp <= 0x0B7F) { odia++; continue; }
    // Greek + Extended
    if ((cp >= 0x0370 && cp <= 0x03FF) || (cp >= 0x1F00 && cp <= 0x1FFF)) { greek++; continue; }
    // Armenian
    if ((cp >= 0x0530 && cp <= 0x058F) || (cp >= 0xFB00 && cp <= 0xFB17)) { armenian++; continue; }
    // Georgian
    if ((cp >= 0x10A0 && cp <= 0x10FF) || (cp >= 0x2D00 && cp <= 0x2D2F)) { georgian++; continue; }
    // Ethiopic (Amharic, Tigrinya)
    if ((cp >= 0x1200 && cp <= 0x137F) || (cp >= 0x1380 && cp <= 0x139F) ||
        (cp >= 0x2D80 && cp <= 0x2DDF)) { ethiopic++; continue; }
    // Tibetan
    if (cp >= 0x0F00 && cp <= 0x0FFF) { tibetan++; continue; }
    // Mongolian
    if (cp >= 0x1800 && cp <= 0x18AF) { mongolian++; continue; }
    // Bopomofo (Traditional Chinese phonetic — strong zh-TW signal)
    if ((cp >= 0x3100 && cp <= 0x312F) || (cp >= 0x31A0 && cp <= 0x31BF)) { bopomofo++; continue; }
    // Yi (Nuosu/彝族)
    if ((cp >= 0xA000 && cp <= 0xA48F) || (cp >= 0xA490 && cp <= 0xA4CF)) { yi++; continue; }
    // Meetei Mayek (Manipuri)
    if ((cp >= 0xABC0 && cp <= 0xABFF) || (cp >= 0xAAE0 && cp <= 0xAAFF)) { meeteiMayek++; continue; }
    // Ol Chiki (Santali)
    if (cp >= 0x1C50 && cp <= 0x1C7F) { olChiki++; continue; }
    // Lepcha (Sikkim)
    if (cp >= 0x1C00 && cp <= 0x1C4F) { lepcha++; continue; }
    // Limbu (Nepal/Sikkim)
    if (cp >= 0x1900 && cp <= 0x194F) { limbu++; continue; }
    // Chakma (Bangladesh/India)
    if (cp >= 0x11100 && cp <= 0x1114F) { chakma++; continue; }
    // Thaana (Dhivehi — Maldives)
    if (cp >= 0x0780 && cp <= 0x07BF) { thaana++; continue; }
    // N'Ko (Mandinka, Bambara — West Africa)
    if (cp >= 0x07C0 && cp <= 0x07FF) { nko++; continue; }
    // Javanese
    if (cp >= 0xA980 && cp <= 0xA9DF) { javanese++; continue; }
    // Balinese
    if (cp >= 0x1B00 && cp <= 0x1B7F) { balinese++; continue; }
    // Sundanese
    if ((cp >= 0x1B80 && cp <= 0x1BBF) || (cp >= 0x1CC0 && cp <= 0x1CCF)) { sundanese++; continue; }
    // Tai Le / New Tai Lue / Tai Tham (Dai languages in China/SE Asia)
    if ((cp >= 0x1950 && cp <= 0x197F) || (cp >= 0x1980 && cp <= 0x19DF) ||
        (cp >= 0x1A20 && cp <= 0x1AAF)) { taiLe++; continue; }
    // Cherokee
    if ((cp >= 0x13A0 && cp <= 0x13FF) || (cp >= 0xAB70 && cp <= 0xABBF)) { cherokee++; continue; }
    // Vietnamese diacritics
    if ('àáảãạăắằẳẵặâấầẩẫậèéẻẽẹêếềểễệìíỉĩịòóỏõọôốồổỗộơớờởỡợùúủũụưứừửữựỳýỷỹỵđ'
        .includes(ch.toLowerCase())) { vietnamese++; continue; }
    // Basic Latin
    if ((cp >= 0x0041 && cp <= 0x005A) || (cp >= 0x0061 && cp <= 0x007A)) { latin++; continue; }
  }

  // ── CJK disambiguation ──
  // Kana present → Japanese; Hangul present → Korean
  if (ja > 0) ja += zh;
  else if (ko > 0 && zh > 0 && ja === 0) ko += zh;

  // Chinese variant detection (Cantonese, Traditional, Mandarin, etc.)
  let zhLang = 'zh-CN'; // default: Mandarin Simplified
  if ((ja === 0 && ko === 0) && zh > 0) {
    const lower = clean.toLowerCase();
    // Bopomofo → Traditional Chinese (Taiwan)
    if (bopomofo > 0) zhLang = 'zh-TW';
    // Cantonese-specific particles and words (粵語)
    else if (/[嘅咗喺嘢咁佢哋啲嗰冇啱噉嗮唔嘥喎嚟]/.test(clean)) zhLang = 'yue-HK';
    // Traditional Chinese markers (commonly used only in Traditional)
    else if (/[們這國與學對書時點後說開過頭長義間無來機隊連還進運達裡線門體員關實際際歡歲覺導體雜聯環線類點雙費設訊記]/.test(clean)) zhLang = 'zh-TW';
    // Shanghainese / Wu dialect markers
    else if (/[侬伊拉覅勿阿拉]/.test(clean) && /\b(侬好|覅|勿要|阿拉)\b/.test(lower)) zhLang = 'wuu';
    // Min Nan / Hokkien (often uses specific characters)
    else if (/[𪜶汝伊咱甲毋閣攏]/.test(clean)) zhLang = 'nan-TW';
  }

  // ── Devanagari disambiguation (India) ──
  // Hindi vs Marathi vs Nepali vs Sanskrit vs Konkani vs Dogri vs Bodo vs Maithili
  let devLang = 'hi-IN'; // default: Hindi
  if (devanagari > 0) {
    const lower = clean;
    // Marathi-specific words and markers
    if (/\b(आहे|आहेत|नाही|काय|आणि|पण|हे|ते|मी|तू|तुम्ही|आम्ही|त्यांचा|मराठी)\b/.test(lower))
      devLang = 'mr-IN';
    // Nepali-specific words
    else if (/\b(छ|छन्|हुन्छ|गर्छ|भएको|गर्नु|तपाईं|हामी|उनीहरू|नेपाली|हुन्|भन्ने|गर्ने)\b/.test(lower))
      devLang = 'ne-NP';
    // Sanskrit markers
    else if (/\b(अस्ति|भवति|तथा|एव|अपि|च|तु|हि|इति|यत्|तत्|नमः|संस्कृत)\b/.test(lower))
      devLang = 'sa-IN';
    // Konkani
    else if (/\b(आसा|ना|हांव|तूं|तो|ती|आमी|कोंकणी)\b/.test(lower))
      devLang = 'kok-IN';
    // Dogri
    else if (/\b(ऐ|है|दा|दी|दे|कन्नै|डोगरी)\b/.test(lower))
      devLang = 'doi-IN';
    // Maithili
    else if (/\b(अछि|छथि|हम|अहाँ|ओ|मैथिली)\b/.test(lower))
      devLang = 'mai-IN';
    // Bodo
    else if (/\b(बड़ो|मोन|बर)\b/.test(lower))
      devLang = 'brx-IN';
  }

  // ── Bengali disambiguation: Bengali vs Assamese ──
  let bnLang = 'bn-BD';
  if (bengali > 0) {
    // Assamese-specific characters: ৰ (ra) and ৱ (wa) unique to Assamese
    if (/[ৰৱ]/.test(clean)) bnLang = 'as-IN';
    // Assamese words
    else if (/\b(আছে|নাই|হয়|এটা|অসমীয়া)\b/.test(clean)) bnLang = 'as-IN';
  }

  // ── Arabic disambiguation ──
  // Arabic vs Urdu vs Persian vs Kurdish vs Pashto vs Sindhi vs Uyghur
  let arLang = 'ar-SA';
  if (arabic > 0) {
    const lower = clean;
    // Urdu-specific: uses Arabic script + specific words/chars
    if (/[ٹڈڑںہھے]/.test(lower) || /\b(ہے|ہیں|کا|کی|کے|سے|نہیں|اور|لیکن|بھی|یہ|وہ|میں|ہم|تم|آپ)\b/.test(lower))
      arLang = 'ur-PK';
    // Persian (Farsi) — specific chars and words
    else if (/[پچژگ]/.test(lower) && /\b(است|هست|نیست|این|آن|من|تو|او|ما|شما|آنها|اما|و|یا|که|با|از|به|در|برای)\b/.test(lower))
      arLang = 'fa-IR';
    // Pashto — specific chars
    else if (/[ټډړ]/.test(lower) || /\b(دا|دی|دې|هغه|هغوی|زه|ته|مونږ|تاسو)\b/.test(lower))
      arLang = 'ps-AF';
    // Kurdish (Sorani — Arabic script)
    else if (/[ڕڵ]/.test(lower) || /\b(ئەو|ئەم|لە|بۆ|کە|دا|نەخێر|بەڵێ)\b/.test(lower))
      arLang = 'ckb-IQ';
    // Sindhi
    else if (/[ڄڃڦ]/.test(lower) || /\b(آهي|آهن|سنڌي|ڪري)\b/.test(lower))
      arLang = 'sd-PK';
    // Uyghur
    else if (/[ئا-ي]/.test(lower) && /\b(بولسا|ئەمەس|مەن|سەن|بىز|سىلەر|ئۇيغۇر)\b/.test(lower))
      arLang = 'ug-CN';
  }

  // ── Script-based scoring ──
  const scriptScores: [string, number][] = [
    ['ko-KR', ko],
    ['ja-JP', ja],
    [zhLang, (ja === 0 && ko === 0) ? zh : 0],
    ['el-GR', greek],
    ['hy-AM', armenian],
    ['ka-GE', georgian],
    ['he-IL', hebrew],
    [arLang, arabic],
    ['th-TH', thai],
    ['lo-LA', lao],
    ['km-KH', khmer],
    ['my-MM', myanmar],
    [devLang, devanagari],
    [bnLang, bengali],
    ['gu-IN', gujarati],
    ['pa-IN', gurmukhi],
    ['ta-IN', tamil],
    ['te-IN', telugu],
    ['kn-IN', kannada],
    ['ml-IN', malayalam],
    ['si-LK', sinhala],
    ['or-IN', odia],
    ['am-ET', ethiopic],
    ['bo-CN', tibetan],
    ['mn-MN', mongolian],
    ['vi-VN', vietnamese],
    // Additional scripts
    ['mni-IN', meeteiMayek],  // Manipuri (Meetei Mayek script)
    ['sat-IN', olChiki],      // Santali (Ol Chiki script)
    ['lep-IN', lepcha],       // Lepcha (Sikkim)
    ['lif-NP', limbu],        // Limbu
    ['ccp-BD', chakma],       // Chakma
    ['dv-MV', thaana],        // Dhivehi (Maldivian)
    ['nqo', nko],             // N'Ko (Mandinka/Bambara)
    ['jv-ID', javanese],      // Javanese (Javanese script)
    ['ban-ID', balinese],     // Balinese
    ['su-ID', sundanese],     // Sundanese (Sundanese script)
    ['ii-CN', yi],            // Yi/Nuosu (China)
    ['khb-CN', taiLe],       // Tai Lü / Tai Le (Dai, China)
    ['chr-US', cherokee],     // Cherokee
  ];

  // Cyrillic needs word-level disambiguation
  if (cyrillic > 0) {
    const lower = clean.toLowerCase();
    if (/[іїєґ]/.test(lower) || /\b(і|та|це|що|як|але|не|від|або|ще|їх|ці)\b/.test(lower))
      scriptScores.push(['uk-UA', cyrillic]);
    else if (/[ўі]/.test(lower) || /\b(і|ў|але|як|гэта|што|яна|яны|таксама)\b/.test(lower))
      scriptScores.push(['be-BY', cyrillic]);
    else if (/\b(и|на|от|за|се|да|не|с|е|по|от|тя|ли|бъ|ще)\b/.test(lower))
      scriptScores.push(['bg-BG', cyrillic]);
    else if (/\b(је|и|на|да|у|се|за|од|су|као|али|има|не|то|са)\b/.test(lower))
      scriptScores.push(['sr-RS', cyrillic]);
    else if (/[ңғүұқәөһ]/.test(lower))
      scriptScores.push(['kk-KZ', cyrillic]);
    else if (/[ӨҮ]/.test(clean) || /\b(бол|байна|энэ|тэр|бид|тэд|юу|яаж|хаана)\b/.test(lower))
      scriptScores.push(['mn-MN', cyrillic]); // Mongolian Cyrillic
    else if (/[ҷӣӯ]/.test(lower) || /\b(аст|нест|ман|ту|мо|шумо|онҳо|аммо|ва|ё|ки|бо|аз|ба|дар|барои)\b/.test(lower))
      scriptScores.push(['tg-TJ', cyrillic]); // Tajik
    else if (/[ңөү]/.test(lower) || /\b(бол|жок|бар|мен|сен|биз|алар|эмес|да|менен)\b/.test(lower))
      scriptScores.push(['ky-KG', cyrillic]); // Kyrgyz
    else
      scriptScores.push(['ru-RU', cyrillic]);
  }

  scriptScores.sort((a, b) => b[1] - a[1]);
  const topScore = scriptScores[0];
  if (topScore && topScore[1] > 0) return topScore[0];

  // ── Latin-script word-level heuristics ──
  if (latin === 0) return UNDETECTED_LANG;
  const lower = clean.toLowerCase();

  // Ordered from most distinctive to least (reduces false positives)
  const latinRules: [string, RegExp][] = [
    // Finnish — very distinctive
    ['fi-FI', /\b(ja|on|ei|se|hän|tämä|että|mutta|tai|niin|ovat|oli|olla|myös|kun|vain|hänen|siitä|minä|sinä|meillä|ääni|ään)\b/],
    // Hungarian — distinctive
    ['hu-HU', /\b(és|egy|az|nem|van|hogy|ezt|meg|volt|még|csak|már|mint|igen|lesz|vagy|itt|ott|ami|aki|nagyon|köszön)\b/],
    // Polish — distinctive diacritics
    ['pl-PL', /\b(nie|jest|się|na|to|jak|ale|czy|tak|już|też|może|tylko|gdzie|kiedy|bardzo|dobrze|proszę|dziękuję)\b/],
    // Czech
    ['cs-CZ', /\b(je|to|na|se|že|ale|jak|tak|jsem|není|jsou|byl|bude|může|tento|tato|toto|také|nebo|když|kde|kdo|proč)\b/],
    // Slovak
    ['sk-SK', /\b(je|na|sa|to|že|ale|som|nie|ako|tak|bol|bude|môže|tento|táto|toto|tiež|alebo|keď|kde|kto|prečo)\b/],
    // Romanian
    ['ro-RO', /\b(este|sunt|care|pentru|sau|dar|mai|cum|când|unde|acest|această|între|poate|acum|foarte|despre|prin|ați|sunt)\b/],
    // Croatian / Bosnian
    ['hr-HR', /\b(je|i|na|da|u|se|za|od|su|kao|ali|ima|ne|to|sa|ovaj|koji|što|može|biti)\b/],
    // Slovenian
    ['sl-SI', /\b(je|in|na|da|se|za|od|so|kot|ali|ne|to|ta|ki|lahko|biti|tudi|sem|smo|ste)\b/],
    // Lithuanian
    ['lt-LT', /\b(ir|yra|kad|bet|tai|su|ar|iš|jis|ji|mes|jie|jos|buvo|gali|labai|dabar|kaip|kur|kas|tik)\b/],
    // Latvian
    ['lv-LV', /\b(ir|un|ka|bet|tas|ar|no|vai|viņš|viņa|mēs|viņi|bija|var|ļoti|tagad|kā|kur|kas|tikai)\b/],
    // Estonian
    ['et-EE', /\b(on|ja|et|ei|see|mis|kui|aga|ka|veel|oli|olla|saab|väga|nüüd|kuidas|kus|kes|ainult)\b/],
    // Vietnamese (Latin-based with unique diacritics - already scored above but add word check)
    ['vi-VN', /\b(là|và|của|không|có|được|trong|cho|này|đã|với|một|những|các|từ|đó|người|khi|cũng)\b/],
    // Swahili
    ['sw-KE', /\b(ni|na|ya|wa|kwa|katika|hii|hiyo|au|lakini|pia|sana|kwamba|kama|hakuna|watu|nchi|moja|yake|wake)\b/],
    // Malay
    ['ms-MY', /\b(yang|dan|di|ini|itu|dengan|untuk|dari|pada|adalah|tidak|akan|sudah|boleh|kami|mereka|saya|anda|telah|juga)\b/],
    // Indonesian
    ['id-ID', /\b(yang|dan|di|ini|itu|dengan|untuk|dari|pada|adalah|tidak|akan|sudah|bisa|kami|mereka|saya|anda|juga|atau)\b/],
    // Tagalog / Filipino
    ['fil-PH', /\b(ang|ng|sa|na|at|ay|mga|ito|iyon|ko|mo|niya|kami|sila|hindi|oo|ano|kung|pero|din|lang|po)\b/],
    // Dutch
    ['nl-NL', /\b(de|het|een|en|van|in|is|dat|op|te|voor|met|niet|zijn|worden|ook|maar|als|nog|wel|deze|die|wat|naar)\b/],
    // Swedish
    ['sv-SE', /\b(och|det|är|en|att|för|som|med|den|har|jag|inte|var|kan|till|av|på|hade|från|men|hon|han|vi|dem)\b/],
    // Norwegian
    ['nb-NO', /\b(og|det|er|en|at|for|som|med|den|har|jeg|ikke|var|kan|til|av|på|hadde|fra|men|hun|han|vi|dem)\b/],
    // Danish
    ['da-DK', /\b(og|det|er|en|at|for|som|med|den|har|jeg|ikke|var|kan|til|af|på|havde|fra|men|hun|han|vi|dem)\b/],
    // Icelandic
    ['is-IS', /\b(og|er|að|það|sem|en|ekki|hann|hún|við|þeir|var|vera|geta|til|á|í|frá|með|um|þetta)\b/],
    // French
    ['fr-FR', /\b(le|la|les|un|une|des|est|sont|avec|dans|pour|que|qui|nous|vous|ils|elles|ce|cette|je|tu|il|elle|ne|pas|mais|et)\b/],
    // Spanish
    ['es-ES', /\b(el|los|las|una|unos|unas|es|son|está|están|con|por|para|que|como|pero|más|este|esta|yo|tú|él|ella|nosotros)\b/],
    // Portuguese
    ['pt-BR', /\b(o|os|as|um|uma|uns|umas|é|são|está|estão|com|por|para|que|como|mas|mais|este|esta|eu|tu|ele|ela|nós|vocês)\b/],
    // Italian
    ['it-IT', /\b(il|lo|la|gli|le|un|uno|una|è|sono|con|per|che|come|ma|più|questo|questa|io|tu|lui|lei|noi|voi|loro|anche|non)\b/],
    // German
    ['de-DE', /\b(der|die|das|ein|eine|ist|sind|haben|mit|und|oder|aber|für|von|ich|du|er|sie|wir|ihr|nicht|auch|noch)\b/],
    // Turkish
    ['tr-TR', /\b(bir|ve|bu|da|de|için|ile|ben|sen|biz|onlar|değil|var|yok|olan|gibi|ama|çok|daha)\b/],
    // Azerbaijani
    ['az-AZ', /\b(bir|və|bu|da|dəqiq|amma|ilə|mən|sən|biz|onlar|deyil|var|yox|olan|kimi|amma|çox|daha)\b/],
    // Uzbek (Latin)
    ['uz-UZ', /\b(va|bir|bu|ham|bilan|men|sen|biz|ular|emas|bor|yoq|kabi|lekin|juda|endi)\b/],
    // Catalan
    ['ca-ES', /\b(el|la|els|les|un|una|és|són|amb|per|que|com|però|més|aquest|aquesta|jo|tu|ell|ella|nosaltres|vosaltres)\b/],
    // Galician
    ['gl-ES', /\b(o|a|os|as|un|unha|é|son|con|por|para|que|como|pero|máis|este|esta|eu|ti|el|ela|nós|vós)\b/],
    // Basque
    ['eu-ES', /\b(da|eta|bat|ez|hau|hori|nire|zure|bere|gure|haien|baina|ere|oso|orain|nola|non|nor|bakarrik)\b/],
    // Welsh
    ['cy-GB', /\b(y|yr|a|i|yn|mae|ei|eu|ond|hefyd|gyda|gan|ar|am|o|fe|hi|ni|nhw|bod|wedi|roedd)\b/],
    // Irish Gaelic
    ['ga-IE', /\b(an|na|agus|is|ar|le|i|ag|go|tá|ní|sé|sí|muid|siad|ach|freisin|anois|conas|cá|cad)\b/],
    // Maltese
    ['mt-MT', /\b(il|u|ta|li|ma|jew|fuq|hija|huwa|aħna|huma|mhux|ukoll|issa|kif|fejn|min|biss)\b/],
    // Hausa
    ['ha-NG', /\b(da|na|ya|ta|ne|ce|ba|shi|ita|mu|su|amma|kuma|ko|sosai|yanzu|yadda|ina|wane|kawai)\b/],
    // Yoruba
    ['yo-NG', /\b(ni|ti|si|ati|ko|se|je|mo|re|wa|won|sugbon|pelu|gidigidi|bayi|bi|nibo|tani|nikan)\b/],
    // Zulu
    ['zu-ZA', /\b(ukuthi|futhi|kodwa|noma|kakhulu|manje|kanjani|kuphi|ubani|kuphela|yebo|cha)\b/],
    // Afrikaans
    ['af-ZA', /\b(die|en|van|in|is|dat|op|te|vir|met|nie|het|het|ook|maar|as|nog|hierdie|wat|na|hulle|ons)\b/],
    // Kurdish (Kurmanji — Latin script)
    ['ku-TR', /\b(ev|ew|û|li|bi|ji|de|e|ye|ne|ku|da|her|jî|yek|du|wek|dikare|dikim|divê)\b/],
    // Albanian
    ['sq-AL', /\b(është|janë|dhe|por|me|për|nga|në|ka|nuk|po|edhe|ose|ky|kjo|një|si|ku|kush|çfarë|shumë)\b/],
    // Somali
    ['so-SO', /\b(waa|iyo|oo|ama|laakiin|in|ka|ku|u|la|si|wuxuu|ayaa|tahay|yihiin|maaha|haa|maya|maxay|sidee)\b/],
    // Amharic (Latin romanization) — typically uses Ethiopic but some type in Latin
    ['am-ET', /\b(yehe|ena|gin|le|ke|new|aydelm|eshi|awo|betam|endet|yet|man|bcha)\b/],
    // Igbo
    ['ig-NG', /\b(na|bụ|nke|ma|ọ|ya|ha|anyị|gị|m|ndị|onye|ihe|dị|enweghị|ee|mba|gịnị|ebee)\b/],
    // Xhosa
    ['xh-ZA', /\b(ukuba|kunye|kodwa|okanye|kakhulu|ngoku|njani|phi|bani|kuphela|ewe|hayi)\b/],
    // Sesotho
    ['st-ZA', /\b(ke|le|ea|ka|ha|ho|ba|sa|tsa|hae|bona|rona|empa|hape|haholo|joale|jwang|kae|mang|feela)\b/],
    // Setswana
    ['tn-ZA', /\b(ke|le|ya|ka|ga|go|ba|sa|tsa|gagwe|bone|rona|mme|gape|thata|jaanong|jang|kae|mang|fela)\b/],
    // Shona
    ['sn-ZW', /\b(ndi|ne|ya|ka|ha|ku|va|sa|wake|vavo|wedu|asi|zvakare|kwazvo|zvino|sei|kupi|ani|chete)\b/],
    // Kinyarwanda
    ['rw-RW', /\b(ni|na|ya|mu|ku|ba|ntabwo|yego|oya|cyane|ubu|gute|hehe|nde|gusa)\b/],
    // Luxembourgish
    ['lb-LU', /\b(ass|an|op|mat|vun|fir|awer|och|nach|net|wéi|datt|ech|du|hien|si|mir|dir|si|wat|wou|wie)\b/],
    // Romansh
    ['rm-CH', /\b(è|ed|cun|per|da|en|il|la|els|ellas|in|ina|nus|vus|els|bun|grond|fitg|uss|co|nua|tgi|mo)\b/],
    // Maori
    ['mi-NZ', /\b(ko|te|he|i|ki|kei|me|ma|ka|e|nei|na|hoki|anō|tino|ināianei|pēhea|hea|wai|anake|ae|kāo)\b/],
    // Samoan
    ['sm-WS', /\b(o|le|i|ma|e|sa|ua|na|foi|ae|tele|nei|faapefea|fea|ai|naʻo|ioe|leai)\b/],
    // Hawaiian
    ['haw-US', /\b(ka|ke|o|i|ma|me|a|e|ua|he|nō|hoʻi|loa|kēia|pehea|hea|wai|wale|ʻae|ʻaʻole)\b/],
    // Tongan
    ['to-TO', /\b(ko|ʻa|ʻi|mo|ʻe|ne|kuo|na|foki|ka|lahi|ni|fēfē|fē|hai|pē|ʻio|ʻikai)\b/],
    // Fijian
    ['fj-FJ', /\b(na|ko|e|kei|mai|ki|sa|a|talega|ka|vakalevu|oqo|vakaevei|evei|cava|ga|io|sega)\b/],
    // Tok Pisin (Papua New Guinea)
    ['tpi-PG', /\b(em|long|bilong|na|ol|i|yu|mi|mipela|yupela|ol|tasol|tu|tru|nau|olsem|we|husat|tasol)\b/],
  ];

  for (const [lang, re] of latinRules) {
    if (re.test(lower)) return lang;
  }

  // If significant Latin chars but no heuristic match, default English
  if (latin > 3) return 'en-US';

  return UNDETECTED_LANG;
}

/** Create a SpeechRecognition instance (cross-browser) */
// eslint-disable-next-line @typescript-eslint/no-explicit-any
function createSpeechRecognition(lang: string) {
  const W = window as any; // eslint-disable-line @typescript-eslint/no-explicit-any
  const SpeechRecognition = W.SpeechRecognition ?? W.webkitSpeechRecognition;
  if (!SpeechRecognition) return null;
  const recognition = new SpeechRecognition();
  recognition.lang = lang;
  recognition.interimResults = true;   // Real-time transcription for speed
  recognition.continuous = true;       // Keep listening until explicitly stopped
  recognition.maxAlternatives = 1;     // Fastest: single best result
  return recognition;
}

/** Convert markdown content to a simple HTML document for export */
function markdownToHtmlDoc(content: string, title = 'Export'): string {
  const body = renderMarkdown(content);
  return `<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>${title}</title>
<style>body{font-family:sans-serif;max-width:800px;margin:2em auto;padding:0 1em;line-height:1.6}
pre{background:#f4f4f4;padding:1em;overflow-x:auto;border-radius:4px}
code{background:#f4f4f4;padding:0.2em 0.4em;border-radius:3px}
blockquote{border-left:4px solid #ddd;margin:0;padding:0 1em;color:#666}</style>
</head><body>${body}</body></html>`;
}

/** Export content as a .doc (HTML-based) file */
function exportToDoc(content: string) {
  const blob = new Blob(
    [`<html xmlns:o="urn:schemas-microsoft-com:office:office" xmlns:w="urn:schemas-microsoft-com:office:word" xmlns="http://www.w3.org/TR/REC-html40">
<head><meta charset="utf-8"><title>Export</title></head><body>${renderMarkdown(content)}</body></html>`],
    { type: 'application/msword' }
  );
  const url = URL.createObjectURL(blob);
  const a = document.createElement('a');
  a.href = url;
  a.download = `export_${Date.now()}.doc`;
  a.click();
  URL.revokeObjectURL(url);
}

/** Export content as a PDF via print dialog */
function exportToPdf(content: string) {
  const html = markdownToHtmlDoc(content, 'PDF Export');
  const win = window.open('', '_blank');
  if (!win) return;
  win.document.write(html);
  win.document.close();
  // Small delay to allow styles to load
  setTimeout(() => {
    win.print();
    // Close after print dialog is handled
    win.addEventListener('afterprint', () => win.close());
  }, 400);
}

/** TTS: read content aloud using Web Speech API */
function speakContent(content: string, lang: string, onEnd?: () => void) {
  // Strip markdown syntax for cleaner speech
  const plain = content
    .replace(/```[\s\S]*?```/g, ' (code block) ')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/\*([^*]+)\*/g, '$1')
    .replace(/#{1,6}\s*/g, '')
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/!\[([^\]]*)\]\([^)]+\)/g, '$1')
    .replace(/[-*_]{3,}/g, '')
    .trim();

  if (!plain) return;
  window.speechSynthesis.cancel();
  const utterance = new SpeechSynthesisUtterance(plain);
  utterance.lang = lang;
  utterance.rate = 1.0;
  if (onEnd) utterance.onend = onEnd;
  if (onEnd) utterance.onerror = onEnd;
  window.speechSynthesis.speak(utterance);
}

/** Copy button component */
function CopyButton({ content }: { content: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(content);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch {
      // Fallback for older browsers
      const textarea = document.createElement('textarea');
      textarea.value = content;
      textarea.style.position = 'fixed';
      textarea.style.opacity = '0';
      document.body.appendChild(textarea);
      textarea.select();
      document.execCommand('copy');
      document.body.removeChild(textarea);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    }
  }, [content]);

  return (
    <button
      onClick={handleCopy}
      className="inline-flex items-center gap-1 text-xs text-gray-500 hover:text-gray-300 transition-colors px-2 py-1 rounded hover:bg-gray-700/50"
      title="Copy as Markdown"
    >
      {copied ? (
        <>
          <Check className="h-3.5 w-3.5 text-green-400" />
          <span className="text-green-400">Copied</span>
        </>
      ) : (
        <>
          <Copy className="h-3.5 w-3.5" />
          <span>Copy</span>
        </>
      )}
    </button>
  );
}

/** Action buttons for agent messages: Copy, Doc export, PDF export, Listen */
function MessageActions({ content, lang }: { content: string; lang: string }) {
  const [speaking, setSpeaking] = useState(false);

  const handleListen = useCallback(() => {
    if (speaking) {
      window.speechSynthesis.cancel();
      setSpeaking(false);
    } else {
      setSpeaking(true);
      speakContent(content, lang, () => setSpeaking(false));
    }
  }, [content, lang, speaking]);

  // Stop speech if component unmounts
  useEffect(() => {
    return () => {
      if (speaking) window.speechSynthesis.cancel();
    };
  }, [speaking]);

  const btnClass =
    'inline-flex items-center gap-1 text-xs text-gray-500 hover:text-gray-300 transition-colors px-2 py-1 rounded hover:bg-gray-700/50';

  return (
    <div className="flex items-center gap-0.5 flex-wrap">
      <CopyButton content={content} />
      <button onClick={() => exportToDoc(content)} className={btnClass} title="Export to Doc">
        <FileText className="h-3.5 w-3.5" />
        <span>Doc</span>
      </button>
      <button onClick={() => exportToPdf(content)} className={btnClass} title="Export to PDF">
        <FileDown className="h-3.5 w-3.5" />
        <span>PDF</span>
      </button>
      <button onClick={handleListen} className={btnClass} title={speaking ? 'Stop listening' : 'Listen'}>
        {speaking ? (
          <>
            <VolumeX className="h-3.5 w-3.5 text-blue-400" />
            <span className="text-blue-400">Stop</span>
          </>
        ) : (
          <>
            <Volume2 className="h-3.5 w-3.5" />
            <span>Listen</span>
          </>
        )}
      </button>
    </div>
  );
}

/** Rendered markdown message component */
function MarkdownMessage({ content }: { content: string }) {
  const html = useMemo(() => renderMarkdown(content), [content]);

  return (
    <div
      className="markdown-body text-sm break-words"
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}

export default function AgentChat() {
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [typing, setTyping] = useState(false);
  const [connected, setConnected] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [listening, setListening] = useState(false);
  const [attachMenuOpen, setAttachMenuOpen] = useState(false);
  const [chatLang, setChatLang] = useState(() => navigator.language || 'en-US');
  const [voiceMode, setVoiceMode] = useState(false); // Persistent voice conversation mode

  const wsRef = useRef<WebSocketClient | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const pendingContentRef = useRef('');
  const recognitionRef = useRef<ReturnType<typeof createSpeechRecognition> | null>(null);
  const fileInputRef = useRef<HTMLInputElement>(null);
  const langLoadedRef = useRef(false);
  const voiceModeRef = useRef(false);             // Sync ref for async callbacks
  const chatLangRef = useRef(chatLang);            // Sync ref for lang in callbacks

  // Keep refs in sync with state
  useEffect(() => { voiceModeRef.current = voiceMode; }, [voiceMode]);
  useEffect(() => { chatLangRef.current = chatLang; }, [chatLang]);

  // Load saved language preference on mount
  useEffect(() => {
    if (langLoadedRef.current) return;
    langLoadedRef.current = true;
    loadLangFromMemory().then((saved) => {
      if (saved) setChatLang(saved);
    });
  }, []);

  useEffect(() => {
    const ws = new WebSocketClient();

    ws.onOpen = () => {
      setConnected(true);
      setError(null);
    };

    ws.onClose = () => {
      setConnected(false);
    };

    ws.onError = () => {
      setError('Connection error. Attempting to reconnect...');
    };

    ws.onMessage = (msg: WsMessage) => {
      switch (msg.type) {
        case 'history': {
          const restored: ChatMessage[] = (msg.messages ?? [])
            .filter((entry) => entry.content?.trim())
            .map((entry) => ({
              id: makeMessageId(),
              role: (entry.role === 'user' ? 'user' : 'agent') as 'user' | 'agent',
              content: entry.content.trim(),
              timestamp: new Date(),
            }));

          setMessages(restored);
          setTyping(false);
          pendingContentRef.current = '';
          break;
        }

        case 'chunk':
          setTyping(true);
          pendingContentRef.current += msg.content ?? '';
          break;

        case 'message':
        case 'done': {
          const content = (msg.full_response ?? msg.content ?? pendingContentRef.current ?? '').trim();
          const finalContent = content || EMPTY_DONE_FALLBACK;

          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: finalContent,
              timestamp: new Date(),
            },
          ]);

          pendingContentRef.current = '';
          setTyping(false);

          // Auto-TTS in voice mode: speak the response, then resume STT
          if (voiceModeRef.current && finalContent !== EMPTY_DONE_FALLBACK) {
            // Pause STT while speaking to avoid echo
            recognitionRef.current?.stop();
            speakContent(finalContent, chatLangRef.current, () => {
              // Resume STT after speech ends (if still in voice mode)
              if (voiceModeRef.current) {
                startListening(chatLangRef.current);
              }
            });
          }
          break;
        }

        case 'tool_call':
          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: `\`[Tool Call]\` **${msg.name ?? 'unknown'}**\n\`\`\`json\n${JSON.stringify(msg.args ?? {}, null, 2)}\n\`\`\``,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'tool_result':
          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: `\`[Tool Result]\`\n\`\`\`\n${msg.output ?? ''}\n\`\`\``,
              timestamp: new Date(),
            },
          ]);
          break;

        case 'error': {
          const errorText = msg.message ?? 'Unknown error';
          const isApiKeyError =
            msg.code === 'missing_api_key' || msg.code === 'provider_auth_error';
          const displayContent = isApiKeyError
            ? `**[API Key Error]** ${errorText}\n\nPlease configure your API key in Settings → Integrations.`
            : `**[Error]** ${errorText}`;

          setMessages((prev) => [
            ...prev,
            {
              id: makeMessageId(),
              role: 'agent',
              content: displayContent,
              timestamp: new Date(),
            },
          ]);
          setTyping(false);
          pendingContentRef.current = '';
          break;
        }
      }
    };

    ws.connect();
    wsRef.current = ws;

    return () => {
      ws.disconnect();
    };
  }, []);

  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages, typing]);

  const handleSend = useCallback(() => {
    const trimmed = input.trim();
    if (!trimmed || !wsRef.current?.connected) return;

    // Detect language from user's message and update session language
    const detected = detectLanguage(trimmed);
    if (detected === UNDETECTED_LANG) {
      // First message and can't detect — ask user
      if (messages.length === 0) {
        setMessages((prev) => [
          ...prev,
          { id: makeMessageId(), role: 'user', content: trimmed, timestamp: new Date() },
          {
            id: makeMessageId(),
            role: 'agent',
            content: "I couldn't detect your language. Which language would you like me to respond in?\n\n(Please type your answer in your preferred language, e.g. \"한국어\", \"日本語\", \"Français\", \"Español\", etc.)",
            timestamp: new Date(),
          },
        ]);
        setInput('');
        return;
      }
      // Otherwise keep current lang
    } else if (detected !== chatLang) {
      setChatLang(detected);
      persistLangToMemory(detected);
    }

    setMessages((prev) => [
      ...prev,
      {
        id: makeMessageId(),
        role: 'user',
        content: trimmed,
        timestamp: new Date(),
      },
    ]);

    try {
      wsRef.current.sendMessage(trimmed);
      setTyping(true);
      pendingContentRef.current = '';
    } catch {
      setError('Failed to send message. Please try again.');
    }

    setInput('');
    inputRef.current?.focus();
  }, [input, messages.length, chatLang]);

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter' && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  const handleNewChat = () => {
    if (!wsRef.current) return;
    setMessages([]);
    setTyping(false);
    setError(null);
    pendingContentRef.current = '';
    wsRef.current.resetSession();
    inputRef.current?.focus();
  };

  // --- STT (Speech-to-Text) with continuous voice mode ---

  /** Start STT listening (internal helper). */
  const startListening = useCallback((lang: string) => {
    const recognition = createSpeechRecognition(lang);
    if (!recognition) {
      setError('Speech recognition is not supported in this browser.');
      return;
    }

    let finalTranscript = '';

    recognition.onresult = (event: { results: SpeechRecognitionResultList }) => {
      let interim = '';
      finalTranscript = '';
      for (let i = 0; i < event.results.length; i++) {
        const r = event.results[i];
        if (!r) continue;
        const alt = r[0];
        if (!alt) continue;
        if (r.isFinal) {
          finalTranscript += alt.transcript;
        } else {
          interim += alt.transcript;
        }
      }
      // Show real-time transcription (final + interim)
      setInput(finalTranscript + (interim ? interim : ''));
    };

    recognition.onerror = (e: { error?: string }) => {
      // 'no-speech' and 'aborted' are not real errors — auto-restart in voice mode
      if (e.error === 'no-speech' || e.error === 'aborted') return;
      setListening(false);
      setVoiceMode(false);
    };

    recognition.onend = () => {
      // In voice mode: auto-send final transcript, then the response handler
      // will restart listening after TTS completes
      if (voiceModeRef.current && finalTranscript.trim()) {
        setInput(finalTranscript.trim());
        // Trigger send on next tick so state is updated
        setTimeout(() => {
          const sendBtn = document.querySelector('[data-voice-send]') as HTMLButtonElement | null;
          sendBtn?.click();
        }, 50);
      } else if (voiceModeRef.current) {
        // No speech detected but still in voice mode — restart
        setTimeout(() => {
          if (voiceModeRef.current) startListening(chatLangRef.current);
        }, 300);
      } else {
        setListening(false);
      }
    };

    recognitionRef.current = recognition;
    recognition.start();
    setListening(true);
  }, []);

  /** Toggle voice mode on/off. */
  const toggleListening = useCallback(() => {
    if (listening || voiceMode) {
      // Stop everything
      recognitionRef.current?.stop();
      window.speechSynthesis.cancel();
      setListening(false);
      setVoiceMode(false);
      return;
    }

    // Enter voice mode: continuous STT ↔ TTS loop
    setVoiceMode(true);
    startListening(chatLang);
  }, [listening, voiceMode, chatLang, startListening]);

  // Cleanup STT + TTS on unmount
  useEffect(() => {
    return () => {
      recognitionRef.current?.stop();
      window.speechSynthesis.cancel();
    };
  }, []);

  // Close attach menu when clicking outside
  useEffect(() => {
    if (!attachMenuOpen) return;
    const handleClick = () => setAttachMenuOpen(false);
    // Delay to avoid closing immediately on the same click
    const timer = setTimeout(() => document.addEventListener('click', handleClick), 0);
    return () => {
      clearTimeout(timer);
      document.removeEventListener('click', handleClick);
    };
  }, [attachMenuOpen]);

  // --- File attachment handler ---
  const handleFileAttach = useCallback(() => {
    fileInputRef.current?.click();
  }, []);

  const handleFileSelected = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const files = e.target.files;
    if (!files?.length) return;
    const names = Array.from(files).map((f) => f.name).join(', ');
    setInput((prev) => (prev ? prev + `\n[Attached: ${names}]` : `[Attached: ${names}]`));
    // Reset so same file can be re-selected
    e.target.value = '';
    setAttachMenuOpen(false);
  }, []);

  // --- Local folder connection ---
  const handleFolderConnect = useCallback(async () => {
    setAttachMenuOpen(false);
    try {
      if (!('showDirectoryPicker' in window)) {
        setError('Folder selection is not supported in this browser. Use Chrome or Edge.');
        return;
      }
      const dirHandle = await (window as unknown as { showDirectoryPicker: () => Promise<FileSystemDirectoryHandle> }).showDirectoryPicker();
      setInput((prev) => (prev ? prev + `\n[Local folder: ${dirHandle.name}]` : `[Local folder: ${dirHandle.name}]`));
    } catch {
      // User cancelled - ignore
    }
  }, []);

  // --- GitHub repo connection ---
  const handleGithubConnect = useCallback(() => {
    setAttachMenuOpen(false);
    const repo = prompt('Enter GitHub repository URL or owner/repo:');
    if (repo?.trim()) {
      setInput((prev) => (prev ? prev + `\n[GitHub: ${repo.trim()}]` : `[GitHub: ${repo.trim()}]`));
    }
  }, []);

  return (
    <div className="flex flex-col h-[calc(100vh-3.5rem)]">
      {/* Chat header with New Chat button */}
      <div className="flex items-center justify-between px-4 py-2 border-b border-gray-800 bg-gray-900/80">
        <div className="flex items-center gap-2">
          <Bot className="h-5 w-5 text-gray-400" />
          <span className="text-sm font-medium text-gray-300">Agent Chat</span>
        </div>
        <button
          onClick={handleNewChat}
          className="inline-flex items-center gap-1.5 text-sm text-gray-400 hover:text-white px-3 py-1.5 rounded-lg hover:bg-gray-700/60 transition-colors"
          title="New Chat"
        >
          <SquarePen className="h-4 w-4" />
          <span>New Chat</span>
        </button>
      </div>

      {/* Connection status bar */}
      {error && (
        <div className="px-4 py-2 bg-red-900/30 border-b border-red-700 flex items-center gap-2 text-sm text-red-300">
          <AlertCircle className="h-4 w-4 flex-shrink-0" />
          {error}
        </div>
      )}

      {/* Messages area */}
      <div className="flex-1 overflow-y-auto p-4 space-y-4">
        {messages.length === 0 && (
          <div className="flex flex-col items-center justify-center h-full text-gray-500">
            <Bot className="h-12 w-12 mb-3 text-gray-600" />
            <p className="text-lg font-medium">MoA Agent</p>
            <p className="text-sm mt-1">Send a message to start the conversation</p>
          </div>
        )}

        {messages.map((msg) => (
          <div
            key={msg.id}
            className={`flex items-start gap-3 ${
              msg.role === 'user' ? 'flex-row-reverse' : ''
            }`}
          >
            <div
              className={`flex-shrink-0 w-8 h-8 rounded-full flex items-center justify-center ${
                msg.role === 'user'
                  ? 'bg-blue-600'
                  : 'bg-gray-700'
              }`}
            >
              {msg.role === 'user' ? (
                <User className="h-4 w-4 text-white" />
              ) : (
                <Bot className="h-4 w-4 text-white" />
              )}
            </div>
            <div
              className={`max-w-[75%] rounded-xl px-4 py-3 ${
                msg.role === 'user'
                  ? 'bg-blue-600 text-white'
                  : 'bg-gray-800 text-gray-100 border border-gray-700'
              }`}
            >
              {msg.role === 'user' ? (
                <p className="text-sm whitespace-pre-wrap break-words">{msg.content}</p>
              ) : (
                <MarkdownMessage content={msg.content} />
              )}
              <div className={`flex items-center justify-between mt-2 ${
                msg.role === 'user' ? '' : 'border-t border-gray-700/50 pt-1.5'
              }`}>
                <p
                  className={`text-xs ${
                    msg.role === 'user' ? 'text-blue-200' : 'text-gray-500'
                  }`}
                >
                  {msg.timestamp.toLocaleTimeString()}
                </p>
                {msg.role === 'agent' && (
                  <MessageActions content={msg.content} lang={chatLang} />
                )}
              </div>
            </div>
          </div>
        ))}

        {typing && (
          <div className="flex items-start gap-3">
            <div className="flex-shrink-0 w-8 h-8 rounded-full bg-gray-700 flex items-center justify-center">
              <Bot className="h-4 w-4 text-white" />
            </div>
            <div className="bg-gray-800 border border-gray-700 rounded-xl px-4 py-3">
              <div className="flex items-center gap-1">
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '0ms' }} />
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '150ms' }} />
                <span className="w-2 h-2 bg-gray-400 rounded-full animate-bounce" style={{ animationDelay: '300ms' }} />
              </div>
              <p className="text-xs text-gray-500 mt-1">Typing...</p>
            </div>
          </div>
        )}

        <div ref={messagesEndRef} />
      </div>

      {/* Input area */}
      <div className="border-t border-gray-800 bg-gray-900 p-4">
        <div className="flex items-center gap-2 max-w-4xl mx-auto">
          {/* Left: Attachment menu */}
          <div className="relative flex-shrink-0">
            <button
              onClick={() => setAttachMenuOpen((v) => !v)}
              className="p-2.5 rounded-xl text-gray-400 hover:text-white hover:bg-gray-700/60 transition-colors"
              title="Attach file / Connect source"
            >
              <Paperclip className="h-5 w-5" />
            </button>
            {attachMenuOpen && (
              <div className="absolute bottom-full left-0 mb-2 bg-gray-800 border border-gray-700 rounded-xl shadow-xl py-1 min-w-[200px] z-50">
                <button
                  onClick={handleFileAttach}
                  className="w-full flex items-center gap-2.5 px-4 py-2.5 text-sm text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
                >
                  <Paperclip className="h-4 w-4" />
                  <span>Attach File</span>
                </button>
                <button
                  onClick={handleFolderConnect}
                  className="w-full flex items-center gap-2.5 px-4 py-2.5 text-sm text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
                >
                  <FolderOpen className="h-4 w-4" />
                  <span>Connect Local Folder</span>
                </button>
                <button
                  onClick={handleGithubConnect}
                  className="w-full flex items-center gap-2.5 px-4 py-2.5 text-sm text-gray-300 hover:bg-gray-700 hover:text-white transition-colors"
                >
                  <Github className="h-4 w-4" />
                  <span>Connect GitHub</span>
                </button>
              </div>
            )}
            <input
              ref={fileInputRef}
              type="file"
              multiple
              className="hidden"
              onChange={handleFileSelected}
            />
          </div>

          {/* Center: Text input */}
          <div className="flex-1 relative">
            <input
              ref={inputRef}
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              placeholder={connected ? 'Type a message...' : 'Connecting...'}
              disabled={!connected}
              className="w-full bg-gray-800 border border-gray-700 rounded-xl px-4 py-3 text-sm text-white placeholder-gray-500 focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent disabled:opacity-50"
            />
          </div>

          {/* Right: Mic (STT/Voice mode) button */}
          <button
            onClick={toggleListening}
            disabled={!connected}
            className={`flex-shrink-0 p-2.5 rounded-xl transition-colors ${
              voiceMode
                ? 'bg-red-600 text-white animate-pulse'
                : listening
                  ? 'bg-orange-500 text-white animate-pulse'
                  : 'text-gray-400 hover:text-white hover:bg-gray-700/60'
            } disabled:opacity-50 disabled:cursor-not-allowed`}
            title={voiceMode ? 'Stop voice mode (STT+TTS)' : 'Start voice mode (STT+TTS)'}
          >
            {voiceMode || listening ? <MicOff className="h-5 w-5" /> : <Mic className="h-5 w-5" />}
          </button>

          {/* Send button */}
          <button
            data-voice-send
            onClick={handleSend}
            disabled={!connected || !input.trim()}
            className="flex-shrink-0 bg-blue-600 hover:bg-blue-700 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded-xl p-3 transition-colors"
          >
            <Send className="h-5 w-5" />
          </button>
        </div>
        <div className="flex items-center justify-center mt-2 gap-3">
          <div className="flex items-center gap-1.5">
            <span
              className={`inline-block h-2 w-2 rounded-full ${
                connected ? 'bg-green-500' : 'bg-red-500'
              }`}
            />
            <span className="text-xs text-gray-500">
              {connected ? 'Connected' : 'Disconnected'}
            </span>
          </div>
          {voiceMode && (
            <span className="text-xs text-red-400 animate-pulse">
              Voice mode ({((chatLang ?? 'en').split('-')[0] ?? 'en').toUpperCase()})
            </span>
          )}
        </div>
      </div>
    </div>
  );
}
