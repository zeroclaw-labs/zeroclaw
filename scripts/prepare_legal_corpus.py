#!/usr/bin/env python3
"""
prepare_legal_corpus.py — 세컨드브레인_로컬DB → zeroclaw vault domain build 호환 corpus

이 스크립트는 법령정보센터에서 수집한 마크다운 파일들을 zeroclaw Rust 파서가
기대하는 포맷으로 변환합니다.

법령 4종 (현행법령, 연혁법령, 자치법규, 행정규칙):
  Rust statute_extractor.rs가 기대하는 포맷:
    # {법령명}
    - **MST(법령 마스터 번호)**: ...
    ```json
    { "meta": {...}, "articles": [...], "supplements": [...] }
    ```

판례 2종 (판례, 헌재결정례):
  Rust case_extractor.rs가 기대하는 포맷:
    ## 사건번호   ## 선고일자   ## 법원명   ## 사건명
    ## 판시사항   ## 판결요지   ## 참조조문  ## 판례내용 ...

  헌재결정례는 필드명이 다르므로 매핑:
    결정요지 → 판결요지,  전문 → 판례내용,  종국일자 → 선고일자

Usage:
  python scripts/prepare_legal_corpus.py [--source DIR] [--output DIR] [--category CATEGORY]

Example:
  python scripts/prepare_legal_corpus.py \\
      --source "C:\\Users\\kjccj\\세컨드브레인_로컬DB" \\
      --output "C:\\Users\\kjccj\\세컨드브레인_로컬DB\\_corpus"
"""

import argparse
import json
import os
import re
import shutil
import sys
import time
from pathlib import Path

# ── Categories ──
CASE_DIRS = {"판례", "헌재결정례"}
STATUTE_DIRS = {"현행법령", "연혁법령", "자치법규", "행정규칙"}


def extract_sections(content: str) -> dict:
    """Parse ## headers → dict."""
    sections = {}
    current_key = None
    current_lines = []

    for line in content.split('\n'):
        m = re.match(r'^##\s+(.+)$', line)
        if m:
            if current_key is not None:
                sections[current_key] = '\n'.join(current_lines).strip()
            current_key = m.group(1).strip()
            current_lines = []
        else:
            current_lines.append(line)

    if current_key is not None:
        sections[current_key] = '\n'.join(current_lines).strip()

    return sections


# ════════════════════════════════════════
# 판례 변환 (이미 호환 포맷, 그대로 복사)
# ════════════════════════════════════════
def write_lf(dst: Path, text: str):
    """Write text with Unix LF line endings (Rust find_json_block requires \\n)."""
    dst.parent.mkdir(parents=True, exist_ok=True)
    # Normalize CRLF → LF
    text = text.replace('\r\n', '\n')
    dst.write_bytes(text.encode('utf-8'))


def convert_case_file(src: Path, dst: Path):
    """판례 파일을 LF로 변환하여 복사."""
    content = src.read_text(encoding='utf-8', errors='replace')
    write_lf(dst, content)


# ════════════════════════════════════════
# 헌재결정례 변환 (필드명 매핑)
# ════════════════════════════════════════
def convert_constitutional_case(src: Path, dst: Path):
    """헌재결정례 필드를 판례 호환 포맷으로 매핑."""
    content = src.read_text(encoding='utf-8', errors='replace')
    sections = extract_sections(content)

    # 필드 매핑
    field_map = {
        '결정요지': '판결요지',
        '전문': '판례내용',
        '종국일자': '선고일자',
        '헌재결정례일련번호': '판례정보일련번호',
        '재판부구분코드': '법원종류코드',
    }

    # 법원명이 없으면 '헌법재판소' 추가
    if '법원명' not in sections:
        sections['법원명'] = '헌법재판소'

    # 판결유형이 없으면 '결정' 추가
    if '판결유형' not in sections and '선고' not in sections:
        sections['선고'] = '결정'
        sections['판결유형'] = '결정'

    # 사건종류코드가 없으면 헌재 기본값
    if '사건종류코드' not in sections:
        code = sections.get('사건종류코드', '')
        if not code:
            sections['사건종류코드'] = '430101'

    # 매핑 적용
    for old_key, new_key in field_map.items():
        if old_key in sections and new_key not in sections:
            sections[new_key] = sections[old_key]

    # 재구성
    output_lines = []
    # 필수 필드 순서
    ordered_keys = [
        '판시사항', '참조판례', '사건종류명', '판결요지',
        '참조조문', '선고일자', '법원명', '사건명', '판례내용',
        '사건번호', '사건종류코드', '판례정보일련번호',
        '선고', '판결유형', '법원종류코드',
    ]
    # 추가 키 (심판대상조문 등)
    extra_keys = [k for k in sections if k not in ordered_keys
                  and k not in field_map]

    for key in ordered_keys + extra_keys:
        if key in sections:
            val = sections[key]
            output_lines.append(f"## {key}")
            output_lines.append(val)
            output_lines.append("")

    write_lf(dst, '\n'.join(output_lines))


# ════════════════════════════════════════
# 현행법령 변환
# ════════════════════════════════════════
def convert_national_statute(src: Path, dst: Path, category: str):
    """현행법령/연혁법령 → Rust ```json 블록 포맷."""
    content = src.read_text(encoding='utf-8', errors='replace')

    # 연혁법령은 이미 ```json 블록이 있을 수 있음
    if category == '연혁법령' and '```json' in content:
        # 이미 호환 포맷 — 그대로 복사하되 MST 마커 추가
        if '법령 마스터 번호' not in content:
            # 상세 JSON에서 law name 추출
            title_m = re.match(r'^#\s+(.+)$', content, re.MULTILINE)
            law_name = title_m.group(1).strip() if title_m else src.stem.rsplit('_', 1)[0]
            content = f"# {law_name}\n\n- **MST(법령 마스터 번호)**: {src.stem.rsplit('_', 1)[-1]}\n\n{content}"
        write_lf(dst, content)
        return

    sections = extract_sections(content)

    # ── 메타데이터 추출 ──
    law_name = ''
    ls_id = ''
    anc_yd = ''
    ef_yd = ''
    lsi_seq = ''
    anc_no = ''

    basic_raw = sections.get('기본정보', '')
    if basic_raw:
        try:
            basic = json.loads(basic_raw)
            law_name = basic.get('법령명_한글', '') or basic.get('법령명', '')
            ls_id = basic.get('법령ID', '')
            ef_yd = basic.get('시행일자', '')
            anc_yd = basic.get('공포일자', '')
            anc_no = basic.get('공포번호', '')
        except (json.JSONDecodeError, TypeError):
            pass

    if not law_name:
        law_name = src.stem.rsplit('_', 1)[0]

    law_key_raw = sections.get('법령키', '').strip()
    if law_key_raw and not lsi_seq:
        lsi_seq = law_key_raw

    # ── 조문 파싱 ──
    articles = []
    articles_raw = sections.get('조문', '')
    if articles_raw:
        try:
            articles_data = json.loads(articles_raw)
            article_list = articles_data.get('조문단위', articles_data.get('조', []))
            if isinstance(article_list, list):
                for art in article_list:
                    if not isinstance(art, dict):
                        continue

                    art_num = art.get('조문번호', '')
                    if isinstance(art_num, list):
                        art_num = art_num[0] if art_num else ''
                    art_num = str(art_num).strip()

                    art_content = art.get('조문내용', '') or art.get('조내용', '')
                    art_title = art.get('조제목', '')

                    # number 필드 구성: "제N조(제목)" 형태
                    # art_content에서 추출하거나 조합
                    number_str = ''
                    if art_content:
                        # 조문내용 앞부분에서 "제N조(제목)" 추출
                        nm = re.match(r'(제\d+조(?:의\d+)?(?:\s*[\(（][^)）]*[\)）])?)', art_content)
                        if nm:
                            number_str = nm.group(1)
                    if not number_str and art_num:
                        if art_title:
                            number_str = f"제{art_num}조({art_title})"
                        else:
                            number_str = f"제{art_num}조"

                    # text: 조문 전체 내용
                    text = str(art_content).strip()

                    # 항/호 내용 병합
                    for hang_key in ['항', '호']:
                        sub_items = art.get(hang_key, None)
                        if sub_items and isinstance(sub_items, dict):
                            # Sometimes 항 contains 호
                            ho_list = sub_items.get('호', [])
                            if isinstance(ho_list, list):
                                for ho in ho_list:
                                    if isinstance(ho, dict):
                                        ho_content = ho.get('호내용', '')
                                        if isinstance(ho_content, list):
                                            for c in ho_content:
                                                if isinstance(c, list):
                                                    text += '\n' + ' '.join(str(x) for x in c)
                                                else:
                                                    text += '\n' + str(c)
                                        elif ho_content:
                                            text += '\n' + str(ho_content)

                    if number_str and text:
                        articles.append({
                            "number": number_str,
                            "text": text,
                            "anchor": f"J{art_num}:0"
                        })
        except (json.JSONDecodeError, TypeError) as e:
            pass  # skip unparseable articles

    # ── 부칙 파싱 ──
    supplements = []
    sup_raw = sections.get('부칙', '')
    if sup_raw:
        try:
            sup_data = json.loads(sup_raw)
            # 다양한 구조 처리
            sup_list = sup_data if isinstance(sup_data, list) else [sup_data]
            for sup_item in sup_list:
                if not isinstance(sup_item, dict):
                    continue
                # 부칙단위가 있는 경우
                if '부칙단위' in sup_item:
                    inner = sup_item['부칙단위']
                    if isinstance(inner, dict):
                        inner = [inner]
                    for s in inner:
                        if isinstance(s, dict):
                            sup_date = s.get('부칙공포일자', '')
                            sup_no = s.get('부칙공포번호', '')
                            sup_content_raw = s.get('부칙내용', '')
                            sup_text = ''
                            if isinstance(sup_content_raw, list):
                                for item in sup_content_raw:
                                    if isinstance(item, list):
                                        sup_text += '\n'.join(str(x) for x in item) + '\n'
                                    else:
                                        sup_text += str(item) + '\n'
                            else:
                                sup_text = str(sup_content_raw)
                            title = f"부칙 <제{sup_no}호, {sup_date[:4]}.{sup_date[4:6]}.{sup_date[6:8]}.>" if sup_date and len(sup_date) >= 8 and sup_no else "부칙"
                            supplements.append({
                                "title": title,
                                "body": sup_text.strip()
                            })
                else:
                    # 직접 부칙 데이터
                    sup_date = sup_item.get('부칙공포일자', '')
                    sup_no = sup_item.get('부칙공포번호', '')
                    sup_content_raw = sup_item.get('부칙내용', '')
                    sup_text = str(sup_content_raw) if sup_content_raw else ''
                    title = f"부칙 <제{sup_no}호, {sup_date[:4]}.{sup_date[4:6]}.{sup_date[6:8]}.>" if sup_date and len(sup_date) >= 8 and sup_no else "부칙"
                    supplements.append({
                        "title": title,
                        "body": sup_text.strip()
                    })
        except (json.JSONDecodeError, TypeError):
            pass

    # ── 출력 구성 ──
    meta = {
        "lsNm": law_name,
        "ancYd": anc_yd,
        "efYd": ef_yd,
        "lsId": ls_id,
        "lsiSeq": lsi_seq,
        "ancNo": anc_no
    }

    json_block = {
        "meta": meta,
        "title": law_name,
        "articles": articles,
        "supplements": supplements
    }

    mst = lsi_seq or ls_id or src.stem.rsplit('_', 1)[-1]

    output = f"""# {law_name}

- **MST(법령 마스터 번호)**: {mst}
- **공포일자**: {anc_yd}
- **시행일**: {ef_yd}

```json
{json.dumps(json_block, ensure_ascii=False, indent=2)}
```
"""

    write_lf(dst, output)


# ════════════════════════════════════════
# 자치법규 변환
# ════════════════════════════════════════
def convert_local_statute(src: Path, dst: Path):
    """자치법규 → Rust ```json 블록 포맷."""
    content = src.read_text(encoding='utf-8', errors='replace')
    sections = extract_sections(content)

    law_name = ''
    basic = {}
    basic_raw = sections.get('자치법규기본정보', '')
    if basic_raw:
        try:
            basic = json.loads(basic_raw)
            law_name = basic.get('자치법규명', '')
        except (json.JSONDecodeError, TypeError):
            pass
    if not law_name:
        law_name = src.stem.rsplit('_', 1)[0]

    ef_yd = basic.get('시행일자', '')
    anc_yd = basic.get('공포일자', '')
    anc_no = basic.get('공포번호', '')
    law_id = basic.get('자치법규ID', '')

    # 조문 파싱
    articles = []
    articles_raw = sections.get('조문', '')
    if articles_raw:
        try:
            articles_data = json.loads(articles_raw)
            article_list = articles_data.get('조', articles_data.get('조문단위', []))
            if isinstance(article_list, list):
                for art in article_list:
                    if not isinstance(art, dict):
                        continue
                    art_title = art.get('조제목', '')
                    art_content = art.get('조내용', '') or art.get('조문내용', '')
                    art_num_raw = art.get('조문번호', '')
                    if isinstance(art_num_raw, list):
                        art_num_raw = art_num_raw[0] if art_num_raw else ''

                    # number 구성
                    number_str = ''
                    if art_content:
                        nm = re.match(r'(제\d+조(?:의\d+)?(?:\s*[\(（][^)）]*[\)）])?)', art_content)
                        if nm:
                            number_str = nm.group(1)
                    if not number_str:
                        # 조문번호에서 추출
                        nm2 = re.search(r'(\d+)', str(art_num_raw))
                        num = nm2.group(1) if nm2 else str(art_num_raw)
                        if art_title:
                            number_str = f"제{num}조({art_title})"
                        else:
                            number_str = f"제{num}조"

                    text = str(art_content).strip()
                    if number_str and text:
                        articles.append({
                            "number": number_str,
                            "text": text,
                            "anchor": f"J{art_num_raw}:0"
                        })
        except (json.JSONDecodeError, TypeError):
            pass

    # 부칙 파싱
    supplements = []
    sup_raw = sections.get('부칙', '')
    if sup_raw:
        try:
            sup_data = json.loads(sup_raw)
            if isinstance(sup_data, dict):
                sup_date = sup_data.get('부칙공포일자', '')
                sup_no = sup_data.get('부칙공포번호', '')
                sup_text = sup_data.get('부칙내용', '')
                title = f"부칙 <제{sup_no}호, {sup_date[:4]}.{sup_date[4:6]}.{sup_date[6:8]}.>" if sup_date and len(sup_date) >= 8 and sup_no else "부칙"
                supplements.append({
                    "title": title,
                    "body": str(sup_text).strip()
                })
        except (json.JSONDecodeError, TypeError):
            pass

    meta = {
        "lsNm": law_name,
        "ancYd": anc_yd,
        "efYd": ef_yd,
        "lsId": law_id,
        "lsiSeq": "",
        "ancNo": anc_no
    }

    json_block = {
        "meta": meta,
        "title": law_name,
        "articles": articles,
        "supplements": supplements
    }

    mst = law_id or src.stem.rsplit('_', 1)[-1]

    output = f"""# {law_name}

- **MST(법령 마스터 번호)**: {mst}
- **공포일자**: {anc_yd}
- **시행일**: {ef_yd}

```json
{json.dumps(json_block, ensure_ascii=False, indent=2)}
```
"""

    write_lf(dst, output)


# ════════════════════════════════════════
# 행정규칙 변환
# ════════════════════════════════════════
def convert_admin_rule(src: Path, dst: Path):
    """행정규칙 → Rust ```json 블록 포맷.
    행정규칙은 조문 구조가 없는 경우가 많아 전체 내용을 단일 article로."""
    content = src.read_text(encoding='utf-8', errors='replace')
    sections = extract_sections(content)

    rule_name = ''
    basic = {}
    basic_raw = sections.get('행정규칙기본정보', '')
    if basic_raw:
        try:
            basic = json.loads(basic_raw)
            rule_name = basic.get('행정규칙명', '')
        except (json.JSONDecodeError, TypeError):
            pass
    if not rule_name:
        rule_name = src.stem.rsplit('_', 1)[0]

    ef_yd = basic.get('시행일자', '')
    anc_yd = basic.get('발령일자', '') or ef_yd
    anc_no = basic.get('발령번호', '')
    rule_id = basic.get('행정규칙ID', '')

    # 행정규칙은 대부분 비구조화 텍스트
    body_raw = sections.get('조문내용', '').strip()

    articles = []
    if body_raw:
        # 조문 구조가 있는지 시도
        art_pattern = re.findall(
            r'(제\d+조(?:의\d+)?(?:\s*[\(（][^)）]*[\)）])?[^제]*?)(?=제\d+조|$)',
            body_raw, re.DOTALL
        )
        if len(art_pattern) > 1:
            for i, art_text in enumerate(art_pattern):
                art_text = art_text.strip()
                if not art_text:
                    continue
                nm = re.match(r'(제\d+조(?:의\d+)?(?:\s*[\(（][^)）]*[\)）])?)', art_text)
                number_str = nm.group(1) if nm else f"제{i+1}조"
                articles.append({
                    "number": number_str,
                    "text": art_text,
                    "anchor": f"J{i+1}:0"
                })
        else:
            # 단일 article로
            articles.append({
                "number": f"제1조({rule_name})",
                "text": body_raw,
                "anchor": "J1:0"
            })

    meta = {
        "lsNm": rule_name,
        "ancYd": anc_yd,
        "efYd": ef_yd,
        "lsId": rule_id,
        "lsiSeq": "",
        "ancNo": anc_no
    }

    json_block = {
        "meta": meta,
        "title": rule_name,
        "articles": articles,
        "supplements": []
    }

    mst = rule_id or src.stem.rsplit('_', 1)[-1]

    output = f"""# {rule_name}

- **MST(법령 마스터 번호)**: {mst}
- **발령일자**: {anc_yd}
- **시행일**: {ef_yd}

```json
{json.dumps(json_block, ensure_ascii=False, indent=2)}
```
"""

    write_lf(dst, output)


# ════════════════════════════════════════
# Main
# ════════════════════════════════════════
def main():
    parser = argparse.ArgumentParser(
        description="세컨드브레인_로컬DB → zeroclaw vault 호환 corpus 변환"
    )
    parser.add_argument(
        '--source', type=str,
        default=r'C:\Users\kjccj\세컨드브레인_로컬DB',
        help='원본 법령/판례 디렉토리'
    )
    parser.add_argument(
        '--output', type=str,
        default=r'C:\Users\kjccj\세컨드브레인_로컬DB\_corpus',
        help='변환된 corpus 출력 디렉토리'
    )
    parser.add_argument(
        '--category', type=str, default=None,
        help='특정 카테고리만 변환 (예: 현행법령)'
    )
    args = parser.parse_args()

    source = Path(args.source)
    output = Path(args.output)

    if not source.exists():
        print(f"ERROR: Source directory not found: {source}")
        sys.exit(1)

    t0 = time.time()
    print(f"\n{'='*60}")
    print(f"  Legal Corpus Preprocessor")
    print(f"  Source: {source}")
    print(f"  Output: {output}")
    print(f"{'='*60}\n")

    # 카테고리별 처리
    all_categories = CASE_DIRS | STATUTE_DIRS
    if args.category:
        all_categories = {args.category}

    total_files = 0
    total_errors = 0

    for category_dir in sorted(source.iterdir()):
        if not category_dir.is_dir():
            continue
        category = category_dir.name
        if category not in all_categories:
            continue

        print(f"[{category}] Processing...")
        cat_count = 0
        cat_errors = 0

        for date_dir in sorted(category_dir.iterdir()):
            if not date_dir.is_dir():
                continue
            date_folder = date_dir.name

            for md_file in date_dir.glob('*.md'):
                dst = output / category / date_folder / md_file.name
                try:
                    if category == '판례':
                        convert_case_file(md_file, dst)
                    elif category == '헌재결정례':
                        convert_constitutional_case(md_file, dst)
                    elif category in ('현행법령', '연혁법령'):
                        convert_national_statute(md_file, dst, category)
                    elif category == '자치법규':
                        convert_local_statute(md_file, dst)
                    elif category == '행정규칙':
                        convert_admin_rule(md_file, dst)
                    cat_count += 1
                except Exception as e:
                    cat_errors += 1
                    if cat_errors <= 5:
                        print(f"  ERROR: {md_file.name}: {e}")

            # Progress every 500 files
            if cat_count > 0 and cat_count % 5000 == 0:
                elapsed = time.time() - t0
                print(f"  {cat_count:,} files ({elapsed:.0f}s)")

        print(f"  → {cat_count:,} files converted, {cat_errors} errors")
        total_files += cat_count
        total_errors += cat_errors

    elapsed = time.time() - t0
    print(f"\n{'='*60}")
    print(f"  DONE in {elapsed:.1f}s")
    print(f"  Total: {total_files:,} files, {total_errors} errors")
    print(f"  Output: {output}")
    print(f"{'='*60}")
    print(f"\nNext step: run zeroclaw to build domain.db:")
    print(f"  zeroclaw vault domain build \"{output}\" --out domain.db")


if __name__ == '__main__':
    main()
