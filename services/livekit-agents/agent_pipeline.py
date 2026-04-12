"""
moa 음성 서비스 — STT → LLM → TTS 파이프라인 에이전트
========================================================

구성:
  - STT  : Deepgram Nova-3 (다국어, <300ms, $0.0077/분)
  - LLM  : 이용자 선택 (llm_factory). 미선택 시 Gemini 3.1 Flash
  - TTS  : Cartesia Sonic-3 (기본) / Typecast (한국어 프리미엄)
  - VAD  : Silero (무음 감지)
  - 턴테이킹 : LiveKit 내장 turn-detector 플러그인

실행:
  python agent_pipeline.py dev     # 로컬 개발 모드
  python agent_pipeline.py start   # 프로덕션 워커 모드
"""

from __future__ import annotations

import json
import logging
import os

from dotenv import load_dotenv
from livekit.agents import (
    Agent,
    AgentSession,
    JobContext,
    RoomInputOptions,
    WorkerOptions,
    cli,
    metrics,
    MetricsCollectedEvent,
)
from livekit.plugins import cartesia, deepgram, silero
from livekit.plugins.turn_detector.multilingual import MultilingualModel

import llm_factory
import billing_hook
from custom_typecast_tts import TypecastTTS, TypecastVoiceOptions

load_dotenv()
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("moa.agent_pipeline")


# ---------------------------------------------------------------------------
# moa 법률 비서 페르소나
# ---------------------------------------------------------------------------

MOA_INSTRUCTIONS = """당신은 'moa'라는 이름의 친근한 한국 법률 개인비서입니다.

성격:
- 말동무 같은 따뜻함과 전문가로서의 신뢰감을 동시에 가집니다
- 반말/존댓말은 상황에 맞게. 기본은 부드러운 존댓말
- 공감을 먼저 표현한 뒤 정보를 제공합니다
- 답변은 2-3 문장으로 짧고 명료하게. 길어질 땐 "더 자세히 들으시겠어요?" 라고 묻습니다

원칙:
- 당신의 답변은 법률 자문이 아닙니다. 최종 판단은 변호사 상담이 필요함을 자연스럽게 안내
- 개인정보(주민번호·계좌번호 등)를 말로 받지 마세요
- 사용자가 힘들어 보이면 정보 전달보다 공감을 우선

언어: 사용자가 쓰는 언어에 맞춰 자동 전환. 기본은 한국어."""


# ---------------------------------------------------------------------------
# TTS 선택 로직
# ---------------------------------------------------------------------------

def build_tts(language: str, tier: str):
    """
    language : 'ko' / 'en' / 기타 ISO 코드
    tier     : 'free' / 'premium'

    규칙:
      - 한국어 + premium → Typecast (한국어 특화)
      - 그 외            → Cartesia Sonic-3 (글로벌 기본)
    """
    if language == "ko" and tier == "premium" and os.getenv("TYPECAST_API_KEY"):
        logger.info("TTS: Typecast (ko premium)")
        return TypecastTTS(
            voice_options=TypecastVoiceOptions(
                voice_id=os.environ["TYPECAST_VOICE_ID_KO_FEMALE"],
                emotion="normal",
                speed_x=1.0,
            ),
            language="ko",
        )

    logger.info("TTS: Cartesia Sonic-3 (default)")
    return cartesia.TTS(
        model="sonic-3",
        voice=os.environ["CARTESIA_VOICE_ID_DEFAULT"],
        language=language if language in {"ko", "en", "ja", "es", "fr", "de"} else "en",
        speed="normal",
        emotion=["positivity:high", "curiosity"],  # Cartesia 감정 태그
    )


# ---------------------------------------------------------------------------
# STT 선택 로직
# ---------------------------------------------------------------------------

def build_stt(language: str):
    """
    기본: Deepgram Nova-3 (multi). 한국어 단독 세션은 language='ko' 고정.
    (CLOVA 백업은 확장 지점으로 표시)
    """
    return deepgram.STT(
        model="nova-3",
        language=language if language in {"ko", "en", "ja", "multi"} else "multi",
        interim_results=True,
        smart_format=True,
        punctuate=True,
        profanity_filter=False,
        keywords=[
            # 법률 도메인 고유명사 부스팅 (예시)
            ("계약해지통지", 1.5),
            ("내용증명", 1.5),
            ("민사소송", 1.2),
            ("지급명령", 1.2),
        ],
    )


# ---------------------------------------------------------------------------
# moa Agent 정의
# ---------------------------------------------------------------------------

class MoaAgent(Agent):
    def __init__(self) -> None:
        super().__init__(instructions=MOA_INSTRUCTIONS)


# ---------------------------------------------------------------------------
# Entrypoint
# ---------------------------------------------------------------------------

async def entrypoint(ctx: JobContext) -> None:
    await ctx.connect()

    participant = await ctx.wait_for_participant()
    logger.info("참가자 접속: %s", participant.identity)

    # 1) 이용자 metadata 파싱
    metadata_raw = participant.metadata
    try:
        meta = json.loads(metadata_raw) if metadata_raw else {}
    except json.JSONDecodeError:
        meta = {}
    language = (meta.get("language") or "ko").lower()
    tier = (meta.get("tier") or "free").lower()

    # 2) LLM 생성 (이용자 선택 → 없으면 운영자 기본 Gemini 3.1 Flash)
    llm_instance, selection = llm_factory.build_from_metadata(metadata_raw)
    logger.info(
        "LLM 선택: %s / model=%s / 이용자 본인키=%s",
        selection.provider, selection.model, selection.using_user_key,
    )

    # 3) STT / TTS 생성
    stt_instance = build_stt(language)
    tts_instance = build_tts(language, tier)

    # 4) 세션 조립
    session = AgentSession(
        stt=stt_instance,
        llm=llm_instance,
        tts=tts_instance,
        vad=silero.VAD.load(),
        turn_detection=MultilingualModel(),
    )

    # 5) 메트릭 수집 (원가 추적·크레딧 차감에 활용)
    usage_collector = metrics.UsageCollector()

    @session.on("metrics_collected")
    def _on_metrics(ev: MetricsCollectedEvent):
        metrics.log_metrics(ev.metrics)
        usage_collector.collect(ev.metrics)

    async def log_usage_and_bill():
        summary = usage_collector.get_summary()
        logger.info("사용량 요약: %s", summary)
        await billing_hook.bill_voice_usage(
            participant_identity=participant.identity,
            selection=selection,
            usage_summary=summary,
        )

    ctx.add_shutdown_callback(log_usage_and_bill)

    # 6) Start
    await session.start(
        agent=MoaAgent(),
        room=ctx.room,
        room_input_options=RoomInputOptions(
            noise_cancellation=None,   # LiveKit BVC 켜고 싶으면 여기에 설정
        ),
    )

    # 첫 인삿말
    await session.generate_reply(
        instructions="안녕하세요로 시작하는 자연스러운 첫 인삿말을 한 문장으로 건네세요."
    )


if __name__ == "__main__":
    cli.run_app(WorkerOptions(entrypoint_fnc=entrypoint))
