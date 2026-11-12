use super::*;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::mpsc;
use wiremock::{
    Mock, MockServer, Request, ResponseTemplate,
    matchers::{method, path},
};
use zeroclaw_config::schema::{Config, TtsProviderConfig};

const OPENER: &str =
    "The opening sentence is long enough to synthesize before the response finishes.";

async fn speech_fixture() -> (Arc<TtsManager>, MockServer, mpsc::UnboundedReceiver<String>) {
    let server = MockServer::start().await;
    let (tx, rx) = mpsc::unbounded_channel();
    Mock::given(method("POST"))
        .and(path("/speech"))
        .respond_with(move |request: &Request| {
            let body: serde_json::Value = serde_json::from_slice(&request.body).unwrap();
            let text = body["input"].as_str().unwrap().to_owned();
            tx.send(text.clone()).ok();
            ResponseTemplate::new(200)
                .set_body_bytes(text.into_bytes())
                .set_delay(Duration::from_millis(50))
        })
        .mount(&server)
        .await;
    let mut config = Config::default();
    config.agents.insert(
        "fixture".into(),
        zeroclaw_config::schema::AliasedAgentConfig {
            tts_provider: "openai.fixture".into(),
            ..Default::default()
        },
    );
    config.providers.tts.openai.insert(
        "fixture".into(),
        zeroclaw_config::schema::OpenAITtsProviderConfig {
            base: TtsProviderConfig {
                api_key: Some("fixture".into()),
                uri: Some(format!("{}/speech", server.uri())),
                ..Default::default()
            },
        },
    );
    (
        Arc::new(TtsManager::from_config_for_agent(&config, Some("fixture")).unwrap()),
        server,
        rx,
    )
}

#[tokio::test]
async fn speech_synthesizes_before_finish_and_reuses_ordered_audio() {
    let (manager, _server, mut requests) = speech_fixture().await;
    let job = SpeechJob::start(manager);
    job.update(&format!("{OPENER} A still unfinished paragraph"));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), requests.recv())
            .await
            .unwrap()
            .unwrap(),
        OPENER
    );
    let final_text = format!("{OPENER} A completed paragraph.\nLast paragraph.");
    let audio = job.finish(&final_text).await.unwrap();
    assert_eq!(
        audio,
        [
            OPENER.as_bytes(),
            b"A completed paragraph.",
            b"Last paragraph."
        ]
        .map(Vec::from)
    );
    assert_eq!(
        requests.recv().await.as_deref(),
        Some("A completed paragraph.")
    );
    assert_eq!(requests.recv().await.as_deref(), Some("Last paragraph."));
    assert!(
        requests.try_recv().is_err(),
        "the opening sentence must not be synthesized twice"
    );
}

#[tokio::test]
async fn speech_final_rewrite_discards_speculative_audio() {
    let (manager, _server, mut requests) = speech_fixture().await;
    let job = SpeechJob::start(manager);
    job.update(&format!("{OPENER} Unfinished text"));
    requests.recv().await.unwrap();
    let final_text = "The final approved response is different from the speculative draft.";
    let audio = job.finish(final_text).await.unwrap();
    assert_eq!(audio, [final_text.as_bytes().to_vec()]);
}

#[tokio::test]
async fn speech_jobs_are_isolated_and_drop_cancels_the_worker() {
    let (manager, _server, mut requests) = speech_fixture().await;
    let canceled = SpeechJob::start(manager.clone());
    canceled.update(&format!("{OPENER} No later chunks should start."));
    requests.recv().await.unwrap();
    let abort = canceled.worker.abort_handle();
    drop(canceled);
    tokio::time::timeout(Duration::from_secs(2), async {
        while !abort.is_finished() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    let other = SpeechJob::start(manager);
    let text = "This other draft owns its own response and must not contain earlier narration.";
    assert_eq!(
        other.finish(text).await.unwrap(),
        [text.as_bytes().to_vec()]
    );
    assert_eq!(requests.recv().await.as_deref(), Some(text));
    assert!(requests.try_recv().is_err());
}

#[cfg(unix)]
fn concat_fixture_command(sidecar: &std::path::Path, stop: bool) -> tokio::process::Command {
    let mut command = tokio::process::Command::new("sh");
    command.args(["-c", if stop {
        "state=$1; shift; while [ $# -gt 0 ]; do if [ \"$1\" = -i ]; then shift; printf '%s\\n' \"$1\" > \"$state\"; break; fi; shift; done; exec sleep 30"
    } else {
        "state=$1; shift; while [ $# -gt 0 ]; do if [ \"$1\" = -i ]; then shift; printf '%s\\n' \"$1\" > \"$state\"; break; fi; shift; done; printf OggS-fixture"
    }, "fixture"]).arg(sidecar);
    command
}

#[cfg(unix)]
#[tokio::test]
async fn speech_concat_private_files_are_removed_on_cancellation_and_success() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = tempfile::tempdir().unwrap();
    for cancel in [false, true] {
        let sidecar = fixture
            .path()
            .join(if cancel { "canceled" } else { "successful" });
        let command = concat_fixture_command(&sidecar, cancel);
        let task = zeroclaw_spawn::spawn!(async move {
            concat_parts_with_command(&[b"first".to_vec(), b"second".to_vec()], "wav", command)
                .await
        });
        let list = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(path) = tokio::fs::read_to_string(&sidecar).await
                    && !path.trim().is_empty()
                {
                    break std::path::PathBuf::from(path.trim());
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let directory = list.parent().unwrap();
        if cancel {
            assert_eq!(
                directory.metadata().unwrap().permissions().mode() & 0o777,
                0o700
            );
            for entry in std::fs::read_dir(directory).unwrap() {
                assert_eq!(
                    entry.unwrap().metadata().unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
        } else {
            assert_eq!(task.await.unwrap().unwrap(), b"OggS-fixture");
        }
        assert!(
            !directory.exists(),
            "audio staging must not survive the operation"
        );
    }
}

pub(crate) fn silence_wav(samples: u32) -> Vec<u8> {
    let length = samples * 2;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + length).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16u32.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&1u16.to_le_bytes());
    bytes.extend_from_slice(&24000u32.to_le_bytes());
    bytes.extend_from_slice(&48000u32.to_le_bytes());
    bytes.extend_from_slice(&2u16.to_le_bytes());
    bytes.extend_from_slice(&16u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&length.to_le_bytes());
    bytes.resize(44 + length as usize, 0);
    bytes
}

#[tokio::test]
#[ignore = "requires ffmpeg with libopus; run explicitly for voice deployment"]
async fn speech_concat_real_wav_parts_decode_as_one_complete_opus_file() {
    let audio = concat_parts_to_opus(&[silence_wav(2400), silence_wav(2400)], "wav")
        .await
        .unwrap();
    assert!(audio.starts_with(b"OggS"));
    let mut file = tempfile::NamedTempFile::new().unwrap();
    file.write_all(&audio).unwrap();
    let decoded = tokio::process::Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(file.path())
        .args(["-f", "s16le", "-ac", "1", "-ar", "24000", "pipe:1"])
        .output()
        .await
        .unwrap();
    assert!(decoded.status.success());
    assert_eq!(
        decoded.stdout.len(),
        4800 * 2,
        "both clips must survive decoding"
    );
}
