"""
moa 음성 서비스 — LLM Factory
===============================

이용자가 클라이언트에서 넘긴 metadata(JSON)를 읽어
적절한 LiveKit LLM 인스턴스를 만들어 반환한다.

metadata 스키마 (클라이언트가 participant.metadata 로 전달):
{
  "llm_provider": "google" | "anthropic" | "openai",   # 없거나 알 수 없으면 google
  "api_key": "sk-... 또는 AIza...",                    # 없으면 운영자 기본 키
  "model": "gemini-3.1-flash-lite-preview",                         # 생략 가능 (provider별 default)
  "temperature": 0.7,                                  # 생략 가능
  "language": "ko"                                     # STT 라우팅용. LLM엔 직접 안 쓰임
}

LiveKit 공식 플러그인은 모두 (model=..., api_key=...) 파라미터를 받는
동일한 인터페이스를 제공하므로, 실제 주입 지점은 이 한 파일이면 충분하다.
"""

from __future__ import annotations

import json
import logging
import os
from dataclasses import dataclass
from typing import Optional

from livekit.agents import llm as llm_base
from livekit.plugins import anthropic, google, openai

logger = logging.getLogger("moa.llm_factory")


# ---------------------------------------------------------------------------
# 운영자 기본값 (이용자가 선택 안 했을 때 사용)
# ---------------------------------------------------------------------------

DEFAULT_PROVIDER = "google"

DEFAULT_MODELS = {
    "google": "gemini-3.1-flash-lite-preview",          # moa 기본: Gemini 3.1 Flash (일반 LLM, Live API 아님)
    "anthropic": "claude-sonnet-4-6",      # 법률 도메인 기본
    "openai": "gpt-4o",                    # 일반 대화 기본
}

FALLBACK_KEYS = {
    "google": os.getenv("GOOGLE_API_KEY"),
    "anthropic": os.getenv("ANTHROPIC_API_KEY"),
    "openai": os.getenv("OPENAI_API_KEY"),
}


@dataclass
class LLMSelection:
    """파싱된 이용자 선택 결과."""
    provider: str
    model: str
    api_key: Optional[str]
    temperature: float
    using_user_key: bool   # True: 이용자 본인 키, False: 운영자 fallback 키


# ---------------------------------------------------------------------------
# metadata 파싱
# ---------------------------------------------------------------------------

def parse_metadata(metadata_json: str | None) -> LLMSelection:
    """participant.metadata (JSON 문자열) → LLMSelection"""
    try:
        meta = json.loads(metadata_json) if metadata_json else {}
    except json.JSONDecodeError:
        logger.warning("metadata JSON 파싱 실패, 기본값 사용: %s", metadata_json)
        meta = {}

    provider = (meta.get("llm_provider") or DEFAULT_PROVIDER).lower()
    if provider not in DEFAULT_MODELS:
        logger.warning("알 수 없는 provider '%s' → 기본값으로 fallback", provider)
        provider = DEFAULT_PROVIDER

    user_key = meta.get("api_key")
    api_key = user_key or FALLBACK_KEYS.get(provider)
    if not api_key:
        raise RuntimeError(
            f"{provider} LLM용 API 키가 없습니다. "
            f"이용자 키 또는 환경변수 {provider.upper()}_API_KEY 를 설정하세요."
        )

    return LLMSelection(
        provider=provider,
        model=meta.get("model") or DEFAULT_MODELS[provider],
        api_key=api_key,
        temperature=float(meta.get("temperature", 0.7)),
        using_user_key=bool(user_key),
    )


# ---------------------------------------------------------------------------
# LLM 인스턴스 생성
# ---------------------------------------------------------------------------

def create_llm(selection: LLMSelection) -> llm_base.LLM:
    """
    LiveKit AgentSession 에 주입할 LLM 객체를 생성.

    세 플러그인 모두 livekit.agents.llm.LLM 을 상속하므로
    AgentSession(llm=...) 인자로 그대로 쓸 수 있다.
    """
    logger.info(
        "LLM 생성: provider=%s model=%s user_key=%s",
        selection.provider, selection.model, selection.using_user_key,
    )

    if selection.provider == "google":
        return google.LLM(
            model=selection.model,
            api_key=selection.api_key,
            temperature=selection.temperature,
        )

    if selection.provider == "anthropic":
        return anthropic.LLM(
            model=selection.model,
            api_key=selection.api_key,
            temperature=selection.temperature,
        )

    if selection.provider == "openai":
        return openai.LLM(
            model=selection.model,
            api_key=selection.api_key,
            temperature=selection.temperature,
        )

    # parse_metadata 에서 이미 검증했으므로 여기까지 오지 않음
    raise ValueError(f"지원하지 않는 provider: {selection.provider}")


# ---------------------------------------------------------------------------
# 편의 함수
# ---------------------------------------------------------------------------

def build_from_metadata(metadata_json: str | None) -> tuple[llm_base.LLM, LLMSelection]:
    """가장 흔한 사용 패턴: metadata 한 줄을 넘기면 LLM 과 선택정보를 반환."""
    selection = parse_metadata(metadata_json)
    return create_llm(selection), selection
