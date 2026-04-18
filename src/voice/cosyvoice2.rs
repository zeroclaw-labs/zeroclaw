//! CosyVoice 2 offline TTS with zero-shot voice cloning (Tier B — Offline Pro).
//!
//! Plan §11.2 / §11.4 path 2: when the user is offline AND has registered a
//! 3–10 s voice sample, synthesize translated output in their own voice
//! locally. CosyVoice 2 supports zero-shot cloning out of the box (no
//! training step needed) and is Apache 2.0.
//!
//! ## Deployment shape
//!
//! Like [`super::kokoro_tts`], this module talks to an HTTP sidecar
//! (FunAudioLLM/CosyVoice exposed via a thin FastAPI wrapper). The trait
//! surface ([`super::tts_engine::TtsEngine`]) is identical so the router
//! (PR #9) doesn't need to know whether it's hitting Kokoro or CosyVoice.
//!
//! ## Voice reference storage
//!
//! Reference samples are written to `~/.moa/voice_references/` encrypted
//! with ChaCha20-Poly1305 and never leave the device. The encryption key
//! is a per-install random secret in `~/.moa/voice_references/.key` with
//! `0600` permissions. This is best-effort confidentiality — an attacker
//! with filesystem access can read the key. A passphrase-protected mode
//! (PBKDF2 → key) is a follow-up and intentionally not in this PR.

use async_trait::async_trait;
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use chacha20poly1305::aead::{Aead, AeadCore, KeyInit, OsRng};
use chacha20poly1305::{ChaCha20Poly1305, Key, Nonce};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs;

use super::tts_engine::{EmotionHint, SynthesisResult, TtsEngine, VoiceCard};

/// Default CosyVoice 2 sidecar endpoint (community FastAPI wrapper convention).
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:9233";
/// CosyVoice 2 native sample rate.
pub const COSYVOICE_SAMPLE_RATE: u32 = 22_050;

/// Synthesizer engine implementing `TtsEngine`.
pub struct CosyVoice2Engine {
    base_url: String,
    client: reqwest::Client,
    /// Cached reference index (loaded at construction).
    references: Vec<VoiceReferenceMeta>,
    /// Optional override for where references live; defaults to ~/.moa/voice_references.
    references_dir: PathBuf,
}

impl CosyVoice2Engine {
    /// Construct an engine pointing at the given sidecar URL. References are
    /// loaded from `references_dir`; pass `None` for the default path.
    pub fn new(
        base_url: impl Into<String>,
        references_dir: Option<PathBuf>,
    ) -> anyhow::Result<Self> {
        let dir = match references_dir {
            Some(p) => p,
            None => default_references_dir().ok_or_else(|| {
                anyhow::anyhow!("cannot locate $HOME for default voice references dir")
            })?,
        };
        let references = load_reference_index_blocking(&dir).unwrap_or_default();
        Ok(Self {
            base_url: base_url.into(),
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client builds"),
            references,
            references_dir: dir,
        })
    }

    /// Default constructor (sidecar at localhost:9233, references under
    /// `~/.moa/voice_references/`).
    pub fn with_defaults() -> anyhow::Result<Self> {
        Self::new(DEFAULT_BASE_URL, None)
    }

    /// Register a new voice reference: encrypt the sample bytes, write to disk,
    /// add to the index. Returns the reference id used downstream by
    /// `synthesize(voice_id = <id>, ...)`.
    pub async fn register_reference(
        &mut self,
        meta: VoiceReferenceInput,
        sample_bytes: &[u8],
    ) -> anyhow::Result<String> {
        if sample_bytes.is_empty() {
            anyhow::bail!("voice reference sample must be non-empty");
        }
        fs::create_dir_all(&self.references_dir).await?;

        let key = ensure_key(&self.references_dir).await?;
        let cipher = ChaCha20Poly1305::new(&key);
        let nonce = ChaCha20Poly1305::generate_nonce(&mut OsRng);
        let nonce_bytes: [u8; 12] = nonce.into();

        let ciphertext = cipher
            .encrypt(&nonce, sample_bytes)
            .map_err(|e| anyhow::anyhow!("encrypt failed: {e}"))?;

        let id = format!("ref_{}", uuid::Uuid::new_v4().simple());
        let blob_path = self.references_dir.join(format!("{id}.bin"));

        // Layout: 12-byte nonce || ciphertext.
        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&nonce_bytes);
        out.extend_from_slice(&ciphertext);
        fs::write(&blob_path, out).await?;

        let card_meta = VoiceReferenceMeta {
            id: id.clone(),
            display_name: meta.display_name,
            language: meta.language,
            registered_at_unix: now_unix_secs(),
        };
        self.references.push(card_meta);
        save_reference_index(&self.references_dir, &self.references).await?;

        Ok(id)
    }

    /// Drop a previously registered reference. Removes both the encrypted
    /// blob and the index entry.
    pub async fn unregister_reference(&mut self, id: &str) -> anyhow::Result<()> {
        let blob_path = self.references_dir.join(format!("{id}.bin"));
        if blob_path.exists() {
            fs::remove_file(&blob_path).await?;
        }
        self.references.retain(|r| r.id != id);
        save_reference_index(&self.references_dir, &self.references).await?;
        Ok(())
    }

    /// Decrypt and return the raw sample bytes for `id`. Used internally by
    /// `synthesize` and exposed for tests.
    pub async fn decrypt_reference(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let blob_path = self.references_dir.join(format!("{id}.bin"));
        let blob = fs::read(&blob_path).await?;
        if blob.len() < 12 {
            anyhow::bail!("reference blob too short");
        }
        let key = ensure_key(&self.references_dir).await?;
        let cipher = ChaCha20Poly1305::new(&key);
        let nonce = Nonce::from_slice(&blob[..12]);
        cipher
            .decrypt(nonce, &blob[12..])
            .map_err(|e| anyhow::anyhow!("decrypt failed: {e}"))
    }

    /// List registered references as voice cards (for the UI picker).
    pub fn list_references(&self) -> Vec<VoiceCard> {
        self.references
            .iter()
            .map(|r| VoiceCard {
                id: r.id.clone(),
                display_name: format!("{} (내 목소리 클로닝)", r.display_name),
                language: r.language.clone(),
                gender: None,
                age_band: None,
                persona_blurb: Some("로컬 zero-shot 클로닝, 외부 송출 없음".into()),
                engine: "cosyvoice2".into(),
            })
            .collect()
    }
}

#[async_trait]
impl TtsEngine for CosyVoice2Engine {
    fn name(&self) -> &str {
        "cosyvoice2"
    }

    fn list_voices(&self) -> Vec<VoiceCard> {
        // CosyVoice 2 has no fixed voice catalog — every voice is a reference.
        self.list_references()
    }

    fn supports_cloning(&self) -> bool {
        true
    }

    async fn synthesize(
        &self,
        text: &str,
        voice_id: &str,
        language: &str,
        emotion: &EmotionHint,
    ) -> anyhow::Result<SynthesisResult> {
        // Decrypt reference sample for the requested voice id.
        let sample = self.decrypt_reference(voice_id).await?;
        let sample_b64 = BASE64.encode(&sample);

        let body = CosyVoice2Request {
            text: text.to_string(),
            reference_audio_b64: sample_b64,
            language: language.to_string(),
            emotion: emotion.emotion.clone(),
            speed: emotion.speed,
        };

        let url = format!("{}/v1/zero_shot/synthesize", self.base_url);
        let resp = self.client.post(&url).json(&body).send().await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let err = resp.text().await.unwrap_or_default();
            anyhow::bail!("CosyVoice 2 error {status}: {err}");
        }
        let pcm = resp.bytes().await?.to_vec();
        Ok(SynthesisResult {
            pcm,
            sample_rate: COSYVOICE_SAMPLE_RATE,
        })
    }

    async fn health_ok(&self) -> bool {
        let url = format!("{}/v1/health", self.base_url);
        match self
            .client
            .get(&url)
            .timeout(Duration::from_secs(2))
            .send()
            .await
        {
            Ok(r) => r.status().is_success(),
            Err(_) => false,
        }
    }
}

// ── Public types ────────────────────────────────────────────────────────

/// User-supplied metadata for a new voice reference registration.
#[derive(Debug, Clone)]
pub struct VoiceReferenceInput {
    /// Display name shown in the UI ("내 목소리", "회의용", ...).
    pub display_name: String,
    /// Native language code of the sample (BCP-47, e.g. "ko").
    pub language: String,
}

/// Persisted metadata for one registered voice reference (matches the on-disk
/// `index.json` layout). Not the encrypted audio itself — that lives in
/// `<id>.bin`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceReferenceMeta {
    pub id: String,
    pub display_name: String,
    pub language: String,
    pub registered_at_unix: u64,
}

// ── Wire format ─────────────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct CosyVoice2Request {
    text: String,
    reference_audio_b64: String,
    language: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    emotion: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    speed: Option<f32>,
}

// ── Reference index helpers ─────────────────────────────────────────────

fn default_references_dir() -> Option<PathBuf> {
    crate::util::home_dir().map(|h| h.join(".moa").join("voice_references"))
}

// Use the shared helper from src/util.rs (was duplicated here + #184's
// network_health.rs on main — both converge on the single util impl).
use crate::util::now_unix_secs;

/// Synchronous variant for use during construction (we don't want to require
/// async context just to load the index).
fn load_reference_index_blocking(dir: &Path) -> anyhow::Result<Vec<VoiceReferenceMeta>> {
    let path = dir.join("index.json");
    if !path.exists() {
        return Ok(Vec::new());
    }
    let data = std::fs::read_to_string(&path)?;
    Ok(serde_json::from_str(&data)?)
}

async fn save_reference_index(dir: &Path, refs: &[VoiceReferenceMeta]) -> anyhow::Result<()> {
    fs::create_dir_all(dir).await?;
    let json = serde_json::to_string_pretty(refs)?;
    fs::write(dir.join("index.json"), json).await?;
    Ok(())
}

/// Load (or generate) the per-install ChaCha20-Poly1305 key. The key file is
/// written with restrictive permissions on Unix so casual filesystem access
/// can't trivially exfiltrate it.
async fn ensure_key(dir: &Path) -> anyhow::Result<Key> {
    fs::create_dir_all(dir).await?;
    let key_path = dir.join(".key");

    if key_path.exists() {
        let bytes = fs::read(&key_path).await?;
        if bytes.len() != 32 {
            anyhow::bail!("voice reference key has wrong length ({})", bytes.len());
        }
        let mut arr = [0u8; 32];
        arr.copy_from_slice(&bytes);
        return Ok(Key::from(arr));
    }

    let key_value = ChaCha20Poly1305::generate_key(&mut OsRng);
    let bytes: [u8; 32] = key_value.into();
    fs::write(&key_path, bytes).await?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&key_path).await?.permissions();
        perms.set_mode(0o600);
        fs::set_permissions(&key_path, perms).await?;
    }

    Ok(Key::from(bytes))
}

// ── Tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_engine() -> (CosyVoice2Engine, tempfile::TempDir) {
        let tmp = tempfile::tempdir().unwrap();
        let engine = CosyVoice2Engine::new(DEFAULT_BASE_URL, Some(tmp.path().to_path_buf()))
            .expect("engine constructs");
        (engine, tmp)
    }

    #[test]
    fn engine_is_cloning_capable() {
        let (e, _t) = fresh_engine();
        assert_eq!(e.name(), "cosyvoice2");
        assert!(e.supports_cloning());
    }

    #[tokio::test]
    async fn register_then_decrypt_roundtrip() {
        let (mut engine, _tmp) = fresh_engine();
        let sample = b"this would normally be 22050 Hz PCM bytes";
        let id = engine
            .register_reference(
                VoiceReferenceInput {
                    display_name: "내 목소리".into(),
                    language: "ko".into(),
                },
                sample,
            )
            .await
            .unwrap();
        assert!(id.starts_with("ref_"));

        let decrypted = engine.decrypt_reference(&id).await.unwrap();
        assert_eq!(decrypted, sample);
    }

    #[tokio::test]
    async fn empty_sample_rejected() {
        let (mut engine, _tmp) = fresh_engine();
        let res = engine
            .register_reference(
                VoiceReferenceInput {
                    display_name: "x".into(),
                    language: "ko".into(),
                },
                &[],
            )
            .await;
        assert!(res.is_err());
    }

    #[tokio::test]
    async fn unregister_removes_blob_and_index_entry() {
        let (mut engine, tmp) = fresh_engine();
        let id = engine
            .register_reference(
                VoiceReferenceInput {
                    display_name: "x".into(),
                    language: "ko".into(),
                },
                b"sample",
            )
            .await
            .unwrap();
        let blob = tmp.path().join(format!("{id}.bin"));
        assert!(blob.exists());

        engine.unregister_reference(&id).await.unwrap();
        assert!(!blob.exists());
        assert!(engine.list_references().is_empty());
    }

    #[tokio::test]
    async fn index_persists_across_engine_reload() {
        let tmp = tempfile::tempdir().unwrap();
        {
            let mut engine =
                CosyVoice2Engine::new(DEFAULT_BASE_URL, Some(tmp.path().to_path_buf())).unwrap();
            engine
                .register_reference(
                    VoiceReferenceInput {
                        display_name: "회의용".into(),
                        language: "ko".into(),
                    },
                    b"audio bytes",
                )
                .await
                .unwrap();
        }
        // New engine instance reads the index from disk.
        let engine2 =
            CosyVoice2Engine::new(DEFAULT_BASE_URL, Some(tmp.path().to_path_buf())).unwrap();
        let cards = engine2.list_references();
        assert_eq!(cards.len(), 1);
        assert!(cards[0].display_name.starts_with("회의용"));
        assert_eq!(cards[0].engine, "cosyvoice2");
    }

    #[tokio::test]
    async fn key_file_is_stable_across_calls() {
        let tmp = tempfile::tempdir().unwrap();
        let key1 = ensure_key(tmp.path()).await.unwrap();
        let key2 = ensure_key(tmp.path()).await.unwrap();
        assert_eq!(key1.as_slice(), key2.as_slice());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn key_file_has_restricted_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let _ = ensure_key(tmp.path()).await.unwrap();
        let key_path = tmp.path().join(".key");
        let perms = std::fs::metadata(&key_path).unwrap().permissions();
        assert_eq!(perms.mode() & 0o777, 0o600);
    }

    #[tokio::test]
    async fn health_ok_returns_false_for_unreachable() {
        let engine = CosyVoice2Engine::new("http://127.0.0.1:1", None).unwrap();
        assert!(!engine.health_ok().await);
    }

    /// Live test against a CosyVoice 2 sidecar. Requires:
    /// - A running CosyVoice 2 FastAPI server on localhost:9233
    /// - At least one registered reference
    ///
    /// Run with:
    ///     cargo test --lib voice::cosyvoice2::tests::live_synthesize -- --ignored --nocapture
    #[tokio::test]
    #[ignore]
    async fn live_synthesize() {
        let mut engine = CosyVoice2Engine::with_defaults().unwrap();
        if !engine.health_ok().await {
            eprintln!("skipping: CosyVoice 2 sidecar not reachable");
            return;
        }
        // Register a placeholder reference if none exists.
        let id = if let Some(card) = engine.list_references().into_iter().next() {
            card.id
        } else {
            let placeholder = vec![0u8; 22_050]; // 1 s of silence (test only)
            engine
                .register_reference(
                    VoiceReferenceInput {
                        display_name: "test".into(),
                        language: "ko".into(),
                    },
                    &placeholder,
                )
                .await
                .unwrap()
        };
        let result = engine
            .synthesize(
                "안녕하세요. 이건 라이브 테스트입니다.",
                &id,
                "ko",
                &EmotionHint::default(),
            )
            .await
            .expect("synthesize should succeed");
        println!(
            "\nGot {} bytes of {} Hz PCM",
            result.pcm.len(),
            result.sample_rate
        );
        assert!(!result.pcm.is_empty());
    }
}
