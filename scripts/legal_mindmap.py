#!/usr/bin/env python3
"""
legal_mindmap.py - 법조문 중심 쟁점별 판례 마인드맵

특정 법조문(예: 민법 제750조)을 인용한 모든 판례를 쟁점별로 그룹핑하여
인터랙티브 마인드맵으로 시각화.

쟁점 그룹핑 방법:
  1. 각 판례의 판시사항에서 핵심 명사구(쟁점 키워드) 추출
  2. 판례들이 공통으로 인용하는 다른 적용법조 조합으로 2차 그룹핑
  3. 유사 쟁점 키워드를 클러스터링 → 쟁점 그룹 형성
  4. 각 그룹의 대표 키워드 = 마인드맵 소제목

Usage:
  python legal_mindmap.py "statute::민법::750" --output mindmap.html
  python legal_mindmap.py "민법 제750조" --output mindmap.html --max-cases 500
"""

import argparse
import json
import re
import sqlite3
import time
import html as html_mod
from collections import defaultdict, Counter
from pathlib import Path

DB_PATH = Path(r"C:\Users\kjccj\세컨드브레인_로컬DB\domain.db")

BOILERPLATE = {
    "statute::형법::37", "statute::형법::38", "statute::형법::39",
    "statute::형법::40", "statute::형법::35", "statute::형법::53",
    "statute::형법::55", "statute::형법::62", "statute::형법::57",
    "statute::형법::70", "statute::형사소송법::334", "statute::형사소송법::369",
    "statute::형사소송법::383", "statute::형사소송법::396",
    "statute::민사소송법::288", "statute::민사소송법::217",
    "statute::소송촉진등에관한특례법::3",
}

# 한글 명사 추출용 불용어 — 조사/어미/부사/형용사/일반동사 등 비명사 제거
# 법률 키워드는 반드시 명사(또는 명사+명사)여야 함
STOP_WORDS = {
    # 조사/어미/접속사/부사
    '있어', '없어', '하여', '되어', '의하여', '관하여', '대하여', '위하여',
    '따라', '따른', '인한', '인하여', '에서', '으로', '에게', '에의',
    '있는', '없는', '하는', '되는', '된다', '한다', '이다', '아닌',
    '것이', '것을', '것의', '것은', '것에', '때의', '등의', '수의',
    '받은', '받는', '받을', '된다', '하고', '하며', '이며', '으며',
    '또는', '및', '이를', '그의', '자의', '자가', '자는', '자에',
    '경우', '경우에', '여부', '여부의', '관한', '대한', '위한',
    '있다', '없다', '한다', '된다', '이다', '아니다',
    '할수', '있을', '없을', '하여야', '되어야',
    '가운데', '사이에', '이상의', '이하의', '에서의',
    '때문', '바에', '이에', '대해', '관해',
    '당시', '이후', '이전', '사이', '동안', '가량',
    '그것', '이것', '저것', '무엇', '어떤', '어느',
    '같은', '다른', '모든', '각각', '각자', '서로',
    '매우', '아주', '다소', '상당', '다만', '특히',
    '통하여', '비추어', '거쳐', '대해서', '관해서',
    # 법원/소송 절차 일반어 (쟁점이 아닌 일반 절차)
    '판결', '선고', '대법원', '법원', '재판', '사건',
    '원고', '피고', '상고', '항소', '상소', '제소',
    '소극', '적극', '원칙', '판단', '기준', '요건',
    '범위', '효력', '의미', '내용', '방법', '절차',
    '제도', '규정', '조항', '법률', '법령', '적용',
    '해당', '이상', '이하', '기타', '전부', '일부',
    '주장', '인정', '기각', '각하', '파기', '환송',
    '취지', '이유', '결론', '결과', '주문',
    '원심', '상고심', '제1심', '항소심',
    '증거', '증명', '입증', '소명', '자료',
    '제출', '주장', '진술', '신청', '청구',
    '심리', '변론', '종결', '선고', '확정',
    # 일반 용언/형용사 어근
    '가능', '불가', '곤란', '상당', '적절', '부당',
    '위법', '적법', '정당', '필요', '중요', '명확',
    '구체적', '일반적', '통상적', '합리적', '객관적',
}

RE_BRACKET_TOPIC = re.compile(r'\[(\d+)\]\s*(.+?)(?=\[\d+\]|<br|$)')
RE_HTML = re.compile(r'<[^>]+>')

# 명사 추출: 블랙리스트(불용어) 방식 대신 → 비명사 어미 패턴 필터링 방식
# 한글 단어 중 아래 패턴으로 끝나는 것은 명사가 아님 (용언 어미/조사/부사 어미)
# 명사는 이런 어미로 끝나지 않음
NON_NOUN_ENDINGS = re.compile(
    r'(?:하여|하고|하며|하는|하면|하다|하게|하든|하니|하자|하였|하지|하더|하도|하려|하듯|'
    r'되어|되고|되며|되는|되면|되다|되지|되었|되니|되든|'
    r'있어|있는|있다|있고|있으|있을|있던|있음|'
    r'없어|없는|없다|없고|없으|없을|없던|없음|'
    r'받은|받는|받을|받고|받아|받지|'
    r'얻은|얻는|얻을|얻고|얻어|'
    r'된다|한다|이다|인지|인가|인바|'
    r'에서|에게|에의|으로|로서|로써|에는|에도|에만|'
    r'까지|부터|마다|조차|밖에|만큼|대로|같이|처럼|'
    r'에서의|으로서|으로써|에게서|'
    r'관한|대한|위한|따른|의한|인한|'
    r'관하여|대하여|위하여|의하여|인하여|관하여는|대하여는|'
    r'함으로써|됨으로써|'
    r'이므로|이라고|이라는|이라면|이라도|'
    r'있으므로|없으므로|'
    r'부당한|정당한|위법한|적법한|상당한|명확한|중요한|필요한|'
    r'아닌|같은|다른|모든|어떤|어느|'
    r'하여야|되어야|하여도|되어도|'
    r'이유있으므로|'
    r'때문에|경우에|여부의|여부가|여부를|'
    r'것이|것을|것은|것의|것에|것도|'
    r'수가|수를|수는|수도|'
    r'자의|자가|자는|자를|자에|자와|'
    r'바에|바의|바를|'
    r'조에|항에|호에|'
    r'등의|등에|등을|등이|등은|'
    r'있다고|없다고|한다고|된다고|'
    # X하는지/X한지/X할지 (의문형 어미)
    r'하는지|한지|할지|된지|인지|'
    # X할/X한/X된/X인 + 수/것 (관형어미)
    r'참작할|산정할|적용할|판단할|인정할|'
    r'순수한|특수한|고유한|중대한|명백한|현저한|'
    # X하는/X되는/X있는 (관형사형 어미)
    r'성립하는|해당하는|인정하는|적용하는|존재하는|'
    r'성립하는지|해당하는지)$'
)

# 추가 불용어: 위 패턴으로 안 잡히는 비명사
NON_NOUN_WORDS = {
    '있어', '없어', '하여', '되어', '받은', '받는', '얻을', '얻은',
    '있어서', '없어서', '있어서의', '관하여', '관하여는', '대하여', '대하여는',
    '의하여', '위하여', '인하여', '의한', '위한', '인한', '따른', '관한', '대한',
    '부당한', '정당한', '상당한', '위법한', '적법한', '명확한',
    '이므로', '있으므로', '없으므로', '이유있으므로',
    '아닌', '같은', '다른', '모든', '어떤', '어느', '각각', '서로',
    '또는', '및', '혹은', '내지', '다만', '특히', '매우', '아주', '다소',
    '경우', '사항', '여부', '때문', '이상', '이하', '기타', '해당',
    '판결', '선고', '대법원', '법원', '재판', '사건',
    '원고', '피고', '상고', '항소', '소극', '적극', '원칙', '판단',
    '기준', '요건', '범위', '효력', '의미', '내용', '방법', '절차',
    '제도', '규정', '조항', '법률', '법령', '적용', '해석',
    '전부', '일부', '주장', '인정', '기각', '각하', '파기', '환송',
    '취지', '이유', '결론', '결과', '주문', '증거', '증명', '입증',
    '제출', '진술', '신청', '청구', '심리', '변론', '종결', '확정',
    '원심', '상고심', '항소심', '가능', '불가', '곤란',
    '구체적', '일반적', '통상적', '합리적', '객관적',
}

RE_NOUN_CANDIDATE = re.compile(r'[가-힣]{2,10}')


def clean(text):
    return RE_HTML.sub('', text).strip()


def get_conn():
    conn = sqlite3.connect(f'file:{DB_PATH}?mode=ro', uri=True)
    conn.execute('PRAGMA cache_size = -128000')
    conn.execute('PRAGMA mmap_size = 536870912')
    return conn


def resolve_statute_slug(conn, query):
    """Resolve user input to a statute slug."""
    if query.startswith('statute::'):
        return query
    # Try alias lookup
    row = conn.execute(
        "SELECT d.title FROM vault_aliases a JOIN vault_documents d ON d.id = a.doc_id WHERE a.alias = ? LIMIT 1",
        (query,)
    ).fetchone()
    if row:
        return row[0]
    # Try partial match
    row = conn.execute(
        "SELECT d.title FROM vault_aliases a JOIN vault_documents d ON d.id = a.doc_id WHERE a.alias LIKE ? LIMIT 1",
        (f"%{query}%",)
    ).fetchone()
    if row:
        return row[0]
    return query


def compress_holding(text):
    """판시사항 쟁점 문구를 핵심만 남기는 압축 요약.

    예:
      '사해행위취소소송에서 채권자취소권의 행사기간에 관한 판단'
      → '채권자취소권의 행사기간'

      '민법상 불법행위가 되는 명예훼손의 의미'
      → '불법행위가 되는 명예훼손'
    """
    t = text.strip()
    t = re.sub(r'<[^>]+>', '', t)
    # 앞부분 법명 수식어
    t = re.sub(r'^(민법|상법|형법|헌법|민사소송법|형사소송법|행정소송법)상\s*', '', t)
    # 뒷부분 불필요 꼬리
    t = re.sub(r'의\s*의미\s*$', '', t)
    t = re.sub(r'의\s*판단\s*기준\s*$', '', t)
    t = re.sub(r'에\s*관한\s*(판단|법리)\s*$', '', t)
    t = re.sub(r'(인지|하는지|되는지|있는지)\s*여부\s*$', '', t)
    t = re.sub(r'여부의\s*판단\s*기준\s*$', '', t)
    t = re.sub(r'\s*여부\s*$', '', t)
    # 중간 연결어 → 공백으로 치환
    t = re.sub(r'\s*에\s*있어서의?\s*', ' ', t)
    t = re.sub(r'\s*에\s*있어\s*', ' ', t)
    t = re.sub(r'\s*에\s*관하여는?\s*', ' ', t)
    t = re.sub(r'\s*에\s*대하여\s*', ' ', t)
    t = re.sub(r'소송에서\s*', ' ', t)
    t = re.sub(r'사해행위취소\s*소송에서\s*', '', t)
    t = re.sub(r'\s+', ' ', t).strip()
    if len(t) > 60:
        t = t[:57] + '...'
    return t


def extract_holding_topics(content):
    """Extract issue topics from 판시사항.

    판시사항 format:
      [1] 첫번째 쟁점에 대한 설명 / [2] 두번째 쟁점 ...
    or:
      단일 쟁점 설명
    """
    m = re.search(r'## 판시사항\n(.+?)(?=\n## )', content, re.DOTALL)
    if not m:
        return []

    holding = clean(m.group(1))
    topics = []

    # Try numbered format: [1] ... [2] ...
    numbered = RE_BRACKET_TOPIC.findall(holding)
    if numbered:
        for num, text in numbered:
            text = text.strip().rstrip('/')
            if len(text) > 10:
                topics.append(text[:200])
    elif len(holding) > 10:
        topics.append(holding[:200])

    return topics


# 단어 끝에 붙는 한글 조사 (1글자)
# "불법행위로" → "불법행위", "과실이" → "과실", "타인의" → "타인"
# 주의: '자'는 조사가 아닌 접미사(채권자, 설정자)이므로 제외
# '인'도 접미사(채무자인, 대리인)와 혼동 → 제외
TRAILING_PARTICLES = set('을를은는이가의에로와과도만서며고')

# 단어 끝에 붙는 2글자 조사
TRAILING_PARTICLES_2 = {'에서', '으로', '에게', '에는', '에도', '와는', '과의', '로서', '로써', '까지', '부터', '마다'}


def strip_particle(word):
    """명사+조사 → 명사만 추출. 조사를 strip."""
    if len(word) <= 2:
        return word
    # 2글자 조사 먼저
    if len(word) > 3 and word[-2:] in TRAILING_PARTICLES_2:
        return word[:-2]
    # 1글자 조사
    if word[-1] in TRAILING_PARTICLES:
        stem = word[:-1]
        if len(stem) >= 2:
            return stem
    return word


def extract_keywords(text, max_kw=8):
    """Extract Korean legal noun phrases ONLY.

    법률용어 추출:
      1단계: 단어 추출 + 용언어미 필터
      2단계: 조사 stripping (불법행위로 → 불법행위)
      3단계: 비명사 불용어 + 소송절차 불용어 제거
      4단계: 법률용어만 남김
    """
    candidates = RE_NOUN_CANDIDATE.findall(text)

    seen = set()
    result = []
    for w in candidates:
        if len(w) < 2:
            continue

        # 1단계: 용언 어미 필터
        if NON_NOUN_ENDINGS.search(w):
            continue
        if w in NON_NOUN_WORDS:
            continue

        # 2단계: 조사 stripping
        w = strip_particle(w)
        if len(w) < 2:
            continue

        # stripping 후 다시 체크
        if w in NON_NOUN_WORDS or w in PROCEDURE_STOP_WORDS:
            continue
        if NON_NOUN_ENDINGS.search(w):
            continue

        if w in seen:
            continue

        # 3단계: 소송절차 불용어
        if w in PROCEDURE_STOP_WORDS:
            continue

        seen.add(w)
        result.append(w)
        if len(result) >= max_kw:
            break
    return result


# 2단계 불용어: 소송절차 일반어 + 비법률 일반명사
# (명사이지만 쟁점 키워드/법률용어가 아닌 것)
PROCEDURE_STOP_WORDS = {
    # 소송절차
    '판결', '선고', '대법원', '법원', '재판', '사건', '소송',
    '원고', '피고', '상고', '항소', '상소', '제소', '소장',
    '원심', '상고심', '항소심', '제1심',
    '증거', '증명', '입증', '소명', '자료',
    '제출', '진술', '신청', '청구', '청구인',
    '심리', '변론', '종결', '확정',
    '기각', '각하', '파기', '환송', '취하',
    '취지', '주문', '결론', '결과', '이유',
    '사항', '여부', '경우', '때문', '문제',
    '이상', '이하', '기타', '해당', '전부', '일부',
    '규정', '조항', '법률', '법령', '적용', '해석',
    '기준', '요건', '범위', '효력', '내용',
    '방법', '절차', '제도', '원칙',
    '당사자', '소송대리인', '변호사', '재판장',
    # 비법률 일반명사 (법률용어가 아닌 것)
    '의미', '성질', '정도', '종류', '상태', '형태',
    '관계', '행위', '사실', '사정', '점에',
    '필요', '가능', '목적',
    '정도', '비율', '금액',
    '시기', '기간', '기일', '시점', '당시',
    '상대방', '제3자', '본인', '대리인',
    '경위', '과정', '시작', '종료', '완료',
    '동의', '승낙', '거부', '거절', '수락',
    '주장', '인정', '부인', '의심', '의문',
    '고려', '참작', '감안', '고려하여',
    '위반', '위배', '저촉', '충돌',
    '특별한', '일반적', '통상적', '합리적', '객관적',
    '직접', '간접', '최초', '최종', '최근',
    '영향', '결과', '원인', '이유', '근거',
    '증가', '감소', '변경', '변동', '추가',
    '발생', '소멸', '존속', '유지', '보존',
    '통지', '고지', '통보', '보고', '제출',
}


def get_co_cited_statutes(conn, case_slug, target_statute):
    """Get other statutes cited alongside the target statute."""
    rows = conn.execute("""
        SELECT DISTINCT l.target_raw FROM vault_links l
        JOIN vault_documents d ON d.id = l.source_doc_id
        WHERE d.title = ? AND l.display_text = 'cites'
    """, (case_slug,)).fetchall()
    return [r[0] for r in rows if r[0] != target_statute and r[0] not in BOILERPLATE]


def get_backlink_counts(conn, case_slugs):
    """Get backlink counts (how many other cases cite each case in their 참조판례/본문).

    백링크 = 다른 판례의 판결이유에서 이 판례의 사건번호가 인용된 횟수.
    백링크가 많을수록 리딩 판례(기본 판례).

    Returns dict: case_slug -> backlink_count
    """
    if not case_slugs:
        return {}

    # Batch query: count ref-case links pointing TO each case
    placeholders = ','.join('?' for _ in case_slugs)
    rows = conn.execute(f"""
        SELECT l.target_raw, COUNT(*) as cnt
        FROM vault_links l
        WHERE l.display_text = 'ref-case' AND l.target_raw IN ({placeholders})
        GROUP BY l.target_raw
    """, case_slugs).fetchall()

    return {slug: cnt for slug, cnt in rows}


def build_issue_clusters(conn, statute_slug, max_cases=500):
    """Build issue clusters from cases citing this statute."""
    t0 = time.time()

    # Get all citing cases
    cases = conn.execute("""
        SELECT DISTINCT d.title, d.id, d.content FROM vault_links l
        JOIN vault_documents d ON d.id = l.source_doc_id
        WHERE l.target_raw = ? AND l.display_text = 'cites' AND d.doc_type = 'case'
        LIMIT ?
    """, (statute_slug, max_cases)).fetchall()

    if not cases:
        return None

    # ── Phase 1: Extract structured issues from 판시사항 ──
    #
    # 대법원 판례의 판시사항에는 쟁점이 [1], [2], [3] 형태로 정리되어 있다.
    # 이것이 가장 신뢰도 높은 쟁점 소스이므로 1순위로 사용한다.
    #
    # 클러스터링 전략:
    #   1순위: 판시사항 [N] 쟁점 텍스트의 핵심 키워드로 그룹핑
    #   2순위: 판결요지에서 쟁점 키워드 추출
    #   3순위: co-cited 법조 패턴 기반 그룹핑 (판시사항이 없는 경우)

    case_data = []
    # issue_text -> list of (case, topic_number) — 쟁점 텍스트별 판례 모음
    issue_phrases = []  # (keywords_tuple, full_text, case_index, topic_num)
    co_statute_counter = Counter()

    for slug, doc_id, content in cases:
        # 판시사항에서 [N] 쟁점 추출 (1순위)
        topics = extract_holding_topics(content)

        # 판결요지에서도 추출 (2순위 보강)
        if not topics:
            m = re.search(r'## 판결요지\n(.+?)(?=\n## )', content, re.DOTALL)
            if m:
                summary = clean(m.group(1))
                sum_topics = re.findall(r'\[(\d+)\]\s*(.+?)(?=\[\d+\]|$)', summary, re.DOTALL)
                if sum_topics:
                    topics = [t.strip()[:200] for _, t in sum_topics]
                elif len(summary) > 10:
                    topics = [summary[:200]]

        # 각 쟁점에서 핵심 키워드 추출
        topic_keywords = []
        for t in topics:
            kws = extract_keywords(t, max_kw=5)
            topic_keywords.append(kws)
            if kws:
                issue_phrases.append((
                    tuple(kws[:3]),  # 상위 3개 키워드를 클러스터 키로
                    t,
                    len(case_data),
                    len(topic_keywords) - 1,
                ))

        co_statutes = get_co_cited_statutes(conn, slug, statute_slug)
        for cs in co_statutes:
            co_statute_counter[cs] += 1

        fm = dict(conn.execute(
            "SELECT key, value FROM vault_frontmatter WHERE doc_id = ?", (doc_id,)
        ).fetchall())

        # 전원합의체 판별
        verdict_type = fm.get('verdict_type', '')
        is_en_banc = '전원합의체' in verdict_type or '전원합의체' in content[:500]

        # 판결요지 한줄 요약 추출
        summary_line = ''
        m_sum = re.search(r'## 판결요지\n(.+?)(?=\n## )', content, re.DOTALL)
        if m_sum:
            raw_sum = clean(m_sum.group(1))
            # [1] 이 있으면 첫 번째 쟁점만
            first_topic = re.match(r'(?:\[1\]\s*)?(.+?)(?:\[2\]|$)', raw_sum, re.DOTALL)
            if first_topic:
                summary_line = first_topic.group(1).strip()[:150]
            else:
                summary_line = raw_sum[:150]
        elif topics:
            summary_line = topics[0][:150]

        # 판결문 주요 섹션 추출 (사이드바에서 보여줄 전문)
        holding_full = ''
        m_h = re.search(r'## 판시사항\n(.+?)(?=\n## )', content, re.DOTALL)
        if m_h:
            holding_full = clean(m_h.group(1))[:2000]

        summary_full = ''
        if m_sum:
            summary_full = clean(m_sum.group(1))[:3000]

        ref_statutes_text = ''
        m_r = re.search(r'## 참조조문\n(.+?)(?=\n## )', content, re.DOTALL)
        if m_r:
            ref_statutes_text = clean(m_r.group(1))[:500]

        ref_cases_text = ''
        m_rc = re.search(r'## 참조판례\n(.+?)(?=\n## )', content, re.DOTALL)
        if m_rc:
            ref_cases_text = clean(m_rc.group(1))[:500]

        # 판례내용(판결이유) 전문 — 팝업용
        body_full = ''
        m_body = re.search(r'## 판례내용\n(.+?)(?=\n## |$)', content, re.DOTALL)
        if m_body:
            body_full = m_body.group(1).strip()
            body_full = re.sub(r'<br\s*/?>', '\n', body_full)
            body_full = re.sub(r'<[^>]+>', '', body_full)

        # 원심판결 정보 + 원심 판결문 전문 추출
        wonsim_info = None
        wonsim_pattern = re.compile(r'【원심판결】\s*(.+?)(?:<br|【|\n)', re.DOTALL)
        wm = wonsim_pattern.search(content)
        if wm:
            wonsim_raw = clean(wm.group(1))
            wonsim_case_nums = re.findall(r'(\d{2,4}[가-힣]{1,4}\d{1,7})', wonsim_raw)
            if wonsim_case_nums:
                wonsim_slug = f"case::{wonsim_case_nums[0]}"
                wonsim_row = conn.execute(
                    "SELECT id, content FROM vault_documents WHERE title = ?", (wonsim_slug,)
                ).fetchone()
                wonsim_fm = {}
                wonsim_holding = ''
                wonsim_summary = ''
                wonsim_body = ''
                wonsim_refs = ''
                if wonsim_row:
                    wonsim_id, wonsim_content = wonsim_row
                    wonsim_fm = dict(conn.execute(
                        "SELECT key, value FROM vault_frontmatter WHERE doc_id = ?", (wonsim_id,)
                    ).fetchall())
                    m_wh = re.search(r'## 판시사항\n(.+?)(?=\n## )', wonsim_content, re.DOTALL)
                    if m_wh: wonsim_holding = clean(m_wh.group(1))
                    m_ws = re.search(r'## 판결요지\n(.+?)(?=\n## )', wonsim_content, re.DOTALL)
                    if m_ws: wonsim_summary = clean(m_ws.group(1))
                    m_wb = re.search(r'## 판례내용\n(.+?)(?=\n## |$)', wonsim_content, re.DOTALL)
                    if m_wb:
                        wonsim_body = m_wb.group(1).strip()
                        wonsim_body = re.sub(r'<br\s*/?>', '\n', wonsim_body)
                        wonsim_body = re.sub(r'<[^>]+>', '', wonsim_body)
                    m_wr = re.search(r'## 참조조문\n(.+?)(?=\n## )', wonsim_content, re.DOTALL)
                    if m_wr: wonsim_refs = clean(m_wr.group(1))

                wonsim_info = {
                    'case_number': wonsim_case_nums[0],
                    'slug': wonsim_slug,
                    'exists': wonsim_row is not None,
                    'raw_text': wonsim_raw,
                    'court': wonsim_fm.get('court_name', ''),
                    'date': wonsim_fm.get('verdict_date', ''),
                    'case_name': wonsim_fm.get('case_name', ''),
                    'holding': wonsim_holding[:3000],
                    'summary': wonsim_summary[:5000],
                    'body': wonsim_body,
                    'refs': wonsim_refs[:500],
                }

        case_data.append({
            'slug': slug,
            'case_number': fm.get('case_number', slug.replace('case::', '')),
            'case_name': fm.get('case_name', ''),
            'court': fm.get('court_name', ''),
            'date': fm.get('verdict_date', ''),
            'verdict_type': fm.get('verdict_type', ''),
            'topics': topics,
            'topic_keywords': topic_keywords,
            'co_statutes': co_statutes,
            'is_en_banc': is_en_banc,
            'summary_line': summary_line,
            'holding_full': holding_full,
            'summary_full': summary_full,
            'ref_statutes': ref_statutes_text,
            'ref_cases': ref_cases_text,
            'body_full': body_full,
            'wonsim': wonsim_info,
        })

    total_cases = len(case_data)

    # ── Phase 1.5: 중요 판례 식별 (리딩/전원합의체/동지) ──
    #
    # 중요 판례 3가지 유형:
    #   1. 리딩 판례: 백링크(다른 판례에서 인용된 횟수)가 많은 판례
    #   2. 전원합의체: 대법원 전원합의체 판결/결정
    #   3. 동지 판례: 동일 취지로 반복 인용되는 확립된 법리
    #
    # 동지 판례 감지 방법:
    #   - 참조판례(ref-case) 링크에서 같은 타겟을 3회 이상 공유하는 판례 그룹
    #   - = 여러 판례가 동일 선행 판례를 참조 → 확립된 법리

    all_slugs = [c['slug'] for c in case_data]
    backlink_map = get_backlink_counts(conn, all_slugs)

    # 동지 판례 감지: 각 판례가 참조하는 판례 목록을 수집
    ref_case_map = {}  # case_slug -> set of referenced case slugs
    for case in case_data:
        refs = conn.execute("""
            SELECT l.target_raw FROM vault_links l
            JOIN vault_documents d ON d.id = l.source_doc_id
            WHERE d.title = ? AND l.display_text = 'ref-case'
        """, (case['slug'],)).fetchall()
        ref_case_map[case['slug']] = set(r[0] for r in refs)

    # 공통 참조판례 카운트: 어떤 선행판례가 N회 이상 참조되면 "확립 법리"
    shared_ref_counter = Counter()
    for refs in ref_case_map.values():
        for r in refs:
            shared_ref_counter[r] += 1

    # 확립 법리 기준: 이 법조 관련 판례들 중 3회 이상 참조된 선행판례
    established_refs = {ref for ref, cnt in shared_ref_counter.items() if cnt >= 3}

    for case in case_data:
        bl = backlink_map.get(case['slug'], 0)
        case['backlink_count'] = bl
        case['is_leading'] = False  # 클러스터 내에서 결정

        # 동지 판례: 이 판례가 참조하는 선행판례 중 확립 법리가 있으면 동지
        refs = ref_case_map.get(case['slug'], set())
        case['is_dongji'] = bool(refs & established_refs)
        case['dongji_refs'] = list(refs & established_refs)[:5]

        # 중요도 종합 판정
        case['importance'] = 'normal'
        if case.get('is_en_banc'):
            case['importance'] = 'en_banc'  # 최고 중요도
        elif bl >= 10:
            case['importance'] = 'leading'  # 리딩 판례
        elif case['is_dongji']:
            case['importance'] = 'dongji'  # 확립 법리

    # ── Phase 2: 쟁점 키워드 기반 클러스터링 ──
    #
    # 각 판례의 판시사항 쟁점에서 추출한 키워드가 겹치면 같은 쟁점 그룹.
    # 핵심: 키워드 겹침 수가 2개 이상이면 같은 클러스터.

    # Count keyword frequency across all issues
    kw_counter = Counter()
    for kws_tuple, _, _, _ in issue_phrases:
        for kw in kws_tuple:
            kw_counter[kw] += 1

    # Find "issue keywords" — common enough to form a cluster (3+ occurrences)
    # but not so common they're meaningless (< 60% of cases)
    cluster_keywords = []
    for kw, cnt in kw_counter.most_common(200):
        if cnt >= 3 and cnt < total_cases * 0.6:
            cluster_keywords.append(kw)

    # Assign each case to its best-matching cluster keyword
    clusters = defaultdict(list)
    clustered_cases = set()

    for case_idx, case in enumerate(case_data):
        matched = False
        # Try matching by topic keywords (판시사항 기반)
        all_case_kws = set()
        for kws in case['topic_keywords']:
            all_case_kws.update(kws)

        for ckw in cluster_keywords:
            if ckw in all_case_kws:
                # Find the topic text that contains this keyword → use as cluster label
                best_topic = ''
                for i, kws in enumerate(case['topic_keywords']):
                    if ckw in kws and i < len(case['topics']):
                        best_topic = case['topics'][i]
                        break
                clusters[ckw].append(case)
                clustered_cases.add(case_idx)
                matched = True
                break

        if not matched:
            # Fallback: co-cited statute based clustering
            for cs, _ in co_statute_counter.most_common(30):
                if cs in case['co_statutes']:
                    cs_label = cs.replace('statute::', '').replace('::', ' 제') + '조 관련'
                    clusters[cs_label].append(case)
                    clustered_cases.add(case_idx)
                    matched = True
                    break

    # Unclustered cases
    unclustered = [c for i, c in enumerate(case_data) if i not in clustered_cases]

    # Merge small clusters
    merged = {}
    small_cases = []
    for key, cases_in_cluster in clusters.items():
        if len(cases_in_cluster) >= 2:
            merged[key] = cases_in_cluster
        else:
            small_cases.extend(cases_in_cluster)
    if small_cases or unclustered:
        merged['기타 쟁점'] = small_cases + unclustered

    # ── Phase 3: Enrich clusters with representative 판시사항 text ──
    result_clusters = []
    for issue_name, case_list in sorted(merged.items(), key=lambda x: -len(x[1])):
        # Find the best representative topic text from 판시사항
        representative_topics = []
        for c in case_list:
            for t in c['topics']:
                # Check if this topic relates to the cluster keyword
                if issue_name in t or any(kw in t for kws in c['topic_keywords'] for kw in kws if kw == issue_name):
                    if t not in representative_topics:
                        representative_topics.append(t[:300])
                        break
            if len(representative_topics) >= 3:
                break
        # Fallback: just take first topics
        if not representative_topics:
            for c in case_list[:3]:
                for t in c['topics'][:1]:
                    representative_topics.append(t[:300])

        # Co-statutes common in this cluster
        cluster_co_statutes = Counter()
        for c in case_list:
            for cs in c['co_statutes']:
                cluster_co_statutes[cs] += 1
        top_co_statutes = [
            {'slug': cs, 'label': cs.replace('statute::', '').replace('::', ' 제') + '조', 'count': cnt}
            for cs, cnt in cluster_co_statutes.most_common(5)
            if cnt >= 2
        ]

        # ── Identify leading case per cluster by backlink count ──
        # 백링크가 가장 많은 판례 = 이 쟁점의 리딩 판례(기본 판례)
        max_bl = max((c.get('backlink_count', 0) for c in case_list), default=0)
        for c in case_list:
            bl = c.get('backlink_count', 0)
            c['is_leading'] = (bl > 0 and bl >= max_bl * 0.5 and bl >= 3)

        # Sort: leading cases first (by backlink desc), then by date desc
        sorted_cases = sorted(case_list, key=lambda c: (
            -1 if c.get('is_leading') else 0,
            -(c.get('backlink_count', 0)),
            c.get('date', '') or '',
        ), reverse=False)
        # Fix sort: leading first, then high backlink, then recent date
        sorted_cases = sorted(case_list, key=lambda c: (
            not c.get('is_leading', False),
            -c.get('backlink_count', 0),
            -(int(c.get('date', '0') or '0')),
        ))

        # Find THE leading case (highest backlink)
        leading_case = max(case_list, key=lambda c: c.get('backlink_count', 0)) if case_list else None
        leading_slug = leading_case['slug'] if leading_case and leading_case.get('backlink_count', 0) >= 3 else None

        result_clusters.append({
            'issue_name': issue_name,
            'case_count': len(case_list),
            'representative_topics': representative_topics,
            'co_statutes': top_co_statutes,
            'leading_case_slug': leading_slug,
            'leading_backlinks': leading_case.get('backlink_count', 0) if leading_case else 0,
            'cases': sorted_cases,
        })

    # Top keywords for info
    issue_keywords = [(kw, cnt) for kw, cnt in kw_counter.most_common(20)
                      if cnt >= 2 and cnt < total_cases * 0.8]

    # Statute metadata
    statute_meta = {}
    row = conn.execute(
        "SELECT d.id FROM vault_aliases a JOIN vault_documents d ON d.id = a.doc_id WHERE a.alias = ? LIMIT 1",
        (statute_slug,)
    ).fetchone()
    if not row:
        row = conn.execute("SELECT d.id FROM vault_documents d WHERE d.title LIKE ? LIMIT 1", (f"{statute_slug}%",)).fetchone()
    if row:
        fm = dict(conn.execute("SELECT key, value FROM vault_frontmatter WHERE doc_id = ?", (row[0],)).fetchall())
        statute_meta = fm

    return {
        'statute_slug': statute_slug,
        'statute_label': statute_slug.replace('statute::', '').replace('::', ' 제') + '조',
        'statute_meta': statute_meta,
        'total_cases': total_cases,
        'clusters': result_clusters,
        'top_keywords': issue_keywords[:20],
        'top_co_statutes': [
            {'slug': cs, 'label': cs.replace('statute::', '').replace('::', ' 제') + '조', 'count': cnt}
            for cs, cnt in co_statute_counter.most_common(15)
        ],
        'build_time': time.time() - t0,
    }


# ──────────────────────────────────────────
# Mindmap HTML
# ──────────────────────────────────────────
MINDMAP_HTML = """<!DOCTYPE html><!-- v3 clean -->
<html lang="ko">
<head>
<meta charset="UTF-8">
<title>Legal Mindmap - {statute_label}</title>
<style>
* {{ margin: 0; padding: 0; box-sizing: border-box; }}
body {{ font-family: 'Pretendard', -apple-system, sans-serif; background: #0d1117; color: #c9d1d9; }}
#mindmap-container {{ width: 100vw; height: 100vh; overflow: auto; position: relative; }}
svg {{ width: 100%; height: 100%; }}
.link {{ fill: none; }}
text {{ font-size: 12px; fill: #c9d1d9; cursor: default; }}
/* no box/circle — text only */

#sidebar {{
    position: fixed; right: 0; top: 0; width: 480px; height: 100vh;
    background: #161b22; border-left: 1px solid #30363d;
    padding: 20px; overflow-y: auto; display: none; z-index: 100;
}}
#sidebar.visible {{ display: block; }}
#sidebar h2 {{ color: #f0883e; margin-bottom: 8px; font-size: 15px; }}
#sidebar h3 {{ color: #58a6ff; margin: 14px 0 6px; font-size: 13px; border-bottom: 1px solid #21262d; padding-bottom: 4px; }}
#sidebar .section-text {{ color: #8b949e; font-size: 12px; line-height: 1.6; white-space: pre-wrap; word-break: break-all; margin: 4px 0 12px; }}
#sidebar .badge {{ display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; margin: 2px; }}
#sidebar .badge-leading {{ background: #ffd70030; color: #ffd700; }}
#sidebar .badge-enbanc {{ background: #ff444430; color: #ff4444; }}
#sidebar .badge-dongji {{ background: #3fb95030; color: #3fb950; }}
#sidebar .case-item {{
    padding: 8px; margin: 4px 0; background: #0d1117; border-radius: 6px;
    border: 1px solid #21262d; font-size: 12px; cursor: pointer;
}}
#sidebar .case-item:hover {{ border-color: #58a6ff; }}
#sidebar .co-statute {{ display: inline-block; padding: 2px 8px; margin: 2px; background: #21262d; border-radius: 4px; font-size: 11px; color: #8b949e; }}
#sidebar .close-btn {{
    position: absolute; top: 12px; right: 12px; background: none; border: none;
    color: #8b949e; font-size: 24px; cursor: pointer; line-height: 1;
}}
#sidebar .close-btn:hover {{ color: #c9d1d9; }}

#stats-bar {{
    position: fixed; top: 0; left: 0; right: 400px; background: #161b22;
    border-bottom: 1px solid #30363d; padding: 12px 20px; z-index: 50;
    display: flex; gap: 24px; align-items: center; font-size: 13px;
}}
#stats-bar .stat {{ color: #8b949e; }}
#stats-bar .stat b {{ color: #58a6ff; }}

#controls {{
    position: fixed; bottom: 12px; left: 12px; z-index: 50;
    display: flex; gap: 8px;
}}
#controls button {{
    background: #161b22; border: 1px solid #30363d; color: #c9d1d9;
    padding: 6px 14px; border-radius: 6px; cursor: pointer; font-size: 12px;
}}
#controls button:hover {{ border-color: #58a6ff; }}
</style>
</head>
<body>
<div id="stats-bar">
    <span class="stat"><b>{statute_label}</b></span>
    <span class="stat">Total Cases: <b>{total_cases}</b></span>
    <span class="stat">Issue Groups: <b>{cluster_count}</b></span>
    <span class="stat">Build: <b>{build_time:.1f}s</b></span>
</div>
<div id="mindmap-container">
    <svg id="mindmap"></svg>
</div>
<div id="sidebar">
    <button class="close-btn" onclick="closeSidebar()">x</button>
    <div id="sidebar-content"></div>
</div>
<div id="controls">
    <button onclick="zoomIn()">+ Zoom In</button>
    <button onclick="zoomOut()">- Zoom Out</button>
    <button onclick="resetView()">Reset</button>
    <button onclick="expandAll()">Expand All</button>
    <button onclick="collapseAll()">Collapse All</button>
</div>
<div style="position:fixed;bottom:12px;right:12px;background:#161b22;border:1px solid #30363d;border-radius:8px;padding:12px;font-size:11px;z-index:50">
    <div style="margin:3px 0"><b style="color:#ffd700">금색 볼드</b> = 리딩 판례 (N) = 인용횟수</div>
    <div style="margin:3px 0"><b style="color:#ff4444">빨강 [전합]</b> = 전원합의체 판결</div>
    <div style="margin:3px 0"><b style="color:#3fb950">녹색 볼드</b> = 동지 (확립 법리)</div>
    <div style="margin:3px 0;color:#8b949e">일반 = 일반 판례</div>
    <div style="margin-top:6px;color:#484f58">* 클릭하면 판결문 상세 보기</div>
</div>

<script src="https://d3js.org/d3.v7.min.js"></script>
<script>
const DATA = {data_json};
const ISSUE_COLORS = [
    '#f0883e', '#58a6ff', '#3fb950', '#bc8cff', '#f778ba',
    '#ffa657', '#79c0ff', '#56d364', '#d2a8ff', '#ff7b72',
    '#a5d6ff', '#7ee787', '#e2c08d', '#ff9bce', '#ffd700',
];

const width = window.innerWidth;
const height = window.innerHeight;
const cx = width / 2;
const cy = height / 2;

const svg = d3.select('#mindmap');
const g = svg.append('g');

const zoom = d3.zoom().scaleExtent([0.2, 4]).on('zoom', e => g.attr('transform', e.transform));
svg.call(zoom);

// State
let expandedIssues = new Set();

function render() {{
    g.selectAll('*').remove();

    const clusters = DATA.clusters;
    const n = clusters.length;

    // Center node — TEXT ONLY, no circle
    g.append('text').attr('x', cx).attr('y', cy)
        .attr('text-anchor', 'middle').attr('dominant-baseline', 'middle')
        .attr('fill', '#f0883e').attr('font-size', '18px').attr('font-weight', '800')
        .text(DATA.statute_label);

    // Issue branches
    clusters.forEach((cluster, i) => {{
        const angle = (2 * Math.PI * i / n) - Math.PI / 2;
        const issueR = 220;
        const ix = cx + issueR * Math.cos(angle);
        const iy = cy + issueR * Math.sin(angle);
        const color = ISSUE_COLORS[i % ISSUE_COLORS.length];
        const isExpanded = expandedIssues.has(i);

        // Link center -> issue (선명한 연결선)
        g.append('path')
            .attr('d', `M${{cx}},${{cy}} Q${{(cx+ix)/2 + 30*Math.sin(angle)}},${{(cy+iy)/2 - 30*Math.cos(angle)}} ${{ix}},${{iy}}`)
            .attr('stroke', color).attr('stroke-width', 2.5).attr('stroke-opacity', 0.8).attr('fill', 'none');

        // Issue node
        const issueG = g.append('g').style('cursor', 'pointer')
            .on('click', () => {{
                if (expandedIssues.has(i)) expandedIssues.delete(i);
                else expandedIssues.add(i);
                render();
            }});

        // Issue node — TEXT ONLY, no box
        const issueLabel = (cluster.issue_name.length > 25 ? cluster.issue_name.slice(0,25)+'...' : cluster.issue_name)
            + ' (' + cluster.case_count + ')';
        issueG.append('text')
            .attr('x', ix).attr('y', iy)
            .attr('text-anchor', 'middle').attr('dominant-baseline', 'middle')
            .attr('fill', color).attr('font-size', '14px').attr('font-weight', '700')
            .style('cursor', 'pointer')
            .text(issueLabel);

        // Expanded: show cases with LEADING CASE at center of cluster
        if (isExpanded) {{
            const cases = cluster.cases.slice(0, 30);
            const leadingCases = cases.filter(c => c.is_leading);
            const normalCases = cases.filter(c => !c.is_leading);
            const caseR = 120;
            const leadingR = 50; // Leading case is closer to issue node

            // Draw leading case(s) first — larger, closer, gold colored
            leadingCases.forEach((c, j) => {{
                const lAngle = angle + (j - leadingCases.length/2) * 0.2;
                const lcx = ix + leadingR * Math.cos(lAngle);
                const lcy = iy + leadingR * Math.sin(lAngle);

                // 선명한 연결선: issue -> leading case
                g.append('line')
                    .attr('x1', ix).attr('y1', iy).attr('x2', lcx).attr('y2', lcy)
                    .attr('stroke', '#ffd700').attr('stroke-width', 3).attr('stroke-opacity', 0.9);

                const leadG = g.append('g').style('cursor', 'pointer')
                    .on('click', () => showCaseDetail(cluster, c));
                // Leading case — 금색 볼드 (LEADING 텍스트 없이, 인용횟수만)
                leadG.append('text')
                    .attr('x', lcx).attr('y', lcy)
                    .attr('text-anchor', 'middle')
                    .attr('fill', '#ffd700').attr('font-size', '13px').attr('font-weight', '700')
                    .text((c.case_number || '').slice(0,25) + ' (' + (c.backlink_count||0) + ')');

                // Links from leading case to normal cases (spoke pattern)
                // 중요 판례 색상/스타일:
                //   en_banc(전원합의체): #ff4444 빨강 볼드
                //   leading(리딩): #ffd700 금색 볼드
                //   dongji(동지/확립법리): #3fb950 녹색 볼드
                //   normal: 기본색
                normalCases.slice(0, 20).forEach((nc, k) => {{
                    const nAngle = angle - Math.PI*0.4 + (Math.PI*0.8 * k / Math.max(normalCases.length - 1, 1));
                    const ncx = lcx + (caseR - leadingR) * Math.cos(nAngle);
                    const ncy = lcy + (caseR - leadingR) * Math.sin(nAngle);

                    const imp = nc.importance || 'normal';
                    const caseColor = imp === 'en_banc' ? '#ff4444' : imp === 'leading' ? '#ffd700' : imp === 'dongji' ? '#3fb950' : color;
                    const isBold = imp !== 'normal';
                    const lineW = isBold ? 1.5 : 0.5;

                    g.append('line')
                        .attr('x1', lcx).attr('y1', lcy).attr('x2', ncx).attr('y2', ncy)
                        .attr('stroke', caseColor).attr('stroke-width', isBold ? 2 : 1.2).attr('stroke-opacity', isBold ? 0.8 : 0.5);

                    const caseG = g.append('g').style('cursor', 'pointer')
                        .on('click', () => showCaseDetail(cluster, nc));

                    // 사건번호 + 중요도만 (판결요지 한줄 제거)
                    const label = (nc.case_number||'').slice(0,20)
                        + (imp === 'en_banc' ? ' [전합]' : '')
                        + ((nc.backlink_count||0) > 0 ? ' ('+nc.backlink_count+')' : '');
                    caseG.append('text')
                        .attr('x', ncx).attr('y', ncy)
                        .attr('text-anchor', 'start')
                        .attr('fill', caseColor)
                        .attr('font-weight', isBold ? '700' : '400')
                        .attr('font-size', isBold ? '12px' : '11px')
                        .text(label);
                }});
            }});

            // If no leading case, fall back to radial layout (with importance styling)
            if (leadingCases.length === 0) {{
            const angleSpan = Math.min(Math.PI * 0.6, cases.length * 0.08);
            cases.forEach((c, j) => {{
                const cAngle = angle - angleSpan/2 + (angleSpan * j / Math.max(cases.length - 1, 1));
                const ccx = ix + caseR * Math.cos(cAngle);
                const ccy = iy + caseR * Math.sin(cAngle);

                const imp = c.importance || 'normal';
                const caseColor = imp === 'en_banc' ? '#ff4444' : imp === 'leading' ? '#ffd700' : imp === 'dongji' ? '#3fb950' : color;
                const isBold = imp !== 'normal';

                g.append('line')
                    .attr('x1', ix).attr('y1', iy).attr('x2', ccx).attr('y2', ccy)
                    .attr('stroke', caseColor).attr('stroke-width', isBold ? 2 : 1.2).attr('stroke-opacity', isBold ? 0.8 : 0.5);

                const caseG = g.append('g').style('cursor', 'pointer')
                    .on('click', () => showCaseDetail(cluster, c));

                // 사건번호 + 중요도만
                const label = ((c.case_number||'').length > 20 ? (c.case_number||'').slice(0,20) : (c.case_number||''))
                    + (imp === 'en_banc' ? ' [전합]' : '')
                    + ((c.backlink_count||0) > 0 ? ' ('+c.backlink_count+')' : '');
                caseG.append('text')
                    .attr('x', ccx).attr('y', ccy)
                    .attr('text-anchor', 'start')
                    .attr('fill', caseColor)
                    .attr('font-weight', isBold ? '700' : '400')
                    .attr('font-size', isBold ? '12px' : '11px')
                    .text(label);
            }});
        }} // end no-leading fallback
        }} // end isExpanded
    }});
}}

function showCaseDetail(cluster, c) {{
    const sidebar = document.getElementById('sidebar');
    sidebar.classList.add('visible');
    // ── 사이드바: 판결문 전문 표시 ──
    let html = '';
    // 사건 기본정보
    html += `<h2>${{esc(c.case_number)}} ${{esc(c.case_name || '')}}</h2>`;
    html += `<p style="color:#8b949e;margin-bottom:8px">${{esc(c.court || '')}} ${{esc(c.date || '')}} ${{esc(c.verdict_type || '')}}</p>`;
    // 중요도 배지
    if (c.is_leading) html += `<span class="badge badge-leading">LEADING (${{c.backlink_count||0}}회 인용)</span> `;
    if (c.is_en_banc) html += `<span class="badge badge-enbanc">전원합의체</span> `;
    if (c.is_dongji) html += `<span class="badge badge-dongji">확립 법리(동지)</span> `;
    if ((c.backlink_count||0) > 0 && !c.is_leading) html += `<span style="color:#8b949e;font-size:11px">인용 ${{c.backlink_count}}회</span>`;
    html += '<br/>';

    // 판결문 전문보기 링크
    if (c.body_full || c.holding_full || c.summary_full) {{
        html += `<div style="margin:8px 0"><a href="#" onclick="openFullText(event, ${{JSON.stringify(c).replace(/</g,'\\u003c').replace(/>/g,'\\u003e')}})" style="color:#58a6ff;font-size:13px;font-weight:600;text-decoration:underline;cursor:pointer">판결문 전문보기 &rarr;</a></div>`;
    }}

    // 쟁점 그룹
    html += `<p style="color:#58a6ff;font-size:12px;margin:8px 0">쟁점: ${{esc(cluster.issue_name)}}</p>`;

    // 3심급 연결 — 원심 클릭 시 팝업
    if (c.wonsim) {{
        html += '<h3>심급 연결</h3>';
        if (c.wonsim.exists) {{
            html += `<div class="case-item" style="cursor:pointer" onclick='openWonsimPopup(${{JSON.stringify(c.wonsim).replace(/'/g,"&#39;")}})'>
                <b>원심(2심)</b>: <span style="color:#58a6ff;text-decoration:underline">${{esc(c.wonsim.case_number)}}</span>
                <div style="color:#8b949e;font-size:11px">${{esc(c.wonsim.court||'')}} ${{esc(c.wonsim.date||'')}} ${{esc(c.wonsim.case_name||'')}}</div>
                <div style="color:#58a6ff;font-size:11px">클릭하면 원심판결문 전문 팝업 &rarr;</div>
            </div>`;
        }} else {{
            html += `<div class="case-item">
                <b>원심(2심)</b>: ${{esc(c.wonsim.case_number)}}
                <div style="color:#8b949e;font-size:11px">${{esc(c.wonsim.raw_text)}}</div>
                <div style="color:#ff7b72;font-size:11px">DB에 미수록</div>
            </div>`;
        }}
    }} else if (c.court_hierarchy && c.court_hierarchy.length) {{
        html += '<h3>심급 연결</h3>';
        c.court_hierarchy.forEach(ch => {{
            const rel = ch.relation === 'wonsim' ? '원심(2심)' : '1심';
            html += `<div class="case-item"><b>${{rel}}</b>: ${{esc(ch.case_number)}}</div>`;
        }});
    }}

    // 판시사항 전문
    if (c.holding_full) {{
        html += '<h3>판시사항</h3>';
        html += `<div class="section-text">${{esc(c.holding_full)}}</div>`;
    }} else if (c.topics && c.topics.length) {{
        html += '<h3>판시사항</h3>';
        c.topics.forEach(t => {{ html += `<div class="section-text">${{esc(t)}}</div>`; }});
    }}

    // 판결요지 전문
    if (c.summary_full) {{
        html += '<h3>판결요지</h3>';
        html += `<div class="section-text">${{esc(c.summary_full)}}</div>`;
    }}

    // 참조조문
    if (c.ref_statutes) {{
        html += '<h3>참조조문 (적용법조)</h3>';
        html += `<div class="section-text">${{esc(c.ref_statutes)}}</div>`;
    }}
    if (c.co_statutes && c.co_statutes.length) {{
        c.co_statutes.forEach(cs => {{
            const label = cs.replace('statute::', '').replace('::', ' 제') + '조';
            html += `<span class="co-statute">${{esc(label)}}</span>`;
        }});
        html += '<br/>';
    }}

    // 참조판례
    if (c.ref_cases) {{
        html += '<h3>참조판례</h3>';
        html += `<div class="section-text">${{esc(c.ref_cases)}}</div>`;
    }}

    // 같은 쟁점 판례 목록
    html += `<h3>같은 쟁점 (${{cluster.case_count}}건)</h3>`;
    cluster.cases.slice(0, 30).forEach(oc => {{
        const isCurrent = oc.slug === c.slug;
        const isLead = oc.is_leading;
        const style = isCurrent ? 'border-color:#f0883e' : (isLead ? 'border-color:#ffd700' : '');
        const prefix = isLead ? '<b style="color:#ffd700">*</b> ' : '';
        html += `<div class="case-item" style="${{style}}" onclick="showCaseDetail(DATA.clusters[${{DATA.clusters.indexOf(cluster)}}], ${{JSON.stringify(oc).replace(/"/g, '&quot;')}})">
            ${{prefix}}<b>${{esc(oc.case_number)}}</b> ${{esc(oc.case_name || '')}}
            <div style="color:#8b949e;font-size:11px">${{esc(oc.court || '')}} ${{esc(oc.date || '')}}${{(oc.backlink_count||0) > 0 ? ' | '+oc.backlink_count+'회' : ''}}</div>
        </div>`;
    }});
    document.getElementById('sidebar-content').innerHTML = html;
}}

function closeSidebar() {{ document.getElementById('sidebar').classList.remove('visible'); }}
function esc(s) {{ const d = document.createElement('div'); d.textContent = s; return d.innerHTML; }}

function openFullText(event, c) {{
    event.preventDefault();
    const w = window.open('', '_blank', 'width=900,height=800,scrollbars=yes,resizable=yes');
    if (!w) {{ alert('팝업이 차단되었습니다. 브라우저 설정에서 팝업을 허용해주세요.'); return; }}
    const doc = w.document;
    doc.open();
    doc.write(`<!DOCTYPE html><html lang="ko"><head><meta charset="UTF-8">
    <title>${{esc(c.case_number)}} - 판결문 전문</title>
    <style>
        body {{ font-family: 'Pretendard', -apple-system, 'Malgun Gothic', sans-serif; background: #fff; color: #1a1a1a; padding: 30px 40px; line-height: 1.8; max-width: 850px; margin: 0 auto; }}
        h1 {{ font-size: 20px; color: #1a1a1a; border-bottom: 2px solid #333; padding-bottom: 10px; margin-bottom: 20px; }}
        h2 {{ font-size: 16px; color: #0066cc; margin: 24px 0 8px; border-left: 4px solid #0066cc; padding-left: 12px; }}
        .meta {{ color: #666; font-size: 14px; margin-bottom: 16px; }}
        .badge {{ display: inline-block; padding: 2px 10px; border-radius: 4px; font-size: 12px; font-weight: 700; margin-right: 6px; }}
        .badge-leading {{ background: #fff3cd; color: #856404; border: 1px solid #ffc107; }}
        .badge-enbanc {{ background: #f8d7da; color: #721c24; border: 1px solid #f5c6cb; }}
        .badge-dongji {{ background: #d4edda; color: #155724; border: 1px solid #c3e6cb; }}
        .section {{ margin: 12px 0; font-size: 14px; white-space: pre-wrap; word-break: break-all; }}
        .section p {{ margin: 6px 0; }}
        hr {{ border: none; border-top: 1px solid #ddd; margin: 20px 0; }}
        @media print {{ body {{ padding: 20px; }} }}
    </style></head><body>`);

    // Header
    doc.write(`<h1>${{esc(c.case_number)}}</h1>`);
    doc.write(`<div class="meta">`);
    doc.write(`${{esc(c.court || '')}} ${{esc(c.date || '')}} ${{esc(c.verdict_type || '')}}<br/>`);
    if (c.case_name) doc.write(`<b>${{esc(c.case_name)}}</b><br/>`);
    if (c.is_leading) doc.write(`<span class="badge badge-leading">LEADING (${{c.backlink_count||0}}회 인용)</span>`);
    if (c.is_en_banc) doc.write(`<span class="badge badge-enbanc">전원합의체</span>`);
    if (c.is_dongji) doc.write(`<span class="badge badge-dongji">확립 법리(동지)</span>`);
    doc.write(`</div>`);

    // 판시사항
    if (c.holding_full) {{
        doc.write(`<h2>판시사항</h2><div class="section">${{esc(c.holding_full)}}</div>`);
    }}

    // 판결요지
    if (c.summary_full) {{
        doc.write(`<h2>판결요지</h2><div class="section">${{esc(c.summary_full)}}</div>`);
    }}

    // 참조조문
    if (c.ref_statutes) {{
        doc.write(`<h2>참조조문</h2><div class="section">${{esc(c.ref_statutes)}}</div>`);
    }}

    // 참조판례
    if (c.ref_cases) {{
        doc.write(`<h2>참조판례</h2><div class="section">${{esc(c.ref_cases)}}</div>`);
    }}

    // 판례내용 (판결이유 전문)
    if (c.body_full) {{
        doc.write(`<hr/><h2>판결이유 (전문)</h2><div class="section">${{esc(c.body_full)}}</div>`);
    }} else {{
        doc.write(`<hr/><p style="color:#999">판결이유 전문이 데이터에 포함되어 있지 않습니다.</p>`);
    }}

    // 원심판결 정보 + 클릭 시 원심 팝업
    if (c.wonsim) {{
        const ws = c.wonsim;
        doc.write(`<hr/><h2>원심판결</h2>`);
        doc.write(`<div class="meta">${{esc(ws.raw_text)}}</div>`);
        if (ws.exists) {{
            doc.write(`<p><a href="#" id="wonsim-link" style="color:#0066cc;font-size:14px;font-weight:700;text-decoration:underline;cursor:pointer">${{esc(ws.case_number)}} 원심판결문 전문보기 &rarr;</a></p>`);
        }} else {{
            doc.write(`<p style="color:#999">${{esc(ws.case_number)}} - DB에 원심판결문이 없습니다</p>`);
        }}
    }}

    doc.write(`</body></html>`);
    doc.close();

    // 원심 클릭 이벤트 (팝업 안에서 새 팝업)
    if (c.wonsim && c.wonsim.exists) {{
        const wsLink = w.document.getElementById('wonsim-link');
        if (wsLink) {{
            wsLink.addEventListener('click', function(e) {{
                e.preventDefault();
                openWonsimPopup(c.wonsim);
            }});
        }}
    }}
}}

function openWonsimPopup(ws) {{
    const w2 = window.open('', '_blank', 'width=900,height=800,scrollbars=yes,resizable=yes');
    if (!w2) {{ alert('팝업이 차단되었습니다.'); return; }}
    const doc = w2.document;
    doc.open();
    doc.write(`<!DOCTYPE html><html lang="ko"><head><meta charset="UTF-8">
    <title>${{esc(ws.case_number)}} - 원심판결문</title>
    <style>
        body {{ font-family: 'Pretendard', -apple-system, 'Malgun Gothic', sans-serif; background: #fff; color: #1a1a1a; padding: 30px 40px; line-height: 1.8; max-width: 850px; margin: 0 auto; }}
        h1 {{ font-size: 20px; color: #1a1a1a; border-bottom: 2px solid #333; padding-bottom: 10px; margin-bottom: 20px; }}
        h2 {{ font-size: 16px; color: #0066cc; margin: 24px 0 8px; border-left: 4px solid #0066cc; padding-left: 12px; }}
        .meta {{ color: #666; font-size: 14px; margin-bottom: 16px; }}
        .section {{ margin: 12px 0; font-size: 14px; white-space: pre-wrap; word-break: break-all; }}
        hr {{ border: none; border-top: 1px solid #ddd; margin: 20px 0; }}
        .tag {{ display: inline-block; padding: 2px 8px; background: #e8f0fe; color: #1a73e8; border-radius: 4px; font-size: 12px; margin: 2px; }}
        @media print {{ body {{ padding: 20px; }} }}
    </style></head><body>`);

    doc.write(`<h1>원심: ${{esc(ws.case_number)}}</h1>`);
    doc.write(`<div class="meta">`);
    if (ws.court) doc.write(`${{esc(ws.court)}} `);
    if (ws.date) doc.write(`${{esc(ws.date)}} `);
    if (ws.case_name) doc.write(`<br/><b>${{esc(ws.case_name)}}</b>`);
    doc.write(`<br/><span class="tag">원심판결</span></div>`);

    if (ws.holding) {{
        doc.write(`<h2>판시사항</h2><div class="section">${{esc(ws.holding)}}</div>`);
    }}
    if (ws.summary) {{
        doc.write(`<h2>판결요지</h2><div class="section">${{esc(ws.summary)}}</div>`);
    }}
    if (ws.refs) {{
        doc.write(`<h2>참조조문</h2><div class="section">${{esc(ws.refs)}}</div>`);
    }}
    if (ws.body) {{
        doc.write(`<hr/><h2>판결이유 (전문)</h2><div class="section">${{esc(ws.body)}}</div>`);
    }} else {{
        doc.write(`<hr/><p style="color:#999">원심 판결이유 전문이 DB에 포함되어 있지 않습니다.</p>`);
    }}

    doc.write(`</body></html>`);
    doc.close();
}}
function zoomIn() {{ svg.transition().call(zoom.scaleBy, 1.3); }}
function zoomOut() {{ svg.transition().call(zoom.scaleBy, 0.7); }}
function resetView() {{ svg.transition().call(zoom.transform, d3.zoomIdentity); }}
function expandAll() {{ DATA.clusters.forEach((_, i) => expandedIssues.add(i)); render(); }}
function collapseAll() {{ expandedIssues.clear(); render(); }}

render();
</script>
</body>
</html>"""


def generate_mindmap(data, output):
    html = MINDMAP_HTML.format(
        statute_label=data['statute_label'],
        total_cases=data['total_cases'],
        cluster_count=len(data['clusters']),
        build_time=data['build_time'],
        data_json=json.dumps(data, ensure_ascii=False),
    )
    Path(output).write_text(html, encoding='utf-8')


def generate_mermaid_mindmap(data, output):
    """Generate Mermaid mindmap syntax."""
    lines = ['```mermaid', 'mindmap', f'  root(({data["statute_label"]}))', '']

    for cluster in data['clusters']:
        name = cluster['issue_name']
        count = cluster['case_count']
        lines.append(f'    {name} [{count}건]')
        # Show top 5 cases per cluster
        for c in cluster['cases'][:5]:
            cn = c['case_number']
            court = c.get('court', '')
            date = c.get('date', '')
            lines.append(f'      {cn}')
            if court or date:
                lines.append(f'        {court} {date}')
        if count > 5:
            lines.append(f'      ... +{count - 5}건')

    lines.append('```')

    md = f"# {data['statute_label']} - Issue Mindmap\n\n"
    md += '\n'.join(lines) + '\n\n'

    # Text summary
    md += "## Issue Summary\n\n"
    for i, cluster in enumerate(data['clusters']):
        md += f"### {i+1}. {cluster['issue_name']} ({cluster['case_count']}건)\n\n"
        if cluster.get('representative_topics'):
            for t in cluster['representative_topics']:
                md += f"> {t}\n\n"
        if cluster.get('co_statutes'):
            md += "**Related statutes:** "
            md += ', '.join(cs['label'] for cs in cluster['co_statutes'])
            md += '\n\n'
        md += "**Cases:**\n"
        for c in cluster['cases'][:10]:
            md += f"- {c['case_number']} ({c.get('court','')}, {c.get('date','')})"
            if c.get('case_name'):
                md += f" - {c['case_name']}"
            md += '\n'
        if cluster['case_count'] > 10:
            md += f"- ... +{cluster['case_count'] - 10}건\n"
        md += '\n'

    Path(output).write_text(md, encoding='utf-8')


# ──────────────────────────────────────────
# 3심급 연결 (대법원 ↔ 항소심 ↔ 1심)
# ──────────────────────────────────────────
RE_WONSIM = re.compile(
    r'【원심판결】\s*(.+?)(?:<br|【|\n)',
    re.DOTALL
)
RE_CASE_IN_TEXT = re.compile(r'(\d{2,4}[가-힣]{1,4}\d{1,7})')


def extract_wonsim_case_number(content):
    """Extract 원심판결 case number from judgment text."""
    m = RE_WONSIM.search(content)
    if not m:
        return None
    text = clean(m.group(1))
    nums = RE_CASE_IN_TEXT.findall(text)
    return nums[0] if nums else None


def build_3tier_links(conn, case_slug, content):
    """Build 3-tier court links: 대법원 ↔ 항소심 ↔ 1심.
    Returns list of {'slug': ..., 'relation': 'wonsim'|'1st_instance', 'weight': 10}
    """
    links = []
    wonsim_num = extract_wonsim_case_number(content)
    if wonsim_num:
        wonsim_slug = f"case::{wonsim_num}"
        links.append({
            'slug': wonsim_slug,
            'case_number': wonsim_num,
            'relation': 'wonsim',
            'weight': 10,  # High weight for same-case hierarchy
        })
        # Check if wonsim case exists and has its own wonsim (= 1심)
        row = conn.execute(
            "SELECT content FROM vault_documents WHERE title = ?",
            (wonsim_slug,)
        ).fetchone()
        if row:
            first_num = extract_wonsim_case_number(row[0])
            if first_num:
                links.append({
                    'slug': f"case::{first_num}",
                    'case_number': first_num,
                    'relation': '1st_instance',
                    'weight': 10,
                })
    return links


def enrich_cases_with_3tier(conn, case_data_list):
    """Add 3-tier court links to each case in the list."""
    for case in case_data_list:
        row = conn.execute(
            "SELECT content FROM vault_documents WHERE title = ?",
            (case['slug'],)
        ).fetchone()
        if row:
            case['court_hierarchy'] = build_3tier_links(conn, case['slug'], row[0])
        else:
            case['court_hierarchy'] = []


# ──────────────────────────────────────────
# Theme-based mindmap (테마별)
# ──────────────────────────────────────────
# Pre-defined legal themes with their core statutes and keywords
LEGAL_THEMES = {
    '사해행위취소': {
        'statutes': ['statute::민법::406', 'statute::민법::407', 'statute::민법::404',
                     'statute::민법::405', 'statute::민법::408', 'statute::민법::409'],
        'keywords': ['사해행위', '취소', '채권자취소'],
        'description': '채권자취소권 (사해행위취소소송)',
    },
    '불법행위_손해배상': {
        'statutes': ['statute::민법::750', 'statute::민법::751', 'statute::민법::756',
                     'statute::민법::760', 'statute::민법::763', 'statute::민법::766'],
        'keywords': ['불법행위', '손해배상', '위자료'],
        'description': '불법행위로 인한 손해배상',
    },
    '임대차': {
        'statutes': ['statute::민법::618', 'statute::민법::621', 'statute::민법::623',
                     'statute::민법::625', 'statute::민법::627', 'statute::민법::629',
                     'statute::민법::630', 'statute::민법::631', 'statute::민법::635',
                     'statute::민법::636', 'statute::민법::639', 'statute::민법::640',
                     'statute::민법::643', 'statute::민법::644', 'statute::민법::646',
                     'statute::민법::647', 'statute::민법::652'],
        'keywords': ['임대차', '임대인', '임차인', '보증금'],
        'description': '민법상 임대차',
    },
    '근저당권': {
        'statutes': ['statute::민법::356', 'statute::민법::357', 'statute::민법::358',
                     'statute::민법::359', 'statute::민법::360', 'statute::민법::361',
                     'statute::민법::362', 'statute::민법::363', 'statute::민법::364',
                     'statute::민법::365'],
        'keywords': ['근저당', '저당권', '담보'],
        'description': '저당권/근저당권',
    },
    '매매': {
        'statutes': ['statute::민법::563', 'statute::민법::564', 'statute::민법::568',
                     'statute::민법::569', 'statute::민법::570', 'statute::민법::572',
                     'statute::민법::573', 'statute::민법::574', 'statute::민법::575',
                     'statute::민법::576', 'statute::민법::580', 'statute::민법::581',
                     'statute::민법::582', 'statute::민법::583', 'statute::민법::584',
                     'statute::민법::587', 'statute::민법::588', 'statute::민법::589',
                     'statute::민법::590'],
        'keywords': ['매매', '매도인', '매수인', '하자담보'],
        'description': '매매계약',
    },
    '부당이득': {
        'statutes': ['statute::민법::741', 'statute::민법::742', 'statute::민법::743',
                     'statute::민법::744', 'statute::민법::745', 'statute::민법::746',
                     'statute::민법::747', 'statute::민법::748', 'statute::민법::749'],
        'keywords': ['부당이득', '반환'],
        'description': '부당이득 반환',
    },
    '해고_근로관계': {
        'statutes': ['statute::근로기준법::23', 'statute::근로기준법::24',
                     'statute::근로기준법::26', 'statute::근로기준법::27',
                     'statute::근로기준법::28', 'statute::근로기준법::30',
                     'statute::근로기준법::36'],
        'keywords': ['해고', '부당해고', '근로자', '근로관계'],
        'description': '해고/부당해고/근로관계',
    },
}

# 판례법리 테마 (법조문 없는 관습법 법리)
CASE_LAW_THEMES = {
    '관습법상_법정지상권': {
        'keywords': ['관습법상 법정지상권', '법정지상권', '관습법'],
        'description': '관습법상 법정지상권 (성문법 근거 없이 판례로 형성된 법리)',
    },
    '양도담보': {
        'keywords': ['양도담보', '담보목적', '양도담보권'],
        'description': '양도담보 (판례에 의해 형성된 비전형담보)',
    },
    '명의신탁': {
        'keywords': ['명의신탁', '명의수탁자', '3자간 등기명의신탁'],
        'description': '명의신탁 법리',
    },
    '신의성실_권리남용': {
        'keywords': ['신의성실', '권리남용', '실효의 원칙', '금반언'],
        'description': '신의칙/권리남용/실효의 원칙',
    },
    '표현대리': {
        'keywords': ['표현대리', '무권대리', '외관법리'],
        'description': '표현대리/외관법리',
    },
}


def build_theme_mindmap(conn, theme_name, theme_config, max_cases_per_statute=100):
    """Build a theme-based mindmap: theme -> statutes -> issues -> cases."""
    t0 = time.time()
    statutes = theme_config.get('statutes', [])
    description = theme_config.get('description', theme_name)

    statute_data = []
    total_cases = 0

    for statute_slug in statutes:
        # Check if this statute has any citing cases
        count_row = conn.execute("""
            SELECT COUNT(DISTINCT d.title) FROM vault_links l
            JOIN vault_documents d ON d.id = l.source_doc_id
            WHERE l.target_raw = ? AND l.display_text = 'cites' AND d.doc_type = 'case'
        """, (statute_slug,)).fetchone()
        case_count = count_row[0] if count_row else 0

        if case_count == 0:
            continue

        # Build issue clusters for this statute
        clusters_data = build_issue_clusters(conn, statute_slug, max_cases=max_cases_per_statute)
        if not clusters_data:
            continue

        # Enrich with 3-tier links
        for cluster in clusters_data['clusters']:
            enrich_cases_with_3tier(conn, cluster['cases'][:20])

        statute_label = statute_slug.replace('statute::', '').replace('::', ' 제') + '조'
        statute_data.append({
            'slug': statute_slug,
            'label': statute_label,
            'total_cases': clusters_data['total_cases'],
            'clusters': clusters_data['clusters'],
        })
        total_cases += clusters_data['total_cases']

    return {
        'theme_name': theme_name,
        'description': description,
        'total_statutes': len(statute_data),
        'total_cases': total_cases,
        'statutes': statute_data,
        'build_time': time.time() - t0,
    }


def build_caselaw_theme_mindmap(conn, theme_name, theme_config, max_cases=300):
    """Build mindmap for case-law themes (no statute basis)."""
    t0 = time.time()
    keywords = theme_config.get('keywords', [])
    description = theme_config.get('description', theme_name)

    # Search cases by keywords
    all_cases = []
    seen_slugs = set()
    for kw in keywords:
        rows = conn.execute("""
            SELECT d.title, d.id, d.content FROM vault_docs_fts fts
            JOIN vault_documents d ON d.id = fts.rowid
            WHERE vault_docs_fts MATCH ? AND d.doc_type = 'case'
            LIMIT ?
        """, (kw, max_cases)).fetchall()
        for slug, doc_id, content in rows:
            if slug not in seen_slugs:
                seen_slugs.add(slug)
                all_cases.append((slug, doc_id, content))
            if len(all_cases) >= max_cases:
                break
        if len(all_cases) >= max_cases:
            break

    if not all_cases:
        return None

    # Cluster by 판시사항
    case_data_list = []
    kw_counter = Counter()

    for slug, doc_id, content in all_cases:
        topics = extract_holding_topics(content)
        if not topics:
            m = re.search(r'## 판결요지\n(.+?)(?=\n## )', content, re.DOTALL)
            if m:
                summary = clean(m.group(1))
                sum_topics = re.findall(r'\[(\d+)\]\s*(.+?)(?=\[\d+\]|$)', summary, re.DOTALL)
                topics = [t.strip()[:200] for _, t in sum_topics] if sum_topics else [summary[:200]] if len(summary) > 10 else []

        topic_keywords = []
        for t in topics:
            kws = extract_keywords(t, max_kw=5)
            topic_keywords.append(kws)
            for kw in kws:
                kw_counter[kw] += 1

        fm = dict(conn.execute(
            "SELECT key, value FROM vault_frontmatter WHERE doc_id = ?", (doc_id,)
        ).fetchall())

        co_statutes = []
        for (s,) in conn.execute("""
            SELECT DISTINCT l.target_raw FROM vault_links l
            WHERE l.source_doc_id = ? AND l.display_text = 'cites'
        """, (doc_id,)):
            if s not in BOILERPLATE:
                co_statutes.append(s)

        case_data_list.append({
            'slug': slug,
            'case_number': fm.get('case_number', slug.replace('case::', '')),
            'case_name': fm.get('case_name', ''),
            'court': fm.get('court_name', ''),
            'date': fm.get('verdict_date', ''),
            'topics': topics,
            'topic_keywords': topic_keywords,
            'co_statutes': co_statutes,
        })

    # Cluster by keywords
    cluster_keywords = [kw for kw, cnt in kw_counter.most_common(100)
                        if cnt >= 2 and cnt < len(case_data_list) * 0.6]
    clusters = defaultdict(list)
    clustered = set()

    for idx, case in enumerate(case_data_list):
        all_kws = set()
        for kws in case['topic_keywords']:
            all_kws.update(kws)
        for ckw in cluster_keywords:
            if ckw in all_kws:
                clusters[ckw].append(case)
                clustered.add(idx)
                break

    unclustered = [c for i, c in enumerate(case_data_list) if i not in clustered]
    if unclustered:
        clusters['기타'] = unclustered

    # Enrich with 3-tier
    for key, case_list in clusters.items():
        enrich_cases_with_3tier(conn, case_list[:20])

    result_clusters = []
    for name, case_list in sorted(clusters.items(), key=lambda x: -len(x[1])):
        if len(case_list) < 2 and name != '기타':
            clusters.setdefault('기타', []).extend(case_list)
            continue
        rep_topics = []
        for c in case_list[:3]:
            for t in c['topics'][:1]:
                rep_topics.append(t[:300])
        result_clusters.append({
            'issue_name': name,
            'case_count': len(case_list),
            'representative_topics': rep_topics,
            'co_statutes': [],
            'cases': sorted(case_list, key=lambda c: c.get('date', '') or '', reverse=True),
        })

    return {
        'theme_name': theme_name,
        'description': description,
        'statute_label': theme_name,  # For template compatibility
        'total_cases': len(case_data_list),
        'clusters': result_clusters,
        'top_keywords': [(kw, cnt) for kw, cnt in kw_counter.most_common(20)],
        'top_co_statutes': [],
        'build_time': time.time() - t0,
        'is_caselaw_theme': True,
    }


# ──────────────────────────────────────────
# Theme mindmap HTML generator
# ──────────────────────────────────────────
def generate_theme_mindmap(data, output):
    """Generate HTML for theme-based mindmap (theme -> statutes -> issues -> cases)."""
    # Reuse single-statute mindmap for case-law themes
    if data.get('is_caselaw_theme'):
        generate_mindmap(data, output)
        return

    # For statute-based themes: build hierarchical JSON
    html = MINDMAP_HTML.format(
        statute_label=data['theme_name'],
        total_cases=data['total_cases'],
        cluster_count=sum(len(s['clusters']) for s in data['statutes']),
        build_time=data['build_time'],
        # Flatten: theme statutes become the clusters, each with sub-clusters
        data_json=json.dumps({
            'statute_label': data['theme_name'],
            'description': data['description'],
            'total_cases': data['total_cases'],
            'clusters': [
                {
                    'issue_name': f"{s['label']} ({s['total_cases']}건)",
                    'case_count': s['total_cases'],
                    'representative_topics': [
                        c['issue_name'] + f" ({c['case_count']}건)"
                        for c in s['clusters'][:5]
                    ],
                    'co_statutes': [],
                    'cases': [
                        case
                        for cluster in s['clusters']
                        for case in cluster['cases'][:10]
                    ][:30],
                    'sub_clusters': s['clusters'],
                }
                for s in data['statutes']
            ],
            'build_time': data['build_time'],
        }, ensure_ascii=False),
    )
    Path(output).write_text(html, encoding='utf-8')


def generate_theme_mermaid(data, output):
    """Generate Mermaid for theme mindmap."""
    lines = ['```mermaid', 'mindmap', f'  root(({data["theme_name"]}))', '']

    if data.get('is_caselaw_theme'):
        # Case-law theme: clusters directly under root
        for cluster in data.get('clusters', []):
            lines.append(f'    {cluster["issue_name"]} [{cluster["case_count"]}건]')
            for c in cluster['cases'][:5]:
                lines.append(f'      {c["case_number"]}')
    else:
        # Statute-based theme
        for s in data['statutes']:
            lines.append(f'    {s["label"]} [{s["total_cases"]}건]')
            for cluster in s['clusters'][:5]:
                lines.append(f'      {cluster["issue_name"]} [{cluster["case_count"]}건]')
                for c in cluster['cases'][:3]:
                    lines.append(f'        {c["case_number"]}')

    lines.append('```')

    md = f"# {data.get('theme_name', '')} - Theme Mindmap\n\n"
    md += f"*{data.get('description', '')}*\n\n"
    md += '\n'.join(lines) + '\n'
    Path(output).write_text(md, encoding='utf-8')


# ──────────────────────────────────────────
# Main
# ──────────────────────────────────────────
def main():
    parser = argparse.ArgumentParser(description='Legal Issue Mindmap Generator')
    parser.add_argument('mode', choices=['statute', 'theme', 'caselaw', 'list-themes'],
                        help='statute: 법조문별, theme: 테마별, caselaw: 판례법리, list-themes: 테마목록')
    parser.add_argument('query', nargs='?', default=None,
                        help='Statute slug/name, theme name, or caselaw theme name')
    parser.add_argument('--output', '-o', default=None)
    parser.add_argument('--max-cases', type=int, default=500)
    args = parser.parse_args()

    out_dir = Path(args.output).parent if args.output else Path("C:/Users/kjccj/세컨드브레인_로컬DB/viz")
    out_dir.mkdir(parents=True, exist_ok=True)

    if args.mode == 'list-themes':
        print("=== Statute-based Themes ===")
        for name, cfg in LEGAL_THEMES.items():
            print(f"  {name}: {cfg['description']} ({len(cfg['statutes'])} statutes)")
        print("\n=== Case-law Themes (no statute basis) ===")
        for name, cfg in CASE_LAW_THEMES.items():
            print(f"  {name}: {cfg['description']}")
        return

    conn = get_conn()

    if args.mode == 'statute':
        # 법조문별 마인드맵 (기존 기능)
        statute_slug = resolve_statute_slug(conn, args.query)
        print(f"Statute: {statute_slug}")
        print(f"Building issue clusters (max {args.max_cases} cases)...")
        data = build_issue_clusters(conn, statute_slug, max_cases=args.max_cases)
        if not data:
            print("No cases found.")
            return

        # Enrich with 3-tier links
        for cluster in data['clusters']:
            enrich_cases_with_3tier(conn, cluster['cases'][:20])

        print(f"  {data['total_cases']} cases -> {len(data['clusters'])} groups ({data['build_time']:.1f}s)")
        base = Path(args.output).stem if args.output else statute_slug.replace('::', '_')
        generate_mindmap(data, str(out_dir / f"{base}_mindmap.html"))
        generate_mermaid_mindmap(data, str(out_dir / f"{base}_mindmap.md"))
        print(f"  Output: {out_dir / base}_mindmap.html/.md")

    elif args.mode == 'theme':
        # 테마별 마인드맵
        theme_name = args.query
        if theme_name not in LEGAL_THEMES:
            print(f"Unknown theme: {theme_name}")
            print(f"Available: {', '.join(LEGAL_THEMES.keys())}")
            return
        theme_config = LEGAL_THEMES[theme_name]
        print(f"Theme: {theme_name} ({theme_config['description']})")
        print(f"Statutes: {len(theme_config['statutes'])}")
        data = build_theme_mindmap(conn, theme_name, theme_config,
                                   max_cases_per_statute=args.max_cases)
        print(f"  {data['total_statutes']} statutes, {data['total_cases']} cases ({data['build_time']:.1f}s)")
        for s in data['statutes']:
            print(f"    {s['label']}: {s['total_cases']} cases, {len(s['clusters'])} groups")

        base = args.output and Path(args.output).stem or theme_name
        generate_theme_mindmap(data, str(out_dir / f"{base}_theme_mindmap.html"))
        generate_theme_mermaid(data, str(out_dir / f"{base}_theme_mermaid.md"))
        print(f"  Output: {out_dir / base}_theme_mindmap.html/.md")

    elif args.mode == 'caselaw':
        # 판례법리 테마 (법조문 없는 관습법)
        theme_name = args.query
        if theme_name not in CASE_LAW_THEMES:
            print(f"Unknown case-law theme: {theme_name}")
            print(f"Available: {', '.join(CASE_LAW_THEMES.keys())}")
            return
        theme_config = CASE_LAW_THEMES[theme_name]
        print(f"Case-law theme: {theme_name} ({theme_config['description']})")
        data = build_caselaw_theme_mindmap(conn, theme_name, theme_config,
                                           max_cases=args.max_cases)
        if not data:
            print("No cases found.")
            return
        print(f"  {data['total_cases']} cases -> {len(data['clusters'])} groups ({data['build_time']:.1f}s)")

        base = args.output and Path(args.output).stem or theme_name
        generate_mindmap(data, str(out_dir / f"{base}_caselaw_mindmap.html"))
        generate_mermaid_mindmap(data, str(out_dir / f"{base}_caselaw_mindmap.md"))
        print(f"  Output: {out_dir / base}_caselaw_mindmap.html/.md")

    conn.close()
    print("Done.")


if __name__ == '__main__':
    main()
