#!/usr/bin/env python3
"""Kokoro TTS Server

OpenAI-compatible HTTP API backed by sherpa-onnx + Kokoro v0.19.
Exposes POST /v1/audio/speech for ZeroClaw voice replies.
"""

import io
import os
import time
from pathlib import Path

import numpy as np
import sherpa_onnx
import soundfile as sf
import uvicorn
from fastapi import FastAPI
from fastapi.responses import JSONResponse, Response
from pydantic import BaseModel

MODEL_DIR = os.environ.get(
    "KOKORO_MODEL_DIR",
    str(Path(__file__).parent / "models" / "kokoro-en-v0_19"),
)

NUM_THREADS = int(os.environ.get("KOKORO_THREADS", "4"))
DEFAULT_SID = int(os.environ.get("KOKORO_DEFAULT_SID", "6"))  # am_michael

VOICES = {
    "af": 0, "af_bella": 1, "af_nicole": 2, "af_sarah": 3, "af_sky": 4,
    "am_adam": 5, "am_michael": 6,
    "bf_emma": 7, "bf_isabella": 8, "bm_george": 9, "bm_lewis": 10,
    # OpenAI voice name aliases
    "alloy": 0, "nova": 2, "shimmer": 3,
    "echo": 6, "onyx": 5, "fable": 9,
}

app = FastAPI(title="Kokoro TTS Server", docs_url="/docs")
tts = None


class SpeechRequest(BaseModel):
    model: str = "tts-1"
    input: str
    voice: str = "echo"
    response_format: str = "wav"
    speed: float = 1.0


def create_tts() -> sherpa_onnx.OfflineTts:
    model_dir = Path(MODEL_DIR)
    config = sherpa_onnx.OfflineTtsConfig(
        model=sherpa_onnx.OfflineTtsModelConfig(
            kokoro=sherpa_onnx.OfflineTtsKokoroModelConfig(
                model=str(model_dir / "model.onnx"),
                voices=str(model_dir / "voices.bin"),
                tokens=str(model_dir / "tokens.txt"),
                data_dir=str(model_dir / "espeak-ng-data"),
            ),
            num_threads=NUM_THREADS,
            provider="cpu",
        ),
        max_num_sentences=2,
    )
    if not config.validate():
        raise ValueError("Invalid Kokoro TTS config — check model files")
    return sherpa_onnx.OfflineTts(config)


def audio_to_wav(samples, sample_rate: int) -> bytes:
    buf = io.BytesIO()
    sf.write(buf, np.array(samples, dtype=np.float32), sample_rate,
             format="WAV", subtype="PCM_16")
    buf.seek(0)
    return buf.read()


@app.on_event("startup")
def startup():
    global tts
    t0 = time.time()
    tts = create_tts()
    print(f"Kokoro TTS loaded in {time.time() - t0:.1f}s")
    print(f"Default voice: SID {DEFAULT_SID}")


@app.get("/health")
def health():
    return {
        "status": "ok" if tts is not None else "loading",
        "model": Path(MODEL_DIR).name,
        "voices": list(VOICES.keys()),
    }


@app.get("/v1/audio/voices")
def list_voices():
    return {"voices": [{"name": k, "sid": v} for k, v in VOICES.items()]}


@app.post("/v1/audio/speech")
async def speech(req: SpeechRequest):
    if not req.input or not req.input.strip():
        return JSONResponse(status_code=400,
                            content={"error": {"message": "Empty input"}})

    if len(req.input) > 4096:
        return JSONResponse(status_code=400,
                            content={"error": {"message": "Input exceeds 4096 chars"}})

    sid = VOICES.get(req.voice, DEFAULT_SID)
    if isinstance(sid, str):
        sid = DEFAULT_SID

    t0 = time.time()
    audio = tts.generate(req.input, sid=sid, speed=req.speed)
    elapsed = time.time() - t0
    duration = len(audio.samples) / audio.sample_rate
    print(f"Synthesized {len(req.input)} chars -> {duration:.1f}s audio in {elapsed:.2f}s "
          f"(RTF {elapsed/duration:.3f}, voice={req.voice}/sid={sid})")

    wav_bytes = audio_to_wav(audio.samples, audio.sample_rate)
    return Response(
        content=wav_bytes,
        media_type="audio/wav",
        headers={"Content-Disposition": "attachment; filename=speech.wav"},
    )


if __name__ == "__main__":
    port = int(os.environ.get("KOKORO_PORT", "6009"))
    print(f"Kokoro TTS server starting on http://127.0.0.1:{port}")
    uvicorn.run(app, host="127.0.0.1", port=port)
