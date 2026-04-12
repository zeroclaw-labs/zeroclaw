"""
moa 음성 서비스 — 크레딧 차감 훅
==================================

세션 종료 시 LiveKit usage_collector 의 요약을 읽어
moa 게이트웨이의 /api/billing/voice-usage 에 POST 한다.

MoA 크레딧 정책:
  - 이용자 본인 API 키 사용 → 크레딧 차감 없음 (무료)
  - 운영자 키 사용 → 실제 비용 × 2.2 (운영자 마진 2.0 × 부가세 1.1)
"""
from __future__ import annotations

import logging
import os

logger = logging.getLogger("moa.billing_hook")

CREDIT_MULTIPLIER = 2.2  # 운영자 마진(2.0) × 부가세(1.1)


async def bill_voice_usage(
    participant_identity: str,
    selection,           # llm_factory.LLMSelection
    usage_summary,       # livekit.agents.metrics summary
):
    """세션 종료 시 호출. 이용자 본인 키면 스킵, 운영자 키면 차감."""
    if selection.using_user_key:
        logger.info("이용자 본인 키 사용 → 크레딧 차감 없음")
        return

    try:
        import aiohttp
        gateway_url = os.getenv("MOA_GATEWAY_URL", "http://127.0.0.1:8080")
        payload = {
            "user_id": participant_identity,
            "service": "voice",
            "provider": selection.provider,
            "model": selection.model,
            "usage_summary": str(usage_summary),
            "credit_multiplier": CREDIT_MULTIPLIER,
            "llm_prompt_tokens": getattr(usage_summary, "llm_prompt_tokens", 0),
            "llm_completion_tokens": getattr(usage_summary, "llm_completion_tokens", 0),
            "tts_characters_count": getattr(usage_summary, "tts_characters_count", 0),
            "stt_audio_duration": getattr(usage_summary, "stt_audio_duration", 0),
        }
        async with aiohttp.ClientSession() as http:
            async with http.post(
                f"{gateway_url}/api/billing/voice-usage",
                json=payload,
                timeout=aiohttp.ClientTimeout(total=10),
            ) as resp:
                if resp.status == 200:
                    result = await resp.json()
                    logger.info(
                        "크레딧 차감 완료: %s 크레딧 (2.2× 적용)",
                        result.get("credits_deducted", "?"),
                    )
                else:
                    body = await resp.text()
                    logger.warning("크레딧 차감 실패 (HTTP %s): %s", resp.status, body)
    except Exception as e:
        # 빌링 실패가 음성 세션을 깨뜨리면 안 됨
        logger.warning("크레딧 차감 중 예외 (세션은 정상 종료): %s", e)
