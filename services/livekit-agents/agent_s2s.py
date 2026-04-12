"""
moa 음성 서비스 — Gemini Live S2S 에이전트 (개발·테스트 전용)
================================================================

구성:
  - google.beta.realtime.RealtimeModel
  - 모델: gemini-3.1-flash-live-preview  (현재 preview)
  - 폴백: gemini-2.5-flash-native-audio-preview-12-2025 (이것도 preview)

⚠️ 프로덕션 배포 금지 경고
  1) Gemini Live API 는 2026-04 현재 모든 버전이 preview 단계이며,
     Google 이용약관상 프로덕션 사용이 허용되지 않습니다.
  2) EEA·스위스·영국 이용자에게는 반드시 유료 티어를 사용해야 합니다.
  3) Preview 트래픽은 Google 제품 개선에 사용될 수 있으므로
     실제 이용자의 민감정보(법률 상담)를 여기로 보내지 마세요.
  4) 이 에이전트는 'mode=s2s' 로 접속한 내부 테스터 세션에서만 실행되도록
     entrypoint 에서 권한 체크를 해야 합니다.

LiveKit 플러그인의 Gemini 3.1 Flash Live 제약 (2026-04 현재):
  - send_client_content : 첫 턴 이후 1007 에러 → 대화 도중 컨텍스트 주입 불가
  - generate_reply() / update_instructions() / update_chat_ctx()
    → 플러그인이 경고 로그만 남기고 무시함
  - 기본 음성 대화·툴 콜·오디오 I/O 는 정상 동작

따라서 instructions 은 생성 시점에 한 번만 주입하고,
세션 중간에 바꿀 수 없다는 점을 염두에 두고 설계해야 합니다.
"""

from __future__ import annotations

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
)
from livekit.plugins import google, silero

load_dotenv()
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger("moa.agent_s2s")


# ---------------------------------------------------------------------------
# S2S 모드용 지침 (세션 중 변경 불가)
# ---------------------------------------------------------------------------

MOA_S2S_INSTRUCTIONS = """당신은 'moa'라는 한국 법률 개인비서입니다.
따뜻하고 친근한 말동무 역할을 하되, 법률 정보는 정확하게.
답변은 짧고 명료하게. 최종 결정은 변호사 상담이 필요함을 자연스럽게 안내.
언어는 이용자에 맞춰 자동 전환. 기본은 한국어."""


# ---------------------------------------------------------------------------
# moa S2S Agent
# ---------------------------------------------------------------------------

class MoaS2SAgent(Agent):
    def __init__(self) -> None:
        # Gemini 3.1 Live 는 세션 도중 instructions 업데이트가 안 되므로
        # Agent 레벨에서 instructions 을 넘기지 않고, RealtimeModel 에만 주입한다.
        super().__init__(instructions="")


# ---------------------------------------------------------------------------
# Entrypoint
# ---------------------------------------------------------------------------

async def entrypoint(ctx: JobContext) -> None:
    await ctx.connect()

    # 내부 테스터 전용 체크 (프로덕션 유저 차단)
    participant = await ctx.wait_for_participant()
    if not participant.identity.startswith("internal-tester-"):
        logger.warning("S2S 모드는 내부 테스터 전용. 연결 종료: %s", participant.identity)
        await ctx.shutdown(reason="S2S 모드는 내부 테스터 전용입니다.")
        return

    model_name = os.getenv("GEMINI_LIVE_MODEL", "gemini-3.1-flash-live-preview")
    voice = os.getenv("GEMINI_LIVE_VOICE", "Puck")
    api_key = os.environ["GOOGLE_API_KEY"]

    logger.info("Gemini Live 연결: model=%s voice=%s", model_name, voice)

    # Gemini Live RealtimeModel — LLM, STT, TTS 가 하나의 모델로 통합
    realtime_llm = google.beta.realtime.RealtimeModel(
        model=model_name,
        voice=voice,
        temperature=0.7,
        instructions=MOA_S2S_INSTRUCTIONS,
        language="ko-KR",
        modalities=["AUDIO"],   # 오디오만. TEXT 까지 받으려면 ["AUDIO","TEXT"]
    )

    session = AgentSession(
        llm=realtime_llm,
        vad=silero.VAD.load(),   # VAD 는 Gemini Live 와 병용 가능 (barge-in 품질↑)
    )

    # 3.1 Flash Live 제약: generate_reply 무시됨.
    # 첫 인사는 Google 이 세션 시작 시 자동 수행하도록 두거나,
    # 필요하면 audio barge-in 으로 이용자 발화 대기만 한다.

    await session.start(
        agent=MoaS2SAgent(),
        room=ctx.room,
        room_input_options=RoomInputOptions(),
    )

    logger.info("S2S 세션 시작 완료")


if __name__ == "__main__":
    cli.run_app(WorkerOptions(entrypoint_fnc=entrypoint))
