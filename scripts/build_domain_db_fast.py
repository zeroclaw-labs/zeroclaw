#!/usr/bin/env python3
"""
build_domain_db_fast.py — 고속 병렬 domain.db 빌더

Rust vault schema와 100% 호환되는 domain.db를 Python multiprocessing으로 빌드.
16코어 기준 약 30~60분 내 319K 파일 처리 목표.

아키텍처:
  - Worker pool (12 프로세스): 파일 파싱 → (doc, aliases, tags, links) 튜플 반환
  - Main thread: batch INSERT (5000건 단위) → SQLite WAL 모드

스키마: src/vault/schema.rs 동일
  vault_documents, vault_links, vault_frontmatter, vault_aliases, vault_tags, vault_docs_fts
"""

import argparse
import hashlib
import json
import os
import re
import sqlite3
import sys
import time
import uuid
from concurrent.futures import ProcessPoolExecutor, as_completed
from pathlib import Path

# ──────────────────────────────────────────
# Config
# ──────────────────────────────────────────
SOURCE_DIR = Path(r"C:\Users\kjccj\세컨드브레인_로컬DB")
CORPUS_DIR = Path(r"C:\Users\kjccj\세컨드브레인_로컬DB\_corpus2")
OUTPUT_DB = Path(r"C:\Users\kjccj\세컨드브레인_로컬DB\domain.db")

BATCH_SIZE = 5000
NUM_WORKERS = 12

CASE_DIRS = {"판례", "헌재결정례"}
STATUTE_DIRS = {"현행법령", "연혁법령", "자치법규", "행정규칙"}

# ──────────────────────────────────────────
# Schema (matches src/vault/schema.rs exactly)
# ──────────────────────────────────────────
SCHEMA_SQL = """
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
PRAGMA cache_size = -256000;
PRAGMA temp_store = MEMORY;
PRAGMA mmap_size = 1073741824;

CREATE TABLE IF NOT EXISTS vault_documents (
    id               INTEGER PRIMARY KEY AUTOINCREMENT,
    uuid             TEXT NOT NULL UNIQUE,
    title            TEXT,
    content          TEXT NOT NULL,
    html_content     TEXT,
    source_type      TEXT NOT NULL DEFAULT 'local_file',
    source_device_id TEXT DEFAULT 'local',
    original_path    TEXT,
    checksum         TEXT,
    doc_type         TEXT,
    char_count       INTEGER NOT NULL DEFAULT 0,
    created_at       INTEGER NOT NULL,
    updated_at       INTEGER NOT NULL,
    embedding_model    TEXT,
    embedding_dim      INTEGER,
    embedding_provider TEXT,
    embedding_version  INTEGER NOT NULL DEFAULT 1,
    embedding_created_at INTEGER
);

CREATE TABLE IF NOT EXISTS vault_links (
    id             INTEGER PRIMARY KEY AUTOINCREMENT,
    source_doc_id  INTEGER NOT NULL,
    target_raw     TEXT NOT NULL,
    target_doc_id  INTEGER,
    display_text   TEXT,
    link_type      TEXT NOT NULL DEFAULT 'wikilink',
    context        TEXT,
    line_number    INTEGER,
    is_resolved    INTEGER NOT NULL DEFAULT 0,
    FOREIGN KEY (source_doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE,
    FOREIGN KEY (target_doc_id) REFERENCES vault_documents(id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS vault_frontmatter (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id INTEGER NOT NULL,
    key    TEXT NOT NULL,
    value  TEXT,
    UNIQUE(doc_id, key),
    FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS vault_aliases (
    id     INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id INTEGER NOT NULL,
    alias  TEXT NOT NULL,
    UNIQUE(doc_id, alias),
    FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS vault_tags (
    id       INTEGER PRIMARY KEY AUTOINCREMENT,
    doc_id   INTEGER NOT NULL,
    tag_name TEXT NOT NULL,
    tag_type TEXT,
    UNIQUE(doc_id, tag_name),
    FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS vault_embeddings (
    doc_id     INTEGER PRIMARY KEY,
    embedding  BLOB NOT NULL,
    dim        INTEGER NOT NULL,
    created_at INTEGER NOT NULL DEFAULT (strftime('%%s','now')),
    FOREIGN KEY (doc_id) REFERENCES vault_documents(id) ON DELETE CASCADE
);

CREATE VIRTUAL TABLE IF NOT EXISTS vault_docs_fts USING fts5(
    title, content,
    content='vault_documents',
    content_rowid='id',
    tokenize='trigram'
);

CREATE TRIGGER IF NOT EXISTS vault_docs_fts_ai AFTER INSERT ON vault_documents BEGIN
    INSERT INTO vault_docs_fts(rowid, title, content)
    VALUES (new.id, new.title, new.content);
END;
CREATE TRIGGER IF NOT EXISTS vault_docs_fts_ad AFTER DELETE ON vault_documents BEGIN
    INSERT INTO vault_docs_fts(vault_docs_fts, rowid, title, content)
    VALUES ('delete', old.id, old.title, old.content);
END;
CREATE TRIGGER IF NOT EXISTS vault_docs_fts_au AFTER UPDATE ON vault_documents BEGIN
    INSERT INTO vault_docs_fts(vault_docs_fts, rowid, title, content)
    VALUES ('delete', old.id, old.title, old.content);
    INSERT INTO vault_docs_fts(rowid, title, content)
    VALUES (new.id, new.title, new.content);
END;
"""

INDEX_SQL = """
CREATE INDEX IF NOT EXISTS idx_vault_docs_checksum ON vault_documents(checksum);
CREATE INDEX IF NOT EXISTS idx_vault_docs_title ON vault_documents(title);
CREATE INDEX IF NOT EXISTS idx_vault_docs_doctype ON vault_documents(doc_type);
CREATE INDEX IF NOT EXISTS idx_vault_links_source ON vault_links(source_doc_id);
CREATE INDEX IF NOT EXISTS idx_vault_links_target_raw ON vault_links(target_raw);
CREATE INDEX IF NOT EXISTS idx_vault_links_target_doc ON vault_links(target_doc_id);
CREATE INDEX IF NOT EXISTS idx_vault_fm_doc ON vault_frontmatter(doc_id);
CREATE INDEX IF NOT EXISTS idx_vault_fm_key ON vault_frontmatter(key);
CREATE INDEX IF NOT EXISTS idx_vault_aliases_doc ON vault_aliases(doc_id);
CREATE INDEX IF NOT EXISTS idx_vault_aliases_alias ON vault_aliases(alias);
CREATE INDEX IF NOT EXISTS idx_vault_tags_doc ON vault_tags(doc_id);
CREATE INDEX IF NOT EXISTS idx_vault_tags_name ON vault_tags(tag_name);
CREATE INDEX IF NOT EXISTS idx_vault_tags_type ON vault_tags(tag_type);
"""

# ──────────────────────────────────────────
# Citation extraction (port from citation_patterns.rs)
# ──────────────────────────────────────────
RE_STATUTE_CITE = re.compile(
    r'([가-힣]+(?:법|령|규칙|규정|특례법|조례|법률|처벌법|기본법|촉진법))\s*'
    r'제\s*(\d+)\s*조(?:의\s*(\d+))?'
)
RE_BARE_ARTICLE = re.compile(r'(?:같은\s*법|동법)\s*제\s*(\d+)\s*조(?:의\s*(\d+))?')
RE_FOLLOW_ARTICLE = re.compile(r',\s*제\s*(\d+)\s*조(?:의\s*(\d+))?')
RE_CASE_NUMBER = re.compile(
    r'(\d{2,4})\s*[가-힣]{1,3}\s*(\d+)'
)


def extract_statute_citations(text, default_law=None):
    """Extract (law_name, article, article_sub) from text."""
    if not text:
        return []
    refs = []
    last_law = default_law

    for m in RE_STATUTE_CITE.finditer(text):
        law = m.group(1)
        art = int(m.group(2))
        sub = int(m.group(3)) if m.group(3) else None
        refs.append((law, art, sub))
        last_law = law

    if last_law:
        for m in RE_BARE_ARTICLE.finditer(text):
            art = int(m.group(1))
            sub = int(m.group(2)) if m.group(2) else None
            refs.append((last_law, art, sub))

    # Deduplicate
    seen = set()
    out = []
    for r in refs:
        if r not in seen:
            seen.add(r)
            out.append(r)
    return out


def extract_case_citations(text):
    """Extract case numbers like 2024다12345."""
    if not text:
        return []
    # More precise pattern for Korean case numbers
    pattern = re.compile(r'(\d{2,4}[가-힣]{1,4}\d{1,7})')
    return list(set(pattern.findall(text)))


def statute_slug(law, art, sub):
    key = f"{art}-{sub}" if sub else str(art)
    return f"statute::{law}::{key}"


def article_key(art, sub):
    return f"{art}-{sub}" if sub else str(art)


def statute_display(law, art, sub):
    if sub:
        return f"{law} 제{art}조의{sub}"
    return f"{law} 제{art}조"


def case_slug(case_number):
    return f"case::{case_number}"


# ──────────────────────────────────────────
# Statute JSON parser
# ──────────────────────────────────────────
def find_json_block(md):
    """Find first ```json ... ``` fenced block."""
    marker = "```json\n"
    start = md.find(marker)
    if start < 0:
        return None
    after = md[start + len(marker):]
    end = after.find("\n```")
    if end < 0:
        return None
    return after[:end]


RE_ARTICLE_NUM = re.compile(r'제\s*(\d+)\s*조(?:의\s*(\d+))?')
RE_ARTICLE_TITLE = re.compile(r'\(([^)]+)\)')


def parse_statute_file(filepath, content, category=''):
    """Parse a Rust-compatible statute markdown → list of doc dicts.

    category: '현행법령'|'연혁법령'|'자치법규'|'행정규칙'

    Slug 체계 (공포일 기반 버전 관리):
      - 모든 법령: statute::법명::조@공포일  (예: statute::민법::750@20251001)
      - 현행법령만 추가 alias: statute::법명::조  (정식 slug → 최신 버전 바로가기)
      - 연혁법령: @공포일 slug만 (최신이 아니므로 정식 alias 없음)

    이렇게 하면:
      - 동일 법령의 현행/연혁 버전이 절대 충돌하지 않음
      - 판례 참조조문 → statute::법명::조 alias → 현행 버전으로 연결
      - 특정 시점 법령 조회: statute::법명::조@YYYYMMDD
    """
    json_text = find_json_block(content)
    if not json_text:
        return None, f"no json block: {filepath}"

    try:
        raw = json.loads(json_text)
    except json.JSONDecodeError as e:
        return None, f"json parse error: {filepath}: {e}"

    meta = raw.get('meta') or {}
    law_name = meta.get('lsNm') or raw.get('title') or ''
    if not law_name:
        # Fallback: extract from markdown # title
        title_m = re.match(r'^#\s+(.+)$', content, re.MULTILINE)
        if title_m:
            law_name = title_m.group(1).strip()
    if not law_name:
        # Fallback: filename
        law_name = Path(filepath).stem.rsplit('_', 1)[0]
    if not law_name:
        return None, f"no law name: {filepath}"

    anc_yd = meta.get('ancYd', '')
    ef_yd = meta.get('efYd', '')
    ls_id = meta.get('lsId', '')
    lsi_seq = meta.get('lsiSeq', '')
    anc_no = meta.get('ancNo', '')

    # 공포일 결정: ancYd(공포일) > efYd(시행일) > 폴더명
    promulgation_date = anc_yd or ef_yd
    if not promulgation_date:
        # 폴더명에서 추출 (YYYYMMDD)
        parent = Path(filepath).parent.name
        if len(parent) == 8 and parent.isdigit():
            promulgation_date = parent

    is_current = (category == '현행법령')

    now = int(time.time())
    results = []
    articles = raw.get('articles') or []

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
        base_slug = statute_slug(law_name, art_num, art_sub)  # statute::법명::조

        # 공포일 기반 버전 slug: statute::법명::조@YYYYMMDD
        if promulgation_date:
            slug = f"{base_slug}@{promulgation_date}"
        else:
            slug = base_slug  # 공포일 없으면 base slug 사용

        checksum = hashlib.sha256((text + slug).encode('utf-8')).hexdigest()

        title_kw_m = RE_ARTICLE_TITLE.search(number)
        title_kw = title_kw_m.group(1) if title_kw_m else None

        # Citations within article body
        citations = extract_statute_citations(text, law_name)
        links = []
        for (claw, cart, csub) in citations:
            target = statute_slug(claw, cart, csub)  # 참조 대상은 base slug
            if target != base_slug:
                links.append(('cites', target, f"{claw} 제{cart}조"))

        # Aliases
        aliases = []
        # 현행법령만: 정식(canonical) slug를 alias로 등록 → 최신 버전 바로가기
        if is_current:
            aliases.append(base_slug)  # statute::법명::조
        # 사람이 읽을 수 있는 형태
        aliases.append(
            f"{law_name} 제{art_num}조의{art_sub}" if art_sub else f"{law_name} 제{art_num}조"
        )
        aliases.append(
            f"{law_name}제{art_num}조의{art_sub}" if art_sub else f"{law_name}제{art_num}조"
        )

        # Frontmatter
        fm = {
            'law_name': law_name,
            'article_num': str(art_num),
            'article_key': key,
            'promulgated_at': anc_yd,
            'effective_at': ef_yd,
            'promulgation_date': promulgation_date,
            'ls_id': ls_id,
            'category': category,
        }
        if title_kw:
            fm['title_kw'] = title_kw

        # Tags
        tags = [
            ('domain:legal', 'domain'),
            ('kind:statute', 'kind'),
            (f'law:{law_name}', 'law'),
            (f'version:{promulgation_date}' if promulgation_date else 'version:unknown', 'version'),
        ]
        if is_current:
            tags.append(('status:current', 'status'))
        else:
            tags.append(('status:historical', 'status'))
        if ef_yd and len(ef_yd) >= 4:
            tags.append((f'year:{ef_yd[:4]}', 'year'))

        results.append({
            'slug': slug,
            'content': text,
            'doc_type': 'statute_article',
            'checksum': checksum,
            'original_path': str(filepath),
            'char_count': len(text),
            'now': now,
            'aliases': aliases,
            'frontmatter': fm,
            'tags': tags,
            'links': links,
        })

    # Supplements
    for sup in (raw.get('supplements') or []):
        stitle = sup.get('title', '')
        sbody = sup.get('body', '')
        if not sbody:
            continue
        # Parse promulgation number
        pno_m = re.search(r'제\s*(\d+)\s*호', stitle)
        pno = pno_m.group(1) if pno_m else ''
        if pno:
            sup_slug = f"statute::{law_name}::supplement::{pno}"
        else:
            sup_slug = f"statute::{law_name}::supplement::{hashlib.md5(stitle.encode()).hexdigest()[:8]}"

        checksum = hashlib.sha256(sbody.encode('utf-8')).hexdigest()
        results.append({
            'slug': sup_slug,
            'content': sbody,
            'doc_type': 'statute_supplement',
            'checksum': checksum,
            'original_path': str(filepath),
            'char_count': len(sbody),
            'now': now,
            'aliases': [],
            'frontmatter': {'law_name': law_name, 'supplement_title': stitle},
            'tags': [('domain:legal', 'domain'), ('kind:statute_supplement', 'kind'), (f'law:{law_name}', 'law')],
            'links': [],
        })

    return results, None


# ──────────────────────────────────────────
# Case parser
# ──────────────────────────────────────────
def split_sections(md):
    sections = {}
    cur_key = None
    cur_lines = []
    for line in md.split('\n'):
        m = re.match(r'^##\s+(.+)$', line)
        if m:
            if cur_key is not None:
                sections[cur_key] = '\n'.join(cur_lines).strip()
            cur_key = m.group(1).strip()
            cur_lines = []
        else:
            cur_lines.append(line)
    if cur_key is not None:
        sections[cur_key] = '\n'.join(cur_lines).strip()
    return sections


def parse_path_meta(filepath):
    """Extract metadata from path: {date}/{사건번호}_{code}_{kind}_{serial}_{verdict}_{court}_{name}.md"""
    p = Path(filepath)
    meta = {}
    parent = p.parent.name
    if len(parent) == 8 and parent.isdigit():
        meta['verdict_date'] = parent
    parts = p.stem.split('_')
    if len(parts) >= 6:
        meta['case_number'] = parts[0]
        meta['case_category_code'] = parts[1]
        meta['case_type_name'] = parts[2]
        meta['precedent_serial_no'] = parts[3]
        meta['verdict_type'] = parts[4]
        meta['court_name'] = parts[5]
    if len(parts) >= 7:
        meta['case_name'] = parts[6]
    return meta


def parse_case_file(filepath, content):
    """Parse case markdown → single doc dict."""
    sections = split_sections(content)
    path_meta = parse_path_meta(filepath)

    case_number = sections.get('사건번호', '').strip() or path_meta.get('case_number', '')
    if not case_number:
        return None, f"no case number: {filepath}"

    slug = case_slug(case_number)
    case_name = sections.get('사건명', '').strip() or path_meta.get('case_name', '')
    court_name = sections.get('법원명', '').strip() or path_meta.get('court_name', '')
    verdict_date = sections.get('선고일자', '').strip() or path_meta.get('verdict_date', '')
    case_type = sections.get('사건종류명', '').strip() or path_meta.get('case_type_name', '')
    serial = sections.get('판례정보일련번호', '').strip() or path_meta.get('precedent_serial_no', '')
    verdict_kind = sections.get('선고', '').strip()
    verdict_type = sections.get('판결유형', '').strip() or path_meta.get('verdict_type', '')
    court_type_code = sections.get('법원종류코드', '').strip()
    case_cat_code = sections.get('사건종류코드', '').strip() or path_meta.get('case_category_code', '')

    holding = sections.get('판시사항', '')
    summary = sections.get('판결요지', '')
    body = sections.get('판례내용', '')
    ref_statutes_text = sections.get('참조조문', '')
    ref_cases_text = sections.get('참조판례', '')

    checksum = hashlib.sha256(content.encode('utf-8')).hexdigest()
    now = int(time.time())

    # Statute citations from 참조조문 + body
    statute_cites = extract_statute_citations(ref_statutes_text)
    if body:
        statute_cites += extract_statute_citations(body)
    # Deduplicate
    seen = set()
    unique_cites = []
    for c in statute_cites:
        if c not in seen:
            seen.add(c)
            unique_cites.append(c)
    statute_cites = unique_cites

    # Case citations
    case_cites = extract_case_citations(ref_cases_text)
    if body:
        case_cites += extract_case_citations(body)
    case_cites = [c for c in set(case_cites) if c != case_number]

    # Links
    links = []
    for (law, art, sub) in statute_cites:
        target = statute_slug(law, art, sub)
        links.append(('cites', target, f"{law} 제{art}조"))
    for cc in case_cites:
        target = case_slug(cc)
        if target != slug:
            links.append(('ref-case', target, cc))

    # Aliases — case number + court-qualified + 적용법조 복합 slug
    aliases = [case_number]
    if court_name:
        aliases.append(f"{court_name} {case_number}")
    for (law, art, sub) in statute_cites:
        compact = f"{law}{article_key(art, sub)}"
        aliases.append(f"{slug}::{compact}")
        display = statute_display(law, art, sub)
        aliases.append(f"{slug}::{display}")

    # Frontmatter
    fm = {}
    for k, v in [
        ('case_number', case_number), ('case_name', case_name),
        ('court_name', court_name), ('court_type_code', court_type_code),
        ('case_category_code', case_cat_code), ('case_type_name', case_type),
        ('precedent_serial_no', serial), ('verdict_date', verdict_date),
        ('verdict_kind', verdict_kind), ('verdict_type', verdict_type),
    ]:
        if v:
            fm[k] = v

    # Tags
    tags = [('domain:legal', 'domain'), ('kind:case', 'kind')]
    if court_name:
        tags.append((f'court:{court_name}', 'court'))
    if case_type:
        tags.append((f'type:{case_type}', 'case_type'))
    if verdict_date and len(verdict_date) >= 4:
        tags.append((f'year:{verdict_date[:4]}', 'year'))
    # Applied statute tags
    for (law, art, sub) in statute_cites:
        key = article_key(art, sub)
        tags.append((f'applied::{law}::{key}', 'applied_statute'))

    return {
        'slug': slug,
        'content': content,
        'doc_type': 'case',
        'checksum': checksum,
        'original_path': str(filepath),
        'char_count': len(content),
        'now': now,
        'aliases': aliases,
        'frontmatter': fm,
        'tags': tags,
        'links': links,
    }, None


# ──────────────────────────────────────────
# Worker function (runs in child process)
# ──────────────────────────────────────────
def process_file(args):
    """Parse a single file. Returns (results_list, error_string_or_None)."""
    filepath, category = args
    try:
        content = Path(filepath).read_text(encoding='utf-8', errors='replace')
    except Exception as e:
        return [], str(e)

    if category in CASE_DIRS:
        result, err = parse_case_file(filepath, content)
        if err:
            return [], err
        return [result], None
    else:
        results, err = parse_statute_file(filepath, content, category)
        if err:
            return [], err
        return results or [], None


# ──────────────────────────────────────────
# DB writer (main thread)
# ──────────────────────────────────────────
class DBWriter:
    def __init__(self, db_path):
        self.conn = sqlite3.connect(str(db_path))
        self.conn.executescript(SCHEMA_SQL)
        # Defer index creation until after bulk insert
        self.pending_docs = []
        self.pending_aliases = []
        self.pending_fm = []
        self.pending_tags = []
        self.pending_links = []
        self.doc_count = 0
        self.alias_count = 0
        self.link_count = 0
        self.tag_count = 0
        self.slug_to_id = {}  # for link resolution

    def add(self, doc):
        uid = uuid.uuid4().hex
        self.pending_docs.append((
            uid, doc['slug'], doc['content'], 'local_file', 'local',
            doc['original_path'], doc['checksum'], doc['doc_type'],
            doc['char_count'], doc['now'], doc['now']
        ))
        # We'll resolve doc_id after insert
        self.pending_aliases.append(doc.get('aliases', []))
        self.pending_fm.append(doc.get('frontmatter', {}))
        self.pending_tags.append(doc.get('tags', []))
        self.pending_links.append(doc.get('links', []))

        if len(self.pending_docs) >= BATCH_SIZE:
            self.flush()

    def flush(self):
        if not self.pending_docs:
            return

        cur = self.conn.cursor()
        cur.execute("BEGIN TRANSACTION")

        # Insert documents
        cur.executemany("""
            INSERT INTO vault_documents
                (uuid, title, content, source_type, source_device_id,
                 original_path, checksum, doc_type, char_count, created_at, updated_at)
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        """, self.pending_docs)

        # Get the auto-incremented IDs
        # last_insert_rowid gives the LAST id; we inserted len(pending) rows
        last_id = cur.execute("SELECT last_insert_rowid()").fetchone()[0]
        first_id = last_id - len(self.pending_docs) + 1
        ids = list(range(first_id, last_id + 1))

        # Map slugs to IDs
        for i, doc_tuple in enumerate(self.pending_docs):
            slug = doc_tuple[1]  # title = slug
            self.slug_to_id[slug] = ids[i]

        # Aliases
        alias_rows = []
        for i, aliases in enumerate(self.pending_aliases):
            for alias in aliases:
                alias_rows.append((ids[i], alias))
        if alias_rows:
            cur.executemany(
                "INSERT OR IGNORE INTO vault_aliases (doc_id, alias) VALUES (?, ?)",
                alias_rows
            )
            self.alias_count += len(alias_rows)

        # Frontmatter
        fm_rows = []
        for i, fm in enumerate(self.pending_fm):
            for k, v in fm.items():
                if v:
                    fm_rows.append((ids[i], k, str(v)))
        if fm_rows:
            cur.executemany(
                "INSERT OR IGNORE INTO vault_frontmatter (doc_id, key, value) VALUES (?, ?, ?)",
                fm_rows
            )

        # Tags
        tag_rows = []
        for i, tags in enumerate(self.pending_tags):
            for tag_name, tag_type in tags:
                tag_rows.append((ids[i], tag_name, tag_type))
        if tag_rows:
            cur.executemany(
                "INSERT OR IGNORE INTO vault_tags (doc_id, tag_name, tag_type) VALUES (?, ?, ?)",
                tag_rows
            )
            self.tag_count += len(tag_rows)

        # Links (target_doc_id resolved later)
        link_rows = []
        for i, links in enumerate(self.pending_links):
            for (relation, target_slug, evidence) in links:
                link_rows.append((ids[i], target_slug, relation, evidence))
        if link_rows:
            cur.executemany(
                "INSERT INTO vault_links (source_doc_id, target_raw, display_text, context) VALUES (?, ?, ?, ?)",
                link_rows
            )
            self.link_count += len(link_rows)

        cur.execute("COMMIT")
        self.doc_count += len(self.pending_docs)

        self.pending_docs.clear()
        self.pending_aliases.clear()
        self.pending_fm.clear()
        self.pending_tags.clear()
        self.pending_links.clear()

    def resolve_links(self):
        """Resolve target_doc_id for links where target exists."""
        print("  Resolving links...")
        cur = self.conn.cursor()
        cur.execute("""
            UPDATE vault_links SET
                target_doc_id = (
                    SELECT id FROM vault_documents WHERE title = vault_links.target_raw LIMIT 1
                ),
                is_resolved = 1
            WHERE EXISTS (
                SELECT 1 FROM vault_documents WHERE title = vault_links.target_raw
            )
        """)
        resolved = cur.execute("SELECT changes()").fetchone()[0]
        self.conn.commit()
        total = cur.execute("SELECT COUNT(*) FROM vault_links").fetchone()[0]
        print(f"    Resolved: {resolved:,} / {total:,}")

    def create_indexes(self):
        """Create indexes after bulk insert for speed."""
        print("  Creating indexes...")
        self.conn.executescript(INDEX_SQL)

    def optimize(self):
        print("  Optimizing...")
        self.conn.execute("ANALYZE")
        self.conn.execute("PRAGMA optimize")
        self.conn.commit()

    def stats(self):
        cur = self.conn.cursor()
        rows = cur.execute("""
            SELECT doc_type, COUNT(*) FROM vault_documents GROUP BY doc_type ORDER BY COUNT(*) DESC
        """).fetchall()
        for doc_type, cnt in rows:
            print(f"    {doc_type}: {cnt:,}")
        total = cur.execute("SELECT COUNT(*) FROM vault_documents").fetchone()[0]
        links = cur.execute("SELECT COUNT(*) FROM vault_links").fetchone()[0]
        aliases = cur.execute("SELECT COUNT(*) FROM vault_aliases").fetchone()[0]
        tags = cur.execute("SELECT COUNT(*) FROM vault_tags").fetchone()[0]
        fm = cur.execute("SELECT COUNT(*) FROM vault_frontmatter").fetchone()[0]
        print(f"    ── Total docs: {total:,}")
        print(f"    ── Links: {links:,}")
        print(f"    ── Aliases: {aliases:,}")
        print(f"    ── Tags: {tags:,}")
        print(f"    ── Frontmatter: {fm:,}")

    def close(self):
        self.conn.close()


# ──────────────────────────────────────────
# File collector
# ──────────────────────────────────────────
def collect_files(corpus_dir):
    """Collect all (filepath, category) pairs."""
    all_files = []
    for category_dir in sorted(corpus_dir.iterdir()):
        if not category_dir.is_dir():
            continue
        category = category_dir.name
        if category not in CASE_DIRS and category not in STATUTE_DIRS:
            continue
        count = 0
        for date_dir in sorted(category_dir.iterdir()):
            if not date_dir.is_dir():
                continue
            for md_file in date_dir.glob('*.md'):
                all_files.append((str(md_file), category))
                count += 1
        print(f"  {category}: {count:,} files")
    return all_files


# ──────────────────────────────────────────
# Main
# ──────────────────────────────────────────
def main():
    parser = argparse.ArgumentParser(description="Fast parallel domain.db builder")
    parser.add_argument('--corpus', type=str, default=str(CORPUS_DIR))
    parser.add_argument('--output', type=str, default=str(OUTPUT_DB))
    parser.add_argument('--workers', type=int, default=NUM_WORKERS)
    parser.add_argument('--batch', type=int, default=BATCH_SIZE)
    args = parser.parse_args()

    corpus = Path(args.corpus)
    output = Path(args.output)

    if output.exists():
        output.unlink()
        print(f"Removed existing {output}")

    t0 = time.time()
    print(f"\n{'='*60}")
    print(f"  Fast Parallel domain.db Builder")
    print(f"  Corpus: {corpus}")
    print(f"  Output: {output}")
    print(f"  Workers: {args.workers}")
    print(f"{'='*60}\n")

    # Collect files
    print("[1/6] Scanning files...")
    all_files = collect_files(corpus)
    total = len(all_files)
    print(f"  Total: {total:,} files\n")

    # Initialize DB
    print("[2/6] Creating database...")
    writer = DBWriter(output)
    print("  Schema created.\n")

    # Process files in parallel
    print(f"[3/6] Processing files ({args.workers} workers)...")
    processed = 0
    errors = 0
    error_files = []

    with ProcessPoolExecutor(max_workers=args.workers) as executor:
        # Submit in chunks to avoid memory explosion
        chunk_size = 10000
        for chunk_start in range(0, total, chunk_size):
            chunk = all_files[chunk_start:chunk_start + chunk_size]
            futures = {executor.submit(process_file, f): f for f in chunk}

            for future in as_completed(futures):
                try:
                    results, err = future.result()
                    if err:
                        errors += 1
                        if errors <= 20:
                            error_files.append(err)
                    else:
                        for doc in results:
                            writer.add(doc)
                except Exception as e:
                    errors += 1
                    if errors <= 20:
                        error_files.append(str(e))

                processed += 1
                if processed % 10000 == 0:
                    writer.flush()
                    elapsed = time.time() - t0
                    rate = processed / elapsed
                    eta = (total - processed) / rate if rate > 0 else 0
                    print(f"  {processed:,}/{total:,} ({processed*100//total}%) "
                          f"docs={writer.doc_count:,} "
                          f"{rate:.0f} files/s ETA {eta:.0f}s")

    writer.flush()

    print(f"\n[4/6] Bulk insert complete.")
    print(f"  Documents: {writer.doc_count:,}")
    print(f"  Files processed: {processed:,}")
    print(f"  Errors: {errors:,}")
    if error_files:
        print(f"  Sample errors:")
        for e in error_files[:10]:
            print(f"    - {e}")

    # Create indexes (after bulk insert = much faster)
    print(f"\n[5/6] Post-processing...")
    writer.create_indexes()
    writer.resolve_links()
    writer.optimize()

    # Stats
    print(f"\n[6/6] Final statistics:")
    writer.stats()

    writer.close()

    db_size = output.stat().st_size / (1024 * 1024)
    elapsed = time.time() - t0
    print(f"\n{'='*60}")
    print(f"  DONE in {elapsed:.1f}s ({elapsed/60:.1f} min)")
    print(f"  domain.db: {db_size:.1f} MB")
    print(f"  Location: {output}")
    print(f"{'='*60}")


if __name__ == '__main__':
    main()
