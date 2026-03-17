#!/usr/bin/env python3
"""Parakeet TDT STT Server

OpenAI Whisper-compatible HTTP API backed by sherpa-onnx and
the Parakeet TDT 0.6B v3 (INT8) offline transducer model.

Exposes POST /v1/audio/transcriptions — drop-in replacement for
the Groq/OpenAI transcription endpoint that ZeroClaw calls.
"""

import os
import subprocess
import tempfile
from pathlib import Path

import numpy as np
import sherpa_onnx
import uvicorn
from fastapi import FastAPI, File, Form, UploadFile
from fastapi.responses import JSONResponse, PlainTextResponse

MODEL_DIR = os.environ.get(
    "PARAKEET_MODEL_DIR",
    str(
        Path(__file__).parent
        / "models"
        / "sherpa-onnx-nemo-parakeet-tdt-0.6b-v3-int8"
    ),
)

NUM_THREADS = int(os.environ.get("PARAKEET_THREADS", "4"))

app = FastAPI(title="Parakeet STT Server", docs_url="/docs")
recognizer = None


def create_recognizer() -> sherpa_onnx.OfflineRecognizer:
    model_dir = Path(MODEL_DIR)
    encoder = model_dir / "encoder.int8.onnx"
    decoder = model_dir / "decoder.int8.onnx"
    joiner = model_dir / "joiner.int8.onnx"
    tokens = model_dir / "tokens.txt"

    for path in (encoder, decoder, joiner, tokens):
        if not path.exists():
            raise FileNotFoundError(
                f"Missing model file: {path}\nRun setup.sh to download the model."
            )

    return sherpa_onnx.OfflineRecognizer.from_transducer(
        encoder=str(encoder),
        decoder=str(decoder),
        joiner=str(joiner),
        tokens=str(tokens),
        num_threads=NUM_THREADS,
        provider="cpu",
        model_type="nemo_transducer",
    )


def audio_to_pcm(audio_bytes: bytes, suffix: str) -> tuple[np.ndarray, int]:
    """Convert audio bytes to 16 kHz mono float32 PCM via ffmpeg."""
    with tempfile.NamedTemporaryFile(suffix=suffix, delete=False) as tmp:
        tmp.write(audio_bytes)
        tmp_path = tmp.name

    try:
        result = subprocess.run(
            [
                "ffmpeg",
                "-hide_banner",
                "-loglevel",
                "error",
                "-i",
                tmp_path,
                "-ar",
                "16000",
                "-ac",
                "1",
                "-f",
                "f32le",
                "-acodec",
                "pcm_f32le",
                "pipe:1",
            ],
            capture_output=True,
            check=True,
        )
        samples = np.frombuffer(result.stdout, dtype=np.float32)
        return samples, 16000
    finally:
        os.unlink(tmp_path)


@app.on_event("startup")
def startup():
    global recognizer
    recognizer = create_recognizer()


@app.get("/health")
def health():
    return {"status": "ok", "model": Path(MODEL_DIR).name}


@app.post("/v1/audio/transcriptions")
async def transcribe(
    file: UploadFile = File(...),
    model: str = Form("parakeet-tdt-0.6b-v3"),
    response_format: str = Form("json"),
    language: str = Form(None),
    prompt: str = Form(None),
):
    audio_bytes = await file.read()
    if not audio_bytes:
        return JSONResponse(
            status_code=400,
            content={"error": {"message": "Empty audio file"}},
        )

    suffix = Path(file.filename or "audio.ogg").suffix or ".ogg"

    try:
        samples, sample_rate = audio_to_pcm(audio_bytes, suffix)
    except subprocess.CalledProcessError as e:
        stderr = e.stderr.decode(errors="replace")[:300]
        return JSONResponse(
            status_code=400,
            content={"error": {"message": f"Audio conversion failed: {stderr}"}},
        )
    except FileNotFoundError:
        return JSONResponse(
            status_code=500,
            content={
                "error": {
                    "message": "ffmpeg not found. Install with: brew install ffmpeg"
                }
            },
        )

    if len(samples) == 0:
        return JSONResponse(
            status_code=400,
            content={"error": {"message": "No audio samples after conversion"}},
        )

    stream = recognizer.create_stream()
    stream.accept_waveform(sample_rate, samples)
    recognizer.decode_stream(stream)
    text = stream.result.text.strip()

    if response_format == "text":
        return PlainTextResponse(text)

    return {"text": text}


if __name__ == "__main__":
    port = int(os.environ.get("PARAKEET_PORT", "6008"))
    print(f"Parakeet STT server starting on http://127.0.0.1:{port}")
    print(f"Model: {MODEL_DIR}")
    print(f"Threads: {NUM_THREADS}")
    uvicorn.run(app, host="127.0.0.1", port=port)
