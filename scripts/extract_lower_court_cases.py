#!/usr/bin/env python3
"""
extract_lower_court_cases.py - 대법원 판례에서 하급심(원심/1심) 판례 정보 추출

대법원 판례의 【원심판결】 섹션에서 하급심 법원명, 선고일, 사건번호를 추출하여:
  1. JSON 파일 (AI가 바로 인식 가능한 형태)
  2. CSV/Excel 파일 (사람이 쉽게 인식 가능한 형태)
  3. domain.db에 없는 하급심만 필터링

출력 구조:
  - lower_court_cases.json: [{supreme_case, court, date, case_number, raw_text, exists_in_db}, ...]
  - lower_court_cases_missing.csv: DB에 없는 하급심만 (bigcase.ai 크롤링 대상)
  - lower_court_cases_all.csv: 전체 하급심 목록
"""

import csv
import json
import re
import sqlite3
import time
from pathlib import Path

DB_PATH = Path(r"C:\Users\kjccj\세컨드브레인_로컬DB\domain.db")
OUTPUT_DIR = Path(r"C:\Users\kjccj\세컨드브레인_로컬DB\하급심추출")

# ──────────────────────────────────────────
# Parsing patterns
# ──────────────────────────────────────────

# 【원심판결】 서울고법 2024. 12. 20. 선고 (인천)2023나15545, 15552 판결
# 【원심판결】 서울남부지법 2022. 5. 19. 선고 2021나66669 판결
RE_WONSIM = re.compile(r'【원심판결】\s*(.+?)(?:<br|【|\n)', re.DOTALL)

# 법원명 패턴: 다양한 법원 형태 지원
# 서울고법, 서울고등법원, 서울중앙지법, 부산지법, 대전고법, 특허법원 등
RE_COURT_DATE_CASE = re.compile(
    r'([가-힣]+(?:고등법원|고법|지방법원|지법|지원|특허법원|가정법원|행정법원|회생법원))'
    r'\s+(\d{4})\.\s*(\d{1,2})\.\s*(\d{1,2})\.\s*선고\s+'
    r'((?:\([가-힣]+\))?\s*\d{2,4}[가-힣]{1,5}\d{1,7}(?:\s*,\s*\d{1,7})*)'
    r'\s*(판결|결정)?'
)

# 판결본문 내 1심 참조 패턴 (원심판결문 안에 기재된 1심)
# 예: "서울중앙지법 2023. 1. 15. 선고 2022가합12345 판결"
RE_FIRST_INSTANCE = re.compile(
    r'([가-힣]+(?:고등법원|고법|지방법원|지법|지원|특허법원|가정법원|행정법원|회생법원))'
    r'\s+(\d{4})\.\s*(\d{1,2})\.\s*(\d{1,2})\.\s*선고\s+'
    r'((?:\([가-힣]+\))?\s*\d{2,4}[가-힣]{1,5}\d{1,7}(?:\s*,\s*\d{1,7})*)'
    r'\s*(판결|결정)?'
)

RE_HTML = re.compile(r'<[^>]+>')


def clean(text):
    return RE_HTML.sub('', text).strip()


def parse_wonsim_info(raw_text):
    """Parse 원심판결 text into structured data."""
    text = clean(raw_text)
    m = RE_COURT_DATE_CASE.search(text)
    if not m:
        return None

    court = m.group(1).strip()
    year = m.group(2)
    month = m.group(3)
    day = m.group(4)
    case_numbers_raw = m.group(5).strip()
    verdict_type = m.group(6) or '판결'

    date_str = f"{year}{int(month):02d}{int(day):02d}"
    date_display = f"{year}. {int(month)}. {int(day)}."

    # 사건번호 분리 (복수 사건번호 처리: 2023나15545, 15552)
    case_numbers = []
    parts = re.split(r'\s*,\s*', case_numbers_raw)
    base_num = parts[0].strip()
    case_numbers.append(base_num)

    # 추가 번호가 있으면 기본 번호의 접두사를 붙여 완성
    if len(parts) > 1:
        prefix_m = re.match(r'((?:\([가-힣]+\))?\s*\d{2,4}[가-힣]{1,5})', base_num)
        if prefix_m:
            prefix = prefix_m.group(1)
            for p in parts[1:]:
                p = p.strip()
                if re.match(r'^\d+$', p):
                    case_numbers.append(f"{prefix}{p}")
                else:
                    case_numbers.append(p)

    return {
        'court': court,
        'date': date_str,
        'date_display': date_display,
        'case_numbers': case_numbers,
        'case_number_primary': case_numbers[0],
        'verdict_type': verdict_type,
        'raw_text': text,
    }


def extract_1st_instance_from_body(content):
    """Extract 1심 판결 info from case body text.
    대법원 판례의 본문에서 '【제1심판결】' 또는 원심판결문 내 1심 참조를 찾는다.
    """
    results = []

    # 【제1심판결】 패턴
    m = re.search(r'【제1심판결】\s*(.+?)(?:<br|【|\n)', content, re.DOTALL)
    if m:
        info = parse_wonsim_info(m.group(1))
        if info:
            info['tier'] = '1심'
            results.append(info)
            return results

    # 본문에서 "제1심" 언급 근처의 법원+선고일+사건번호
    m2 = re.search(r'제\s*1\s*심[^【]*?([가-힣]+(?:지법|지방법원|지원))\s+(\d{4})\.\s*(\d{1,2})\.\s*(\d{1,2})\.\s*선고\s+((?:\([가-힣]+\))?\s*\d{2,4}[가-힣]{1,5}\d{1,7})', content)
    if m2:
        court = m2.group(1)
        date_str = f"{m2.group(2)}{int(m2.group(3)):02d}{int(m2.group(4)):02d}"
        case_num = m2.group(5).strip()
        results.append({
            'court': court,
            'date': date_str,
            'date_display': f"{m2.group(2)}. {int(m2.group(3))}. {int(m2.group(4))}.",
            'case_numbers': [case_num],
            'case_number_primary': case_num,
            'verdict_type': '판결',
            'raw_text': clean(m2.group(0)),
            'tier': '1심',
        })

    return results


def main():
    t0 = time.time()
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)

    conn = sqlite3.connect(f'file:{DB_PATH}?mode=ro', uri=True)
    conn.execute('PRAGMA cache_size = -128000')

    print(f"{'='*60}")
    print(f"  Lower Court Case Extractor")
    print(f"  DB: {DB_PATH}")
    print(f"  Output: {OUTPUT_DIR}")
    print(f"{'='*60}")
    print()

    # Get all 대법원 cases
    print("[1/4] Loading supreme court cases...")
    supreme_cases = conn.execute("""
        SELECT d.title, d.content FROM vault_documents d
        JOIN vault_frontmatter fm ON fm.doc_id = d.id AND fm.key = 'court_name'
        WHERE d.doc_type = 'case' AND fm.value LIKE '%대법원%'
    """).fetchall()
    print(f"  Supreme court cases: {len(supreme_cases):,}")

    # Extract lower court info
    print("\n[2/4] Extracting lower court references...")
    all_lower = []
    no_wonsim = 0
    parse_fail = 0

    for supreme_slug, content in supreme_cases:
        # 원심(2심) 추출
        m = RE_WONSIM.search(content)
        if not m:
            no_wonsim += 1
            continue

        info = parse_wonsim_info(m.group(1))
        if not info:
            parse_fail += 1
            continue

        info['supreme_case'] = supreme_slug
        info['tier'] = '원심(2심)'

        # DB 존재 여부 확인
        for cn in info['case_numbers']:
            slug = f"case::{cn}"
            exists = conn.execute(
                "SELECT 1 FROM vault_documents WHERE title = ? LIMIT 1", (slug,)
            ).fetchone()
            all_lower.append({
                'supreme_case': supreme_slug.replace('case::', ''),
                'tier': info['tier'],
                'court': info['court'],
                'date': info['date'],
                'date_display': info['date_display'],
                'case_number': cn,
                'verdict_type': info['verdict_type'],
                'raw_text': info['raw_text'],
                'case_slug': slug,
                'exists_in_db': bool(exists),
            })

        # 1심 추출 (본문에서)
        first_instances = extract_1st_instance_from_body(content)
        for fi in first_instances:
            for cn in fi['case_numbers']:
                slug = f"case::{cn}"
                exists = conn.execute(
                    "SELECT 1 FROM vault_documents WHERE title = ? LIMIT 1", (slug,)
                ).fetchone()
                all_lower.append({
                    'supreme_case': supreme_slug.replace('case::', ''),
                    'tier': '1심',
                    'court': fi['court'],
                    'date': fi['date'],
                    'date_display': fi['date_display'],
                    'case_number': cn,
                    'verdict_type': fi.get('verdict_type', '판결'),
                    'raw_text': fi.get('raw_text', ''),
                    'case_slug': slug,
                    'exists_in_db': bool(exists),
                })

    # 헌재결정례에서도 추출
    constitutional = conn.execute("""
        SELECT d.title, d.content FROM vault_documents d
        JOIN vault_frontmatter fm ON fm.doc_id = d.id AND fm.key = 'court_name'
        WHERE d.doc_type = 'case' AND fm.value = '헌법재판소'
    """).fetchall()
    for slug, content in constitutional:
        m = RE_WONSIM.search(content)
        if m:
            info = parse_wonsim_info(m.group(1))
            if info:
                for cn in info['case_numbers']:
                    cs = f"case::{cn}"
                    exists = conn.execute(
                        "SELECT 1 FROM vault_documents WHERE title = ? LIMIT 1", (cs,)
                    ).fetchone()
                    all_lower.append({
                        'supreme_case': slug.replace('case::', ''),
                        'tier': '원심',
                        'court': info['court'],
                        'date': info['date'],
                        'date_display': info['date_display'],
                        'case_number': cn,
                        'verdict_type': info['verdict_type'],
                        'raw_text': info['raw_text'],
                        'case_slug': cs,
                        'exists_in_db': bool(exists),
                    })

    print(f"  Total lower court references: {len(all_lower):,}")
    print(f"  No 원심판결 found: {no_wonsim:,}")
    print(f"  Parse failures: {parse_fail:,}")

    exists_count = sum(1 for r in all_lower if r['exists_in_db'])
    missing_count = sum(1 for r in all_lower if not r['exists_in_db'])
    print(f"  Already in DB: {exists_count:,}")
    print(f"  Missing (crawl target): {missing_count:,}")

    # Deduplicate by case_number
    seen = set()
    deduped = []
    for r in all_lower:
        key = r['case_number']
        if key not in seen:
            seen.add(key)
            deduped.append(r)

    deduped_missing = [r for r in deduped if not r['exists_in_db']]
    print(f"\n  After dedup: {len(deduped):,} unique lower court cases")
    print(f"  Missing (unique): {len(deduped_missing):,}")

    # ── Output ──
    print(f"\n[3/4] Writing output files...")

    # 1. JSON (AI-readable)
    json_path = OUTPUT_DIR / 'lower_court_cases_all.json'
    with open(json_path, 'w', encoding='utf-8') as f:
        json.dump(deduped, f, ensure_ascii=False, indent=2)
    print(f"  {json_path.name}: {len(deduped):,} cases")

    json_missing_path = OUTPUT_DIR / 'lower_court_cases_missing.json'
    with open(json_missing_path, 'w', encoding='utf-8') as f:
        json.dump(deduped_missing, f, ensure_ascii=False, indent=2)
    print(f"  {json_missing_path.name}: {len(deduped_missing):,} cases")

    # 2. CSV/Excel (Human-readable)
    csv_all_path = OUTPUT_DIR / 'lower_court_cases_all.csv'
    with open(csv_all_path, 'w', encoding='utf-8-sig', newline='') as f:
        writer = csv.writer(f)
        writer.writerow([
            '대법원사건번호', '심급', '하급심법원명', '선고일(YYYYMMDD)',
            '선고일(표시)', '하급심사건번호', '판결유형', 'DB존재여부',
            '원문텍스트'
        ])
        for r in deduped:
            writer.writerow([
                r['supreme_case'], r['tier'], r['court'], r['date'],
                r['date_display'], r['case_number'], r['verdict_type'],
                'O' if r['exists_in_db'] else 'X',
                r['raw_text'][:200],
            ])
    print(f"  {csv_all_path.name}: {len(deduped):,} rows")

    csv_missing_path = OUTPUT_DIR / 'lower_court_cases_missing.csv'
    with open(csv_missing_path, 'w', encoding='utf-8-sig', newline='') as f:
        writer = csv.writer(f)
        writer.writerow([
            '대법원사건번호', '심급', '하급심법원명', '선고일(YYYYMMDD)',
            '선고일(표시)', '하급심사건번호', '판결유형',
            'bigcase_search_query'
        ])
        for r in deduped_missing:
            # bigcase.ai 검색 쿼리 형식
            search_query = f"{r['case_number']}"
            writer.writerow([
                r['supreme_case'], r['tier'], r['court'], r['date'],
                r['date_display'], r['case_number'], r['verdict_type'],
                search_query,
            ])
    print(f"  {csv_missing_path.name}: {len(deduped_missing):,} rows (crawl targets)")

    # 3. Summary by court
    print(f"\n[4/4] Summary by court:")
    from collections import Counter
    court_counter = Counter()
    for r in deduped_missing:
        court_counter[r['court']] += 1
    for court, cnt in court_counter.most_common(20):
        print(f"  {court}: {cnt:,}")

    tier_counter = Counter(r['tier'] for r in deduped_missing)
    print(f"\n  By tier:")
    for tier, cnt in tier_counter.most_common():
        print(f"    {tier}: {cnt:,}")

    conn.close()
    elapsed = time.time() - t0
    print(f"\n{'='*60}")
    print(f"  Done in {elapsed:.1f}s")
    print(f"  Output: {OUTPUT_DIR}")
    print(f"{'='*60}")


if __name__ == '__main__':
    main()
