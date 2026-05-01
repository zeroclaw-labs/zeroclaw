#!/usr/bin/env python3
"""
rescue_broken_json.py — 깨진 JSON 연혁법령 파일 복구 + domain.db 추가 인제스트

원본 법령정보센터 JSON에 unterminated string 등의 오류가 있는 파일들을
수리하여 domain.db에 추가합니다.

전략:
1. corpus 디렉토리의 모든 법령 파일을 스캔
2. JSON 파싱 시도 → 실패하면 repair 시도
3. repair 방법:
   a. 끝나지 않은 문자열 → 가장 가까운 " 를 찾아 닫기
   b. trailing comma → 제거
   c. 잘린 JSON → 끝 부분을 잘라내고 배열/객체 닫기
4. repair 성공 시 domain.db에 INSERT
"""

import hashlib
import json
import re
import sqlite3
import sys
import time
import uuid
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
from build_domain_db_fast import (
    find_json_block, parse_statute_file, statute_slug, article_key,
    RE_ARTICLE_NUM, RE_ARTICLE_TITLE, extract_statute_citations,
    CORPUS_DIR, STATUTE_DIRS
)

DB_PATH = Path(r"C:\Users\kjccj\세컨드브레인_로컬DB\domain.db")


def repair_json(text):
    """Attempt to fix common JSON errors in Korean legal data."""
    # Strategy 1: Try as-is first
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        pass

    # Strategy 2: Fix unterminated strings
    # The most common error: a string containing unescaped control characters
    # or HTML that breaks JSON parsing.
    # Replace problematic characters inside strings
    fixed = text
    # Escape unescaped backslashes that aren't part of valid escape sequences
    fixed = re.sub(r'\\(?!["\\/bfnrtu])', r'\\\\', fixed)
    # Remove control characters (except \n \r \t)
    fixed = re.sub(r'[\x00-\x08\x0b\x0c\x0e-\x1f]', '', fixed)

    try:
        return json.loads(fixed)
    except json.JSONDecodeError:
        pass

    # Strategy 3: Truncate at the error point and close brackets
    # Find the longest valid prefix
    for end in range(len(fixed), max(0, len(fixed) - 5000), -100):
        chunk = fixed[:end]
        # Count open/close brackets
        opens = chunk.count('{') + chunk.count('[')
        closes = chunk.count('}') + chunk.count(']')
        # Try to close remaining brackets
        suffix = ''
        # Simple heuristic: close with ] or } as needed
        open_braces = chunk.count('{') - chunk.count('}')
        open_brackets = chunk.count('[') - chunk.count(']')

        # Clean trailing comma/whitespace
        chunk = chunk.rstrip().rstrip(',')
        # Close any open string
        if chunk.count('"') % 2 == 1:
            chunk += '"'
        suffix = ']' * max(0, open_brackets) + '}' * max(0, open_braces)

        try:
            result = json.loads(chunk + suffix)
            return result
        except json.JSONDecodeError:
            continue

    return None


def find_failed_files(corpus_dir):
    """Find all statute files where JSON parsing fails."""
    failed = []
    total_checked = 0
    for category_dir in sorted(corpus_dir.iterdir()):
        if not category_dir.is_dir():
            continue
        category = category_dir.name
        if category not in STATUTE_DIRS:
            continue
        for date_dir in sorted(category_dir.iterdir()):
            if not date_dir.is_dir():
                continue
            for md_file in date_dir.glob('*.md'):
                total_checked += 1
                try:
                    content = md_file.read_text(encoding='utf-8', errors='replace')
                except Exception:
                    continue
                json_text = find_json_block(content)
                if not json_text:
                    continue  # no json block = different error, not our target
                try:
                    json.loads(json_text)
                except json.JSONDecodeError:
                    failed.append((md_file, category, content, json_text))
    return failed, total_checked


def ingest_repaired(conn, filepath, category, raw_data, content):
    """Insert repaired statute articles into domain.db."""
    meta = raw_data.get('meta') or {}
    law_name = meta.get('lsNm') or raw_data.get('title') or ''
    if not law_name:
        title_m = re.match(r'^#\s+(.+)$', content, re.MULTILINE)
        if title_m:
            law_name = title_m.group(1).strip()
    if not law_name:
        law_name = filepath.stem.rsplit('_', 1)[0]
    if not law_name:
        return 0

    anc_yd = meta.get('ancYd', '')
    ef_yd = meta.get('efYd', '')
    ls_id = meta.get('lsId', '')

    promulgation_date = anc_yd or ef_yd
    if not promulgation_date:
        parent = filepath.parent.name
        if len(parent) == 8 and parent.isdigit():
            promulgation_date = parent

    is_current = (category == '현행법령')
    now = int(time.time())
    inserted = 0

    articles = raw_data.get('articles') or []
    for ra in articles:
        number = ra.get('number', '')
        text = ra.get('text', '')
        if not number or not text:
            continue
        m = RE_ARTICLE_NUM.match(number)
        if not m:
            continue

        art_num = int(m.group(1))
        art_sub = int(m.group(2)) if m.group(2) else None
        key = article_key(art_num, art_sub)
        base_slug = statute_slug(law_name, art_num, art_sub)
        slug = f"{base_slug}@{promulgation_date}" if promulgation_date else base_slug
        checksum = hashlib.sha256((text + slug).encode('utf-8')).hexdigest()
        uid = uuid.uuid4().hex

        title_kw_m = RE_ARTICLE_TITLE.search(number)
        title_kw = title_kw_m.group(1) if title_kw_m else None

        try:
            conn.execute("""
                INSERT INTO vault_documents
                    (uuid, title, content, source_type, source_device_id,
                     original_path, checksum, doc_type, char_count, created_at, updated_at)
                VALUES (?, ?, ?, 'local_file', 'local', ?, ?, 'statute_article', ?, ?, ?)
            """, (uid, slug, text, str(filepath), checksum, len(text), now, now))
            doc_id = conn.execute("SELECT last_insert_rowid()").fetchone()[0]

            # Frontmatter
            for k, v in [('law_name', law_name), ('article_num', str(art_num)),
                         ('article_key', key), ('promulgated_at', anc_yd),
                         ('effective_at', ef_yd), ('promulgation_date', promulgation_date),
                         ('ls_id', ls_id), ('category', category)]:
                if v:
                    conn.execute(
                        "INSERT OR IGNORE INTO vault_frontmatter (doc_id, key, value) VALUES (?, ?, ?)",
                        (doc_id, k, v))
            if title_kw:
                conn.execute(
                    "INSERT OR IGNORE INTO vault_frontmatter (doc_id, key, value) VALUES (?, ?, ?)",
                    (doc_id, 'title_kw', title_kw))

            # Aliases
            if is_current:
                conn.execute(
                    "INSERT OR IGNORE INTO vault_aliases (doc_id, alias) VALUES (?, ?)",
                    (doc_id, base_slug))
            display = f"{law_name} 제{art_num}조의{art_sub}" if art_sub else f"{law_name} 제{art_num}조"
            conn.execute("INSERT OR IGNORE INTO vault_aliases (doc_id, alias) VALUES (?, ?)",
                         (doc_id, display))

            # Tags
            for tag, ttype in [('domain:legal', 'domain'), ('kind:statute', 'kind'),
                               (f'law:{law_name}', 'law'),
                               ('status:current' if is_current else 'status:historical', 'status')]:
                conn.execute(
                    "INSERT OR IGNORE INTO vault_tags (doc_id, tag_name, tag_type) VALUES (?, ?, ?)",
                    (doc_id, tag, ttype))

            inserted += 1
        except Exception:
            continue

    return inserted


def main():
    corpus = Path(CORPUS_DIR)
    t0 = time.time()

    print(f"\n{'='*60}")
    print(f"  Rescue Broken JSON Files")
    print(f"  Corpus: {corpus}")
    print(f"  DB: {DB_PATH}")
    print(f"{'='*60}\n")

    print("[1/3] Scanning for broken JSON files...")
    failed, total_checked = find_failed_files(corpus)
    print(f"  Scanned: {total_checked:,} statute files")
    print(f"  Broken JSON: {len(failed):,} files\n")

    if not failed:
        print("No broken files found. Done.")
        return

    print("[2/3] Repairing and ingesting...")
    conn = sqlite3.connect(str(DB_PATH))
    conn.execute("PRAGMA journal_mode = WAL")
    conn.execute("PRAGMA synchronous = NORMAL")

    repaired = 0
    repair_failed = 0
    total_articles = 0

    for i, (filepath, category, content, json_text) in enumerate(failed):
        raw = repair_json(json_text)
        if raw is None:
            repair_failed += 1
            if repair_failed <= 5:
                print(f"  REPAIR FAILED: {filepath.name}")
            continue

        articles_inserted = ingest_repaired(conn, filepath, category, raw, content)
        if articles_inserted > 0:
            repaired += 1
            total_articles += articles_inserted
        else:
            repair_failed += 1

        if (i + 1) % 500 == 0:
            conn.commit()
            print(f"  {i+1}/{len(failed)} - repaired: {repaired}, articles: {total_articles}")

    conn.commit()

    # Resolve new links
    print("\n[3/3] Resolving new links...")
    conn.execute("""
        UPDATE vault_links SET
            target_doc_id = (
                SELECT id FROM vault_documents WHERE title = vault_links.target_raw LIMIT 1
            ),
            is_resolved = 1
        WHERE is_resolved = 0
          AND EXISTS (SELECT 1 FROM vault_documents WHERE title = vault_links.target_raw)
    """)
    conn.commit()
    conn.close()

    elapsed = time.time() - t0
    print(f"\n{'='*60}")
    print(f"  Rescue complete in {elapsed:.1f}s")
    print(f"  Repaired: {repaired:,} files → {total_articles:,} articles")
    print(f"  Repair failed: {repair_failed:,} files")
    print(f"{'='*60}")


if __name__ == '__main__':
    main()
