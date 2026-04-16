#!/usr/bin/env python3
"""
eval_rag_llm.py — LLM-judged RAGAS metrics for MoA retrieval (PR #8).

Computes `faithfulness` and `answer_relevance` against the same golden
set the Rust harness uses. The Rust harness
(`cargo run --bin moa_eval`) owns context_precision / context_recall /
MRR; this script adds the metrics that require an LLM judge.

Usage
-----

    # 1. Make the Rust retriever emit retrieval + answer pairs to JSONL
    cargo run --bin moa_eval -- --set all --top-k 5 \\
        --emit-answers /tmp/moa-retrieval.jsonl

    # 2. Run the judge
    pip install openai python-dotenv
    export OPENAI_API_KEY=sk-...
    python tests/evals/scripts/eval_rag_llm.py \\
        --retrievals /tmp/moa-retrieval.jsonl \\
        --judge-model gpt-4o-mini \\
        --output /tmp/llm-judge.json

Input format (one JSON object per line, produced by moa_eval or any
retrieval pipeline the caller points at):

    {
      "query": "주택임대차보호법 대항력 요건",
      "gold_keys": ["corpus_law_001"],
      "retrieved_keys": ["corpus_law_001", "corpus_law_002"],
      "retrieved_contexts": [
        "주택임대차보호법 제3조는 …",
        "상가건물임대차보호법상 …"
      ],
      "answer": "주택의 인도와 주민등록을 마친 때부터 제3자에 대해 대항력이 …",
      "domain": "law"
    }

Output schema mirrors `moa_eval`'s `eval-report.json` so the CI
PR-comment workflow can render both reports in one table.

Design
------

Faithfulness:
    1. Ask the judge to enumerate atomic claims in `answer`.
    2. For each claim, ask "is this claim supported by
       retrieved_contexts?" (binary).
    3. faithfulness = supported_claims / total_claims.

Answer relevance:
    1. Ask the judge to generate 3 questions that `answer` would be a
       good reply to.
    2. For each generated question, cosine-compare against the
       original query using the judge's own embeddings.
    3. answer_relevance = mean(cosine).

Both metrics fall back to `None` and flag `skeleton=True` when the
judge call fails — we prefer "no data" over "false confidence".

This script deliberately avoids the full `ragas` Python package (extra
50+ MB install, requires matching LangChain versions). The three
prompts are inlined so the contract is greppable and there's no
dependency drift across RAGAS releases.
"""

from __future__ import annotations

import argparse
import dataclasses
import json
import math
import os
import sys
from pathlib import Path
from typing import Any, Iterable


# ── Input / output schemas ─────────────────────────────────────────


@dataclasses.dataclass
class RetrievalEntry:
    query: str
    gold_keys: list[str]
    retrieved_keys: list[str]
    retrieved_contexts: list[str]
    answer: str
    domain: str


@dataclasses.dataclass
class PerQueryScore:
    query: str
    domain: str
    faithfulness: float | None
    answer_relevance: float | None
    notes: list[str]


# ── IO ─────────────────────────────────────────────────────────────


def load_retrievals(path: Path) -> list[RetrievalEntry]:
    out: list[RetrievalEntry] = []
    with path.open("r", encoding="utf-8") as fh:
        for line in fh:
            s = line.strip()
            if not s or s.startswith("#"):
                continue
            obj = json.loads(s)
            out.append(
                RetrievalEntry(
                    query=str(obj["query"]),
                    gold_keys=list(obj.get("gold_keys", [])),
                    retrieved_keys=list(obj.get("retrieved_keys", [])),
                    retrieved_contexts=list(obj.get("retrieved_contexts", [])),
                    answer=str(obj.get("answer", "")),
                    domain=str(obj.get("domain", "unknown")),
                )
            )
    return out


# ── Judge client ───────────────────────────────────────────────────


class JudgeError(RuntimeError):
    pass


class JudgeClient:
    """
    Thin wrapper around any OpenAI-compatible chat endpoint. Supports
    the official OpenAI API today; any endpoint that accepts
    `{model, messages, temperature}` at `/v1/chat/completions` works.

    Using requests + JSON instead of the openai SDK so the script
    doesn't break when the SDK's API shape drifts.
    """

    def __init__(self, model: str, base_url: str, api_key: str):
        self.model = model
        self.base_url = base_url.rstrip("/")
        self.api_key = api_key
        try:
            import requests  # noqa: F401
        except ImportError as err:  # pragma: no cover — install guard
            raise JudgeError(
                "requests package missing; install with `pip install requests`"
            ) from err

    def chat(self, system: str, user: str, temperature: float = 0.0) -> str:
        import requests

        payload = {
            "model": self.model,
            "temperature": temperature,
            "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": user},
            ],
        }
        resp = requests.post(
            f"{self.base_url}/v1/chat/completions",
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
            json=payload,
            timeout=60,
        )
        if resp.status_code != 200:
            raise JudgeError(f"judge {self.model} HTTP {resp.status_code}: {resp.text[:400]}")
        data = resp.json()
        try:
            return data["choices"][0]["message"]["content"]
        except (KeyError, IndexError, TypeError) as err:
            raise JudgeError(f"unexpected judge response: {data}") from err

    def embed(self, text: str, model: str = "text-embedding-3-small") -> list[float]:
        import requests

        resp = requests.post(
            f"{self.base_url}/v1/embeddings",
            headers={
                "Authorization": f"Bearer {self.api_key}",
                "Content-Type": "application/json",
            },
            json={"model": model, "input": text},
            timeout=60,
        )
        if resp.status_code != 200:
            raise JudgeError(f"embed HTTP {resp.status_code}: {resp.text[:400]}")
        data = resp.json()
        try:
            return data["data"][0]["embedding"]
        except (KeyError, IndexError, TypeError) as err:
            raise JudgeError(f"unexpected embed response: {data}") from err


def cosine(a: list[float], b: list[float]) -> float:
    if not a or not b or len(a) != len(b):
        return 0.0
    dot = sum(x * y for x, y in zip(a, b))
    na = math.sqrt(sum(x * x for x in a))
    nb = math.sqrt(sum(y * y for y in b))
    if na == 0 or nb == 0:
        return 0.0
    return dot / (na * nb)


# ── Metric implementations ─────────────────────────────────────────


FAITHFULNESS_SYSTEM = (
    "You split an answer into atomic factual claims and check each against "
    "retrieved contexts. Output JSON ONLY in this shape: "
    '{"claims": [{"text": "…", "supported": true|false}]}. '
    "A claim is `supported` ONLY if the retrieved contexts explicitly state "
    "or directly imply it. Unsupported guesses, paraphrases that change "
    "meaning, and hallucinated details count as `false`."
)


ANSWER_RELEVANCE_SYSTEM = (
    "Given an ANSWER, produce 3 questions that ANSWER would be a correct, "
    "direct reply to. Output JSON ONLY: "
    '{"questions": ["…", "…", "…"]}.'
)


def measure_faithfulness(
    judge: JudgeClient, entry: RetrievalEntry
) -> tuple[float | None, list[str]]:
    if not entry.answer.strip() or not entry.retrieved_contexts:
        return None, ["empty answer or contexts"]
    ctx = "\n---\n".join(entry.retrieved_contexts)
    user = f"Retrieved contexts:\n{ctx}\n\nAnswer:\n{entry.answer}\n\nReturn JSON only."
    try:
        raw = judge.chat(FAITHFULNESS_SYSTEM, user)
    except JudgeError as err:
        return None, [f"judge error: {err}"]
    try:
        data = json.loads(raw.strip().removeprefix("```json").removesuffix("```").strip())
        claims = data.get("claims", [])
    except json.JSONDecodeError:
        return None, [f"could not parse judge JSON: {raw[:200]}"]
    if not claims:
        return None, ["judge returned no claims"]
    supported = sum(1 for c in claims if c.get("supported") is True)
    return supported / len(claims), []


def measure_answer_relevance(
    judge: JudgeClient, entry: RetrievalEntry
) -> tuple[float | None, list[str]]:
    if not entry.answer.strip():
        return None, ["empty answer"]
    try:
        raw = judge.chat(ANSWER_RELEVANCE_SYSTEM, f"ANSWER:\n{entry.answer}")
    except JudgeError as err:
        return None, [f"judge error: {err}"]
    try:
        data = json.loads(raw.strip().removeprefix("```json").removesuffix("```").strip())
        qs = data.get("questions", [])[:3]
    except json.JSONDecodeError:
        return None, [f"could not parse judge JSON: {raw[:200]}"]
    if not qs:
        return None, ["judge returned no questions"]
    try:
        query_emb = judge.embed(entry.query)
        sims = [cosine(query_emb, judge.embed(q)) for q in qs]
    except JudgeError as err:
        return None, [f"embedding error: {err}"]
    return sum(sims) / len(sims), []


# ── Main ───────────────────────────────────────────────────────────


def average(values: Iterable[float | None]) -> float | None:
    kept = [v for v in values if v is not None]
    if not kept:
        return None
    return sum(kept) / len(kept)


def aggregate(scores: list[PerQueryScore]) -> dict[str, Any]:
    domains: dict[str, list[PerQueryScore]] = {}
    for s in scores:
        domains.setdefault(s.domain, []).append(s)

    sets = []
    for dom in sorted(domains):
        rows = domains[dom]
        sets.append(
            {
                "domain": dom,
                "queries": len(rows),
                "faithfulness": average(s.faithfulness for s in rows),
                "answer_relevance": average(s.answer_relevance for s in rows),
            }
        )
    return {
        "sets": sets,
        "overall": {
            "domain": "overall",
            "queries": len(scores),
            "faithfulness": average(s.faithfulness for s in scores),
            "answer_relevance": average(s.answer_relevance for s in scores),
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--retrievals", type=Path, required=True)
    parser.add_argument("--judge-model", default="gpt-4o-mini")
    parser.add_argument(
        "--base-url",
        default=os.environ.get("OPENAI_BASE_URL", "https://api.openai.com"),
    )
    parser.add_argument("--output", type=Path, default=None)
    parser.add_argument(
        "--dry-run",
        action="store_true",
        help="Skip judge calls; emit a skeleton report for CI wiring tests.",
    )
    parser.add_argument(
        "--max-queries",
        type=int,
        default=None,
        help="Limit number of queries scored (for spot-checks).",
    )
    args = parser.parse_args()

    api_key = os.environ.get("OPENAI_API_KEY", "")
    if not args.dry_run and not api_key:
        print(
            "error: OPENAI_API_KEY not set; pass --dry-run for a skeleton report",
            file=sys.stderr,
        )
        return 2

    entries = load_retrievals(args.retrievals)
    if args.max_queries is not None:
        entries = entries[: args.max_queries]

    scores: list[PerQueryScore] = []
    if args.dry_run:
        for e in entries:
            scores.append(
                PerQueryScore(
                    query=e.query,
                    domain=e.domain,
                    faithfulness=None,
                    answer_relevance=None,
                    notes=["dry-run"],
                )
            )
    else:
        judge = JudgeClient(args.judge_model, args.base_url, api_key)
        for e in entries:
            f, f_notes = measure_faithfulness(judge, e)
            a, a_notes = measure_answer_relevance(judge, e)
            scores.append(
                PerQueryScore(
                    query=e.query,
                    domain=e.domain,
                    faithfulness=f,
                    answer_relevance=a,
                    notes=[*f_notes, *a_notes],
                )
            )

    report = aggregate(scores)
    report["judge_model"] = args.judge_model
    report["base_url"] = args.base_url
    report["skeleton"] = bool(args.dry_run)
    report["per_query_notes"] = [
        {"query": s.query, "domain": s.domain, "notes": s.notes}
        for s in scores
        if s.notes
    ]

    payload = json.dumps(report, indent=2, ensure_ascii=False)
    if args.output:
        args.output.write_text(payload, encoding="utf-8")
        print(f"wrote LLM judge report → {args.output}")
    else:
        print(payload)
    return 0


if __name__ == "__main__":
    sys.exit(main())
