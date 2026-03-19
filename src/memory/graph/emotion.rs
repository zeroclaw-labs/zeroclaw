//! VAD (Valence-Arousal-Dominance) emotional analysis via regex patterns.
//!
//! Analyzes Spanish and English text for emotional content using keyword matching.
//! Returns a VAD triplet used to weight episode importance in the knowledge graph.

use regex::Regex;
use std::sync::LazyLock;

/// Emotional valence-arousal-dominance triplet.
#[derive(Debug, Clone, Copy, Default)]
pub struct VadScore {
    /// Pleasure/displeasure (-1.0 to 1.0)
    pub valence: f64,
    /// Calm/excited (0.0 to 1.0)
    pub arousal: f64,
    /// Submissive/dominant (0.0 to 1.0)
    pub dominance: f64,
}

struct EmotionPattern {
    regex: Regex,
    vad: VadScore,
}

static EMOTION_PATTERNS: LazyLock<Vec<EmotionPattern>> = LazyLock::new(|| {
    vec![
        // Positive high-arousal (joy, excitement)
        ep(
            r"(?i)\b(genial|excelente|increíble|amazing|great|awesome|fantastic|love|encanta)\b",
            0.8,
            0.7,
            0.6,
        ),
        ep(
            r"(?i)\b(feliz|happy|alegre|contento|excited|emocionado)\b",
            0.7,
            0.6,
            0.5,
        ),
        ep(
            r"(?i)\b(gracias|thanks|agradecido|grateful)\b",
            0.6,
            0.3,
            0.4,
        ),
        ep(r"(?i)\b(bien|good|bueno|nice|cool)\b", 0.4, 0.3, 0.5),
        // Positive low-arousal (calm, satisfied)
        ep(
            r"(?i)\b(tranquilo|calm|peaceful|sereno|relajado|relaxed)\b",
            0.5,
            0.1,
            0.5,
        ),
        ep(r"(?i)\b(perfecto|perfect|exacto|correcto)\b", 0.6, 0.4, 0.7),
        // Negative high-arousal (anger, frustration)
        ep(
            r"(?i)\b(furioso|angry|enojado|rage|rabia)\b",
            -0.8,
            0.9,
            0.7,
        ),
        ep(
            r"(?i)\b(frustrado|frustrated|molesto|annoyed)\b",
            -0.6,
            0.7,
            0.4,
        ),
        ep(r"(?i)\b(odio|hate|detesto|desprecio)\b", -0.9, 0.8, 0.6),
        // Negative low-arousal (sadness, disappointment)
        ep(r"(?i)\b(triste|sad|deprimido|depressed)\b", -0.7, 0.3, 0.2),
        ep(
            r"(?i)\b(decepcionado|disappointed|desilusionado)\b",
            -0.5,
            0.4,
            0.3,
        ),
        ep(r"(?i)\b(aburrido|bored|cansado|tired)\b", -0.3, 0.1, 0.3),
        // Fear/anxiety
        ep(
            r"(?i)\b(miedo|fear|asustado|scared|worried|preocupado|ansioso|anxious)\b",
            -0.6,
            0.8,
            0.2,
        ),
        // Surprise
        ep(
            r"(?i)\b(sorpresa|surprised|wow|increíble|asombroso)\b",
            0.3,
            0.8,
            0.5,
        ),
        // Urgency/importance markers
        ep(
            r"(?i)\b(urgente|urgent|importante|important|critical|crítico)\b",
            0.0,
            0.9,
            0.7,
        ),
    ]
});

fn ep(pattern: &str, valence: f64, arousal: f64, dominance: f64) -> EmotionPattern {
    EmotionPattern {
        regex: Regex::new(pattern).expect("invalid emotion regex"),
        vad: VadScore {
            valence,
            arousal,
            dominance,
        },
    }
}

/// Analyze text for emotional content and return aggregated VAD scores.
///
/// Matches are averaged across all detected patterns. Returns default (neutral)
/// if no emotional patterns are detected.
pub fn analyze_emotion(text: &str) -> VadScore {
    let mut total_v = 0.0;
    let mut total_a = 0.0;
    let mut total_d = 0.0;
    let mut count = 0u32;

    for pattern in EMOTION_PATTERNS.iter() {
        let matches = pattern.regex.find_iter(text).count();
        if matches > 0 {
            // Weight by number of matches but diminish additional matches
            let weight = 1.0 + (matches as f64 - 1.0).min(2.0) * 0.3;
            total_v += pattern.vad.valence * weight;
            total_a += pattern.vad.arousal * weight;
            total_d += pattern.vad.dominance * weight;
            count += 1;
        }
    }

    if count == 0 {
        return VadScore::default();
    }

    let n = f64::from(count);
    VadScore {
        valence: (total_v / n).clamp(-1.0, 1.0),
        arousal: (total_a / n).clamp(0.0, 1.0),
        dominance: (total_d / n).clamp(0.0, 1.0),
    }
}

/// Calculate emotional salience (0.0 to 1.0) from VAD scores.
/// High arousal + extreme valence = high salience.
pub fn emotional_salience(vad: &VadScore) -> f64 {
    let valence_intensity = vad.valence.abs();
    ((valence_intensity * 0.4 + vad.arousal * 0.4 + vad.dominance * 0.2) as f64).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positive_text_has_positive_valence() {
        let vad = analyze_emotion("Esto es genial, estoy muy feliz con el resultado");
        assert!(
            vad.valence > 0.0,
            "valence should be positive: {}",
            vad.valence
        );
    }

    #[test]
    fn negative_text_has_negative_valence() {
        let vad = analyze_emotion("Estoy furioso y frustrado con este error");
        assert!(
            vad.valence < 0.0,
            "valence should be negative: {}",
            vad.valence
        );
    }

    #[test]
    fn neutral_text_returns_default() {
        let vad = analyze_emotion("La función recibe dos parámetros y retorna un resultado");
        assert!((vad.valence).abs() < 0.01);
        assert!((vad.arousal).abs() < 0.01);
    }

    #[test]
    fn english_emotions_detected() {
        let vad = analyze_emotion("This is amazing and I love it");
        assert!(vad.valence > 0.5);
    }

    #[test]
    fn salience_high_for_extreme_emotions() {
        let vad = VadScore {
            valence: -0.9,
            arousal: 0.9,
            dominance: 0.7,
        };
        let s = emotional_salience(&vad);
        assert!(s > 0.7);
    }

    #[test]
    fn salience_low_for_neutral() {
        let s = emotional_salience(&VadScore::default());
        assert!(s < 0.1);
    }
}
