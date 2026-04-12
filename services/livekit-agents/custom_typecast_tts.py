"""
moa 음성 서비스 — Typecast 커스텀 TTS 플러그인
================================================

Typecast(Neosapience)는 2026-04 현재 LiveKit 공식 플러그인이 없으므로
livekit.agents.tts.TTS 를 직접 상속해 구현한다.

아래 코드는 Typecast REST API (v2) 를 사용하며,
한국어 SSFM 보이스를 지정해 음성을 합성한 뒤 LiveKit 오디오 프레임으로 변환한다.

⚠️ Typecast API 스펙은 서비스 운영사가 수시로 업데이트할 수 있으므로
   실제 통합 전 공식 문서와 https://typecast.ai/dashboard/api 의
   최신 엔드포인트·파라미터를 반드시 확인하세요.

이 파일은 "작동 가능한 골격"이며, Typecast 가입 후 voice_id, endpoint 값을
본인 계정 것으로 교체해야 합니다.
"""

from __future__ import annotations

import asyncio
import logging
import os
from dataclasses import dataclass
from typing import AsyncIterator

import httpx
from livekit import rtc
from livekit.agents import tts, utils

logger = logging.getLogger("moa.typecast_tts")

TYPECAST_API_BASE = "https://api.typecast.ai/v1"
SAMPLE_RATE = 24_000   # Typecast 기본 출력 샘플레이트 (PCM16)
NUM_CHANNELS = 1


@dataclass
class TypecastVoiceOptions:
    voice_id: str
    emotion: str = "normal"   # normal / happy / sad / angry / tonedown / toneup 등
    speed_x: float = 1.0
    tempo: float = 1.0
    pitch: int = 0


class TypecastTTS(tts.TTS):
    """Typecast v2 REST API 기반 커스텀 TTS 플러그인."""

    def __init__(
        self,
        *,
        api_key: str | None = None,
        voice_options: TypecastVoiceOptions,
        language: str = "ko",
    ) -> None:
        super().__init__(
            capabilities=tts.TTSCapabilities(streaming=False),
            sample_rate=SAMPLE_RATE,
            num_channels=NUM_CHANNELS,
        )
        self._api_key = api_key or os.environ.get("TYPECAST_API_KEY")
        if not self._api_key:
            raise RuntimeError("TYPECAST_API_KEY 가 설정되지 않았습니다.")
        self._voice = voice_options
        self._language = language

    # LiveKit TTS 는 synthesize(text) 메서드를 호출한다
    def synthesize(self, text: str) -> "TypecastChunkedStream":
        return TypecastChunkedStream(
            tts=self,
            text=text,
            api_key=self._api_key,
            voice=self._voice,
            language=self._language,
        )


class TypecastChunkedStream(tts.ChunkedStream):
    """단일 발화 요청을 Typecast에 보내고 PCM 프레임으로 쪼개 스트리밍한다."""

    def __init__(
        self,
        *,
        tts: TypecastTTS,
        text: str,
        api_key: str,
        voice: TypecastVoiceOptions,
        language: str,
    ) -> None:
        super().__init__(tts=tts, input_text=text)
        self._api_key = api_key
        self._voice = voice
        self._language = language

    async def _run(self) -> None:
        payload = {
            "text": self._input_text,
            "lang": self._language,
            "voice_id": self._voice.voice_id,
            "emotion_preset": self._voice.emotion,
            "model_version": "latest",
            "speed_x": self._voice.speed_x,
            "tempo": self._voice.tempo,
            "pitch": self._voice.pitch,
            "output": {
                "audio_format": "wav",
                "sample_rate": SAMPLE_RATE,
            },
        }
        headers = {
            "Authorization": f"Bearer {self._api_key}",
            "Content-Type": "application/json",
        }

        audio_bytes = b""
        try:
            async with httpx.AsyncClient(timeout=30.0) as client:
                # 1) 합성 요청 (polling 방식)
                r = await client.post(
                    f"{TYPECAST_API_BASE}/text-to-speech",
                    json=payload, headers=headers,
                )
                r.raise_for_status()
                speak_url = r.json()["result"]["speak_v2_url"]

                # 2) polling — Typecast 는 결과가 준비될 때까지 status 를 갱신
                for _ in range(30):
                    poll = await client.get(speak_url, headers=headers)
                    poll.raise_for_status()
                    data = poll.json().get("result", {})
                    if data.get("status") == "done":
                        audio_url = data["audio_download_url"]
                        audio_resp = await client.get(audio_url, headers=headers)
                        audio_resp.raise_for_status()
                        audio_bytes = audio_resp.content
                        break
                    await asyncio.sleep(0.3)
                else:
                    raise RuntimeError("Typecast 합성 polling 타임아웃")
        except Exception as e:
            logger.exception("Typecast 합성 실패: %s", e)
            raise

        # 3) WAV 헤더 건너뛰고 PCM 프레임으로 쪼개 LiveKit 큐에 push
        pcm_data = _strip_wav_header(audio_bytes)
        frame_size = SAMPLE_RATE // 10  # 100ms 프레임
        offset = 0
        while offset < len(pcm_data):
            chunk = pcm_data[offset : offset + frame_size * 2]  # int16 = 2 bytes
            if not chunk:
                break
            frame = rtc.AudioFrame(
                data=chunk,
                sample_rate=SAMPLE_RATE,
                num_channels=NUM_CHANNELS,
                samples_per_channel=len(chunk) // 2,
            )
            self._event_ch.send_nowait(
                tts.SynthesizedAudio(
                    request_id=utils.shortuuid(),
                    frame=frame,
                )
            )
            offset += frame_size * 2


def _strip_wav_header(data: bytes) -> bytes:
    """44바이트 WAV 헤더를 제거하고 PCM16 payload만 반환."""
    if data[:4] == b"RIFF":
        return data[44:]
    return data
