//! Localized re-ask / confirmation messages for the voice-chat
//! self-validation pipeline (`voice_chat_pipeline`).
//!
//! When the pipeline decides to ask the speaker to repeat or to
//! confirm Gemma's interpretation, the message must be rendered in
//! the **speaker's** own language so a Korean user hears Korean and
//! an English user hears English. In bidirectional interpretation
//! this flips per-turn — which is why the catalog is keyed by the
//! per-turn detected `LanguageCode`, not by a global session locale.
//!
//! # Coverage
//!
//! The catalog has natural-sounding translations for the 25 languages
//! the original voice pipeline shipped with (Korean, Japanese, the
//! major European and Southeast-Asian languages, Hindi, Arabic). For
//! the languages added in the 2026-05 expansion (Bengali, Tamil,
//! Hebrew, Greek, Armenian, Georgian, …) and any other unrecognized
//! `LanguageCode` variant, callers fall through to English. That is
//! intentional — it is better to deliver a comprehensible English
//! re-ask than to leave the user in silence, and a future PR can
//! extend the catalog without changing any caller.
//!
//! # Stability
//!
//! These string literals are user-facing copy. Edits to them ship
//! straight to the speaker's TTS output. Treat them like UI strings:
//! preserve tone, do not introduce slang, keep the formal register.

use super::pipeline::LanguageCode;

/// First-attempt re-ask phrase (the `AskUserToRepeat` route).
///
/// Spoken to the user when Gemma was uncertain it understood the
/// audio — equivalent to a human asking "sorry, I didn't catch that"
/// in a noisy room. The user can either re-speak more clearly or
/// switch to typed input.
pub fn ask_user_to_repeat(lang: LanguageCode) -> &'static str {
    match lang.as_str() {
        "ko" => "잘 들리지 않습니다. 혹시 다시 말씀해주시거나 \
                 아니면 텍스트로 입력해주시면 감사하겠습니다.",
        "ja" => "うまく聞き取れませんでした。もう一度お話しいただくか、\
                 テキストで入力していただけますと幸いです。",
        "zh" => "我没有听清楚。请再说一遍,或者用文字输入,谢谢。",
        "zh-TW" => "我沒有聽清楚。請再說一遍,或者用文字輸入,謝謝。",
        "es" => "No le he entendido bien. ¿Podría repetirlo, \
                 o bien escribirlo en texto? Gracias.",
        "fr" => "Je n'ai pas bien compris. Pourriez-vous répéter, \
                 ou bien le saisir en texte ? Merci.",
        "de" => "Ich habe Sie nicht richtig verstanden. \
                 Könnten Sie es bitte wiederholen oder als Text eingeben? Danke.",
        "it" => "Non ho capito bene. Potrebbe ripeterlo, \
                 oppure scriverlo come testo? Grazie.",
        "pt" => "Não consegui entender bem. Poderia repetir, \
                 ou então digitar em texto? Obrigado.",
        "nl" => "Ik heb u niet goed verstaan. Kunt u het herhalen, \
                 of als tekst invoeren? Dank u.",
        "pl" => "Nie zrozumiałem dokładnie. Czy mógłby Pan powtórzyć, \
                 lub wpisać to jako tekst? Dziękuję.",
        "cs" => "Nerozuměl jsem dobře. Mohl byste to zopakovat, \
                 nebo to napsat jako text? Děkuji.",
        "sv" => "Jag hörde inte riktigt. Kan du upprepa, \
                 eller skriva det som text? Tack.",
        "da" => "Jeg hørte ikke helt. Kan du gentage, \
                 eller skrive det som tekst? Tak.",
        "ru" => "Я не расслышал. Не могли бы вы повторить, \
                 или ввести это в виде текста? Спасибо.",
        "uk" => "Я не розчув. Чи не могли б ви повторити, \
                 або ввести це у вигляді тексту? Дякую.",
        "tr" => "Tam olarak anlayamadım. Lütfen tekrar eder misiniz, \
                 ya da metin olarak yazar mısınız? Teşekkürler.",
        "ar" => "لم أفهم جيدًا. هل يمكنك إعادة القول، \
                 أو كتابته كنص؟ شكرًا لك.",
        "th" => "ฉันฟังไม่ชัดเจน กรุณาพูดอีกครั้ง \
                 หรือพิมพ์เป็นข้อความ ขอบคุณค่ะ",
        "vi" => "Tôi nghe không rõ. Xin vui lòng nói lại, \
                 hoặc nhập bằng văn bản. Cảm ơn.",
        "id" => "Saya tidak menangkapnya dengan jelas. Tolong ulangi, \
                 atau ketik sebagai teks. Terima kasih.",
        "ms" => "Saya tidak dapat menangkap dengan jelas. Tolong ulang, \
                 atau taip sebagai teks. Terima kasih.",
        "tl" => "Hindi ko maririnig nang malinaw. Pakiulit po, \
                 o i-type bilang teksto. Salamat.",
        "hi" => "मैं स्पष्ट रूप से नहीं सुन सका। कृपया फिर से कहें, \
                 या इसे टेक्स्ट में लिखें। धन्यवाद।",
        // English + everything else falls through to English.
        _ => "I didn't catch that. Could you please say it again, \
              or type it as text? Thank you.",
    }
}

/// Second-attempt confirmation prefix (the `ConfirmInterpretation`
/// route).
///
/// The pipeline appends `" '<paraphrase>'"` after this string, where
/// `paraphrase` is Gemma's best reading of what the speaker said.
/// Spoken/displayed to the user so they can confirm or correct
/// before the (potentially expensive) cloud-LLM call goes out.
pub fn confirm_interpretation_prefix(lang: LanguageCode) -> &'static str {
    match lang.as_str() {
        "ko" => "혹시 이렇게 이해하는 것이 맞습니까?",
        "ja" => "もしかして、このように理解してよろしいでしょうか?",
        "zh" => "请问我这样理解对吗?",
        "zh-TW" => "請問我這樣理解對嗎?",
        "es" => "¿Es esto lo que quería decir?",
        "fr" => "Est-ce bien ce que vous vouliez dire ?",
        "de" => "Habe ich Sie richtig verstanden?",
        "it" => "È questo che intendeva dire?",
        "pt" => "É isto que queria dizer?",
        "nl" => "Bedoelde u dit?",
        "pl" => "Czy o to chodziło?",
        "cs" => "Měl jste na mysli toto?",
        "sv" => "Menade du detta?",
        "da" => "Mente du dette?",
        "ru" => "Вы это имели в виду?",
        "uk" => "Ви це мали на увазі?",
        "tr" => "Bunu mu kastettiniz?",
        "ar" => "هل كنت تقصد هذا؟",
        "th" => "คุณหมายถึงแบบนี้ใช่ไหมคะ?",
        "vi" => "Có phải ý bạn là thế này không?",
        "id" => "Apakah ini yang Anda maksud?",
        "ms" => "Adakah ini yang anda maksudkan?",
        "tl" => "Ito po ba ang ibig ninyong sabihin?",
        "hi" => "क्या आपका मतलब यह था?",
        _ => "Did you mean this?",
    }
}

/// Fallback message used when the route is `ConfirmInterpretation`
/// but Gemma did not produce any paraphrase (empty
/// `interpreted_meaning` field). Spoken in the speaker's own
/// language so the failure feels like a continuation of the
/// conversation rather than an error pop-up.
pub fn confirm_interpretation_fallback(lang: LanguageCode) -> &'static str {
    match lang.as_str() {
        "ko" => "여전히 잘 들리지 않습니다. \
                 텍스트로 입력해주시면 더 정확하게 도와드릴 수 있습니다.",
        "ja" => "まだうまく聞き取れません。\
                 テキストで入力していただけると、より正確にお手伝いできます。",
        "zh" => "我还是听不清楚。请用文字输入,这样我可以更准确地帮助您。",
        "zh-TW" => "我還是聽不清楚。請用文字輸入,這樣我可以更準確地幫助您。",
        "es" => "Sigo sin entender bien. \
                 Si lo escribe como texto, podré ayudarle con más precisión.",
        "fr" => "Je n'arrive toujours pas à comprendre. \
                 Si vous le saisissez en texte, je pourrai vous aider plus précisément.",
        "de" => "Ich kann es immer noch nicht richtig verstehen. \
                 Wenn Sie es als Text eingeben, kann ich Ihnen genauer helfen.",
        "it" => "Continuo a non capire bene. \
                 Se lo scrive come testo, potrò aiutarla con più precisione.",
        "pt" => "Continuo sem entender bem. \
                 Se digitar em texto, poderei ajudá-lo com mais precisão.",
        "nl" => "Ik versta het nog steeds niet goed. \
                 Als u het als tekst invoert, kan ik u nauwkeuriger helpen.",
        "pl" => "Nadal nie rozumiem dokładnie. \
                 Jeśli wpisze to Pan jako tekst, mogę pomóc dokładniej.",
        "cs" => "Stále nerozumím dobře. \
                 Pokud to napíšete jako text, mohu vám pomoci přesněji.",
        "sv" => "Jag hör fortfarande inte tydligt. \
                 Om du skriver det som text kan jag hjälpa dig mer exakt.",
        "da" => "Jeg hører stadig ikke tydeligt. \
                 Hvis du skriver det som tekst, kan jeg hjælpe dig mere præcist.",
        "ru" => "Я по-прежнему не могу разобрать. \
                 Если введёте текстом, я смогу помочь точнее.",
        "uk" => "Я все ще не розумію. \
                 Якщо введете текстом, я зможу допомогти точніше.",
        "tr" => "Hâlâ tam olarak anlayamıyorum. \
                 Metin olarak yazarsanız size daha doğru yardımcı olabilirim.",
        "ar" => "ما زلت لا أفهم بوضوح. \
                 إذا كتبته كنص، يمكنني مساعدتك بدقة أكبر.",
        "th" => "ฉันยังฟังไม่ชัดเจน \
                 หากพิมพ์เป็นข้อความ ฉันจะช่วยคุณได้แม่นยำยิ่งขึ้น",
        "vi" => "Tôi vẫn nghe không rõ. \
                 Nếu bạn nhập bằng văn bản, tôi có thể giúp bạn chính xác hơn.",
        "id" => "Saya masih tidak menangkap dengan jelas. \
                 Jika Anda mengetiknya, saya dapat membantu lebih akurat.",
        "ms" => "Saya masih tidak dapat menangkap dengan jelas. \
                 Jika anda menaipnya, saya boleh membantu dengan lebih tepat.",
        "tl" => "Hindi ko pa rin maririnig nang malinaw. \
                 Kung ita-type ninyo, mas tumpak kong matutulungan kayo.",
        "hi" => "मुझे अभी भी स्पष्ट रूप से नहीं सुनाई दे रहा। \
                 यदि आप इसे टेक्स्ट में लिखें, तो मैं आपकी अधिक सटीक सहायता कर सकता हूँ।",
        _ => "I still can't quite catch it. \
              If you type it as text, I can help you more accurately.",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── Spot checks: each helper returns the right language for a
    //    handful of representative codes. Quicker to read than the
    //    coverage harness below and useful when bisecting a regression.

    #[test]
    fn ask_user_to_repeat_returns_korean_for_ko() {
        assert!(ask_user_to_repeat(LanguageCode::Ko).contains("잘 들리지 않습니다"));
    }

    #[test]
    fn ask_user_to_repeat_returns_japanese_for_ja() {
        assert!(ask_user_to_repeat(LanguageCode::Ja).contains("聞き取れません"));
    }

    #[test]
    fn ask_user_to_repeat_returns_english_for_en() {
        assert!(ask_user_to_repeat(LanguageCode::En).contains("I didn't catch that"));
    }

    #[test]
    fn ask_user_to_repeat_falls_back_to_english_for_unsupported_locale() {
        // Bengali was added in the 2026-05 expansion but is not yet
        // in the message catalog — must fall through to English so
        // the user gets a comprehensible re-ask rather than silence.
        assert_eq!(
            ask_user_to_repeat(LanguageCode::Bn),
            ask_user_to_repeat(LanguageCode::En),
        );
    }

    #[test]
    fn confirm_interpretation_prefix_returns_korean_for_ko() {
        assert!(confirm_interpretation_prefix(LanguageCode::Ko)
            .contains("이렇게 이해하는 것이 맞습니까"));
    }

    #[test]
    fn confirm_interpretation_prefix_returns_arabic_for_ar() {
        assert!(confirm_interpretation_prefix(LanguageCode::Ar).contains("هل كنت تقصد"));
    }

    #[test]
    fn confirm_interpretation_fallback_returns_traditional_chinese_for_zh_tw() {
        // Verify the Traditional-Chinese form is used (uses 還 not 还).
        let msg = confirm_interpretation_fallback(LanguageCode::ZhTw);
        assert!(msg.contains("還是"));
        assert!(!msg.contains("还是"));
    }

    // ── Catalog-coverage harness: every variant in the 25-language
    //    "catalog" set must produce a non-fallback string in *all
    //    three* helpers. New additions to the catalog should be
    //    appended to `CATALOG_LANGUAGES` below.

    const CATALOG_LANGUAGES: &[LanguageCode] = &[
        LanguageCode::Ko,
        LanguageCode::Ja,
        LanguageCode::Zh,
        LanguageCode::ZhTw,
        LanguageCode::Es,
        LanguageCode::Fr,
        LanguageCode::De,
        LanguageCode::It,
        LanguageCode::Pt,
        LanguageCode::Nl,
        LanguageCode::Pl,
        LanguageCode::Cs,
        LanguageCode::Sv,
        LanguageCode::Da,
        LanguageCode::Ru,
        LanguageCode::Uk,
        LanguageCode::Tr,
        LanguageCode::Ar,
        LanguageCode::Th,
        LanguageCode::Vi,
        LanguageCode::Id,
        LanguageCode::Ms,
        LanguageCode::Tl,
        LanguageCode::Hi,
    ];

    #[test]
    fn all_catalog_languages_have_native_translations() {
        // English fallback markers — anyone in the catalog must NOT
        // produce a string that starts with these markers (which
        // indicate the `_ =>` arm fired).
        for lang in CATALOG_LANGUAGES {
            let m1 = ask_user_to_repeat(*lang);
            let m2 = confirm_interpretation_prefix(*lang);
            let m3 = confirm_interpretation_fallback(*lang);

            assert!(
                !m1.starts_with("I didn't catch"),
                "{} fell through to English in ask_user_to_repeat",
                lang.as_str()
            );
            assert!(
                !m2.starts_with("Did you mean"),
                "{} fell through to English in confirm_interpretation_prefix",
                lang.as_str()
            );
            assert!(
                !m3.starts_with("I still can't"),
                "{} fell through to English in confirm_interpretation_fallback",
                lang.as_str()
            );
        }
    }

    #[test]
    fn expansion_languages_intentionally_fall_back_to_english() {
        // Languages added in the 2026-05 expansion that the catalog
        // does NOT yet cover — verify they fall through to English
        // (rather than producing some untranslated junk). Future
        // catalog extensions should remove entries from this list
        // and add them to CATALOG_LANGUAGES.
        let expansion_only = [
            LanguageCode::Bn,
            LanguageCode::He,
            LanguageCode::El,
            LanguageCode::Hy,
            LanguageCode::Ka,
            LanguageCode::Sw,
            LanguageCode::Am,
        ];
        let en_ask = ask_user_to_repeat(LanguageCode::En);
        let en_confirm = confirm_interpretation_prefix(LanguageCode::En);
        let en_fallback = confirm_interpretation_fallback(LanguageCode::En);
        for lang in expansion_only {
            assert_eq!(
                ask_user_to_repeat(lang),
                en_ask,
                "{} should still fall back to English",
                lang.as_str()
            );
            assert_eq!(confirm_interpretation_prefix(lang), en_confirm);
            assert_eq!(confirm_interpretation_fallback(lang), en_fallback);
        }
    }
}
