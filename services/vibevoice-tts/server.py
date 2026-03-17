#!/usr/bin/env python3
"""VibeVoice-Realtime TTS Server

OpenAI-compatible HTTP API backed by VibeVoice-Realtime-0.5B.
Exposes POST /v1/audio/speech — drop-in for OpenAI TTS that
ZeroClaw's Matrix channel calls for voice replies.
"""

import copy
import io
import os
import sys
import time
from pathlib import Path

import soundfile as sf
import torch
import uvicorn
from fastapi import FastAPI
from fastapi.responses import Response, JSONResponse
from pydantic import BaseModel

VIBEVOICE_DIR = os.environ.get(
    "VIBEVOICE_DIR",
    str(Path.home() / "code" / "m4_16GB_models" / "tools" / "VibeVoice"),
)
VOICES_DIR = os.environ.get(
    "VOICES_DIR",
    str(Path(VIBEVOICE_DIR) / "demo" / "voices" / "streaming_model"),
)
MODEL_ID = os.environ.get("VIBEVOICE_MODEL", "microsoft/VibeVoice-Realtime-0.5B")
DEFAULT_VOICE = os.environ.get("VIBEVOICE_VOICE", "en-Carter_man")

SAMPLE_RATE = 24000

app = FastAPI(title="VibeVoice TTS Server", docs_url="/docs")
model = None
processor = None
voice_presets = {}


class SpeechRequest(BaseModel):
    model: str = "tts-1"
    input: str
    voice: str = "alloy"
    response_format: str = "wav"
    speed: float = 1.0


# Map OpenAI voice names to VibeVoice presets
VOICE_MAP = {
    "alloy": "en-Alice_woman",
    "echo": "en-Carter_man",
    "fable": "en-Maya_woman",
    "onyx": "en-Frank_man",
    "nova": "en-Mary_woman_bgm",
    "shimmer": "en-Alice_woman",
}

MIME_MAP = {
    "wav": "audio/wav",
    "mp3": "audio/mpeg",
    "ogg": "audio/ogg",
    "opus": "audio/opus",
    "flac": "audio/flac",
}


def load_model():
    global model, processor

    sys.path.insert(0, VIBEVOICE_DIR)
    from vibevoice.modular.modeling_vibevoice_streaming_inference import (
        VibeVoiceStreamingForConditionalGenerationInference,
    )
    from vibevoice.processor.vibevoice_streaming_processor import (
        VibeVoiceStreamingProcessor,
    )

    device = "mps" if torch.backends.mps.is_available() else "cpu"

    processor = VibeVoiceStreamingProcessor.from_pretrained(MODEL_ID)
    model = VibeVoiceStreamingForConditionalGenerationInference.from_pretrained(
        MODEL_ID,
        torch_dtype=torch.float32,
        attn_implementation="sdpa",
        device_map=None,
    )
    model.to(device)
    model.eval()
    model.set_ddpm_inference_steps(num_steps=5)
    return device


def load_voice(voice_name: str):
    if voice_name in voice_presets:
        return voice_presets[voice_name]

    voice_file = Path(VOICES_DIR) / f"{voice_name}.pt"
    if not voice_file.exists():
        return None

    device = "mps" if torch.backends.mps.is_available() else "cpu"
    preset = torch.load(str(voice_file), map_location=device, weights_only=False)
    voice_presets[voice_name] = preset
    return preset


def synthesize(text: str, voice_name: str) -> torch.Tensor:
    device = "mps" if torch.backends.mps.is_available() else "cpu"
    preset = load_voice(voice_name)
    if preset is None:
        raise ValueError(f"Voice preset not found: {voice_name}")

    inputs = processor.process_input_with_cached_prompt(
        text=text,
        cached_prompt=preset,
        padding=True,
        return_tensors="pt",
        return_attention_mask=True,
    )
    for k, v in inputs.items():
        if torch.is_tensor(v):
            inputs[k] = v.to(device)

    outputs = model.generate(
        **inputs,
        max_new_tokens=None,
        cfg_scale=1.5,
        tokenizer=processor.tokenizer,
        generation_config={"do_sample": False},
        verbose=False,
        all_prefilled_outputs=copy.deepcopy(preset),
    )
    return outputs.speech_outputs[0]


def audio_to_bytes(audio: torch.Tensor, fmt: str) -> bytes:
    audio_np = audio.squeeze().cpu().numpy()
    buf = io.BytesIO()

    if fmt == "wav":
        sf.write(buf, audio_np, SAMPLE_RATE, format="WAV", subtype="PCM_16")
    elif fmt == "flac":
        sf.write(buf, audio_np, SAMPLE_RATE, format="FLAC")
    elif fmt in ("mp3", "ogg", "opus"):
        # Write WAV first, convert via ffmpeg
        import subprocess
        import tempfile

        wav_buf = io.BytesIO()
        sf.write(wav_buf, audio_np, SAMPLE_RATE, format="WAV", subtype="PCM_16")
        wav_buf.seek(0)

        codec_args = {
            "mp3": ["-c:a", "libmp3lame", "-q:a", "2"],
            "ogg": ["-c:a", "libvorbis", "-q:a", "4"],
            "opus": ["-c:a", "libopus", "-b:a", "64k"],
        }
        with tempfile.NamedTemporaryFile(suffix=f".{fmt}", delete=False) as tmp:
            tmp_path = tmp.name

        try:
            result = subprocess.run(
                [
                    "ffmpeg", "-hide_banner", "-loglevel", "error",
                    "-i", "pipe:0", *codec_args[fmt], "-f", fmt, tmp_path, "-y",
                ],
                input=wav_buf.read(),
                capture_output=True,
                check=True,
            )
            with open(tmp_path, "rb") as f:
                buf = io.BytesIO(f.read())
        finally:
            os.unlink(tmp_path)
    else:
        sf.write(buf, audio_np, SAMPLE_RATE, format="WAV", subtype="PCM_16")

    buf.seek(0)
    return buf.read()


@app.on_event("startup")
def startup():
    print(f"Loading VibeVoice-Realtime model: {MODEL_ID}")
    t0 = time.time()
    device = load_model()
    print(f"Model loaded on {device} in {time.time() - t0:.1f}s")

    # Pre-load default voice
    preset = load_voice(DEFAULT_VOICE)
    if preset:
        print(f"Default voice loaded: {DEFAULT_VOICE}")
    else:
        print(f"WARNING: Default voice not found: {DEFAULT_VOICE}")


@app.get("/health")
def health():
    return {
        "status": "ok" if model is not None else "loading",
        "model": MODEL_ID,
        "voices": list(voice_presets.keys()),
    }


@app.get("/v1/audio/voices")
def list_voices():
    voices_path = Path(VOICES_DIR)
    available = [p.stem for p in voices_path.glob("*.pt")] if voices_path.exists() else []
    return {"voices": available}


@app.post("/v1/audio/speech")
async def speech(req: SpeechRequest):
    if not req.input or not req.input.strip():
        return JSONResponse(
            status_code=400,
            content={"error": {"message": "Empty input text"}},
        )

    if len(req.input) > 4096:
        return JSONResponse(
            status_code=400,
            content={"error": {"message": "Input exceeds 4096 characters"}},
        )

    # Resolve voice name
    voice_name = VOICE_MAP.get(req.voice, req.voice)

    try:
        audio = synthesize(req.input, voice_name)
    except ValueError as e:
        return JSONResponse(
            status_code=400,
            content={"error": {"message": str(e)}},
        )
    except Exception as e:
        return JSONResponse(
            status_code=500,
            content={"error": {"message": f"Synthesis failed: {str(e)[:200]}"}},
        )

    fmt = req.response_format if req.response_format in MIME_MAP else "wav"
    audio_bytes = audio_to_bytes(audio, fmt)
    mime = MIME_MAP.get(fmt, "audio/wav")

    return Response(
        content=audio_bytes,
        media_type=mime,
        headers={"Content-Disposition": f"attachment; filename=speech.{fmt}"},
    )


if __name__ == "__main__":
    port = int(os.environ.get("VIBEVOICE_PORT", "6009"))
    print(f"VibeVoice TTS server starting on http://127.0.0.1:{port}")
    uvicorn.run(app, host="127.0.0.1", port=port)
