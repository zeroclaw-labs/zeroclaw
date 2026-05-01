#!/usr/bin/env python3
"""
legal_search.py - domain.db 인터랙티브 법률 검색 도구

사용법:
  python scripts/legal_search.py

검색 명령어:
  /s 키워드        - 전문검색 (FTS5)
  /c 사건번호      - 판례 상세 조회
  /l 법조문        - 법조문 인용 판례 검색
  /r 사건번호      - 참조조문 (적용법조) 조회
  /b 사건번호      - 백링크 (이 판례를 인용한 다른 판례)
  /h 사건번호      - 심급 연결 (대법원-항소심-1심)
  /t 태그          - 태그 기반 검색
  /stat            - DB 통계
  /q               - 종료
"""

import sqlite3
import re
import sys
import json

DB_PATH = r"C:\Users\kjccj\세컨드브레인_로컬DB\domain.db"

def get_conn():
    conn = sqlite3.connect(f'file:{DB_PATH}?mode=ro', uri=True)
    conn.execute('PRAGMA cache_size = -128000')
    conn.execute('PRAGMA mmap_size = 536870912')
    return conn

def clean_html(text):
    return re.sub(r'<[^>]+>', '', text or '').strip()

def fts_search(conn, keyword, limit=20):
    """FTS5 전문검색."""
    rows = conn.execute("""
        SELECT d.title, d.doc_type,
               snippet(vault_docs_fts, 1, '[', ']', '...', 30) as preview
        FROM vault_docs_fts fts
        JOIN vault_documents d ON d.id = fts.rowid
        WHERE vault_docs_fts MATCH ?
        LIMIT ?
    """, (keyword, limit)).fetchall()
    return rows

def case_detail(conn, case_number):
    """판례 상세 조회."""
    slug = case_number if case_number.startswith('case::') else f'case::{case_number}'
    row = conn.execute(
        "SELECT id, title, content FROM vault_documents WHERE title = ?", (slug,)
    ).fetchone()
    if not row:
        # alias 검색
        row = conn.execute("""
            SELECT d.id, d.title, d.content FROM vault_aliases a
            JOIN vault_documents d ON d.id = a.doc_id
            WHERE a.alias LIKE ? LIMIT 1
        """, (f'%{case_number}%',)).fetchone()
    return row

def case_applied_statutes(conn, case_slug):
    """판례의 적용법조 조회."""
    rows = conn.execute("""
        SELECT l.target_raw, l.context FROM vault_links l
        JOIN vault_documents d ON d.id = l.source_doc_id
        WHERE d.title = ? AND l.display_text = 'cites'
        ORDER BY l.target_raw
    """, (case_slug,)).fetchall()
    return rows

def statute_citing_cases(conn, statute_slug, limit=30):
    """법조문을 인용한 판례 검색."""
    rows = conn.execute("""
        SELECT d.title, fm_name.value, fm_court.value, fm_date.value
        FROM vault_links l
        JOIN vault_documents d ON d.id = l.source_doc_id
        LEFT JOIN vault_frontmatter fm_name ON fm_name.doc_id = d.id AND fm_name.key = 'case_name'
        LEFT JOIN vault_frontmatter fm_court ON fm_court.doc_id = d.id AND fm_court.key = 'court_name'
        LEFT JOIN vault_frontmatter fm_date ON fm_date.doc_id = d.id AND fm_date.key = 'verdict_date'
        WHERE l.target_raw = ? AND l.display_text = 'cites' AND d.doc_type = 'case'
        ORDER BY fm_date.value DESC
        LIMIT ?
    """, (statute_slug, limit)).fetchall()
    return rows

def backlinks(conn, case_slug, limit=30):
    """이 판례를 인용한 다른 판례."""
    rows = conn.execute("""
        SELECT d.title, fm_name.value, fm_date.value
        FROM vault_links l
        JOIN vault_documents d ON d.id = l.source_doc_id
        LEFT JOIN vault_frontmatter fm_name ON fm_name.doc_id = d.id AND fm_name.key = 'case_name'
        LEFT JOIN vault_frontmatter fm_date ON fm_date.doc_id = d.id AND fm_date.key = 'verdict_date'
        WHERE l.target_raw = ? AND l.display_text = 'ref-case'
        ORDER BY fm_date.value DESC
        LIMIT ?
    """, (case_slug, limit)).fetchall()
    return rows

def court_hierarchy(conn, case_slug):
    """심급 연결."""
    row = conn.execute(
        "SELECT content FROM vault_documents WHERE title = ?", (case_slug,)
    ).fetchone()
    if not row:
        return []
    content = row[0]
    results = []
    # 원심
    m = re.search(r'【원심판결】\s*(.+?)(?:<br|【|\n)', content, re.DOTALL)
    if m:
        text = clean_html(m.group(1))
        case_nums = re.findall(r'(\d{2,4}[가-힣]{1,4}\d{1,7})', text)
        if case_nums:
            results.append(('원심(2심)', text, case_nums[0]))
    # 1심
    m2 = re.search(r'【제1심판결】\s*(.+?)(?:<br|【|\n)', content, re.DOTALL)
    if m2:
        text = clean_html(m2.group(1))
        case_nums = re.findall(r'(\d{2,4}[가-힣]{1,4}\d{1,7})', text)
        if case_nums:
            results.append(('1심', text, case_nums[0]))
    return results

def db_stats(conn):
    """DB 통계."""
    stats = {}
    for row in conn.execute("SELECT doc_type, COUNT(*) FROM vault_documents GROUP BY doc_type"):
        stats[row[0]] = row[1]
    stats['total'] = conn.execute("SELECT COUNT(*) FROM vault_documents").fetchone()[0]
    stats['links'] = conn.execute("SELECT COUNT(*) FROM vault_links").fetchone()[0]
    stats['resolved'] = conn.execute("SELECT COUNT(*) FROM vault_links WHERE is_resolved=1").fetchone()[0]
    stats['aliases'] = conn.execute("SELECT COUNT(*) FROM vault_aliases").fetchone()[0]
    stats['tags'] = conn.execute("SELECT COUNT(*) FROM vault_tags").fetchone()[0]
    return stats

def main():
    conn = get_conn()
    print()
    print("=" * 50)
    print("  Legal Search - domain.db Interactive Query")
    print("=" * 50)
    print()
    print("Commands:")
    print("  /s keyword     - Full-text search")
    print("  /c case_number - Case detail")
    print("  /l statute     - Cases citing statute (e.g. /l statute::민법::750)")
    print("  /r case_number - Applied statutes")
    print("  /b case_number - Backlinks (who cites this case)")
    print("  /h case_number - Court hierarchy (3-tier)")
    print("  /t tag_name    - Tag search")
    print("  /stat          - DB statistics")
    print("  /q             - Quit")
    print()

    while True:
        try:
            cmd = input("search> ").strip()
        except (EOFError, KeyboardInterrupt):
            break

        if not cmd:
            continue
        if cmd == '/q':
            break

        if cmd == '/stat':
            stats = db_stats(conn)
            print(f"\n  Documents: {stats['total']:,}")
            for k, v in stats.items():
                if k not in ('total',):
                    print(f"    {k}: {v:,}")
            print()
            continue

        parts = cmd.split(maxsplit=1)
        if len(parts) < 2 and parts[0] != '/stat':
            print("  Usage: /command argument")
            continue

        command = parts[0]
        arg = parts[1] if len(parts) > 1 else ''

        if command == '/s':
            import time
            t0 = time.time()
            results = fts_search(conn, arg)
            elapsed = (time.time() - t0) * 1000
            print(f"\n  Results for '{arg}' ({len(results)} hits, {elapsed:.0f}ms):")
            for slug, doc_type, preview in results:
                preview_clean = clean_html(preview)[:80]
                print(f"    [{doc_type}] {slug}")
                print(f"           {preview_clean}")
            print()

        elif command == '/c':
            row = case_detail(conn, arg)
            if not row:
                print(f"  Not found: {arg}")
                continue
            doc_id, slug, content = row
            fm = dict(conn.execute(
                "SELECT key, value FROM vault_frontmatter WHERE doc_id = ?", (doc_id,)
            ).fetchall())
            print(f"\n  === {slug} ===")
            print(f"  Case: {fm.get('case_name', '')}")
            print(f"  Court: {fm.get('court_name', '')} | Date: {fm.get('verdict_date', '')}")
            print(f"  Type: {fm.get('case_type_name', '')} | Verdict: {fm.get('verdict_type', '')}")
            # 판시사항
            m = re.search(r'## 판시사항\n(.+?)(?=\n## )', content, re.DOTALL)
            if m:
                holding = clean_html(m.group(1))[:500]
                print(f"\n  [판시사항]\n  {holding}")
            # 적용법조
            refs = case_applied_statutes(conn, slug)
            if refs:
                print(f"\n  [적용법조] ({len(refs)}건)")
                for target, ctx in refs[:10]:
                    print(f"    {target}")
            print()

        elif command == '/l':
            statute = arg if arg.startswith('statute::') else f'statute::{arg}'
            import time
            t0 = time.time()
            results = statute_citing_cases(conn, statute)
            elapsed = (time.time() - t0) * 1000
            print(f"\n  Cases citing {statute} ({len(results)} hits, {elapsed:.0f}ms):")
            for slug, name, court, date in results:
                print(f"    {slug} | {court or ''} {date or ''} | {name or ''}")
            print()

        elif command == '/r':
            slug = arg if arg.startswith('case::') else f'case::{arg}'
            refs = case_applied_statutes(conn, slug)
            print(f"\n  Applied statutes for {slug} ({len(refs)}):")
            for target, ctx in refs:
                print(f"    {target}")
            print()

        elif command == '/b':
            slug = arg if arg.startswith('case::') else f'case::{arg}'
            import time
            t0 = time.time()
            results = backlinks(conn, slug)
            elapsed = (time.time() - t0) * 1000
            print(f"\n  Cases citing {slug} ({len(results)} backlinks, {elapsed:.0f}ms):")
            for s, name, date in results:
                print(f"    {s} | {date or ''} | {name or ''}")
            print()

        elif command == '/h':
            slug = arg if arg.startswith('case::') else f'case::{arg}'
            hierarchy = court_hierarchy(conn, slug)
            fm = dict(conn.execute("""
                SELECT key, value FROM vault_frontmatter fm
                JOIN vault_documents d ON d.id = fm.doc_id
                WHERE d.title = ? AND key IN ('court_name','verdict_date','case_name')
            """, (slug,)).fetchall())
            print(f"\n  Court Hierarchy for {slug}:")
            print(f"  [대법원] {fm.get('court_name','')} {fm.get('verdict_date','')} {fm.get('case_name','')}")
            for tier, text, case_num in hierarchy:
                exists = conn.execute(
                    "SELECT 1 FROM vault_documents WHERE title = ?", (f'case::{case_num}',)
                ).fetchone()
                status = 'DB' if exists else 'MISSING'
                print(f"  [{tier}] {text} [{status}]")
            print()

        elif command == '/t':
            import time
            t0 = time.time()
            rows = conn.execute("""
                SELECT d.title, d.doc_type FROM vault_tags t
                JOIN vault_documents d ON d.id = t.doc_id
                WHERE t.tag_name = ? LIMIT 20
            """, (arg,)).fetchall()
            elapsed = (time.time() - t0) * 1000
            print(f"\n  Tag '{arg}' ({len(rows)} hits, {elapsed:.0f}ms):")
            for slug, dtype in rows:
                print(f"    [{dtype}] {slug}")
            print()

        else:
            print(f"  Unknown command: {command}")

    conn.close()
    print("Bye.")

if __name__ == '__main__':
    main()
