#!/usr/bin/env python3
"""
legal_graph_viz.py - domain.db 법률 그래프 시각화 도구

3가지 시각화 모드:
  1. graph   - D3.js 인터랙티브 포스 그래프 (HTML)
  2. dashboard - 쟁점 분석 대시보드 (HTML)
  3. mermaid  - Mermaid 다이어그램 + JSON 내보내기

Usage:
  python legal_graph_viz.py graph "사해행위취소" --output graph.html
  python legal_graph_viz.py dashboard "case::2022다242649" --output dash.html
  python legal_graph_viz.py mermaid "statute::민법::750" --output diagram.md
"""

import argparse
import json
import sqlite3
import time
import html as html_mod
from pathlib import Path
from collections import defaultdict

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


# ──────────────────────────────────────────
# Data extraction
# ──────────────────────────────────────────
def get_conn():
    conn = sqlite3.connect(f'file:{DB_PATH}?mode=ro', uri=True)
    conn.execute('PRAGMA cache_size = -128000')
    conn.execute('PRAGMA mmap_size = 536870912')
    return conn


def search_cases_by_keyword(conn, keyword, limit=50):
    """FTS5 search for cases matching keyword."""
    rows = conn.execute("""
        SELECT d.title, d.id FROM vault_docs_fts fts
        JOIN vault_documents d ON d.id = fts.rowid
        WHERE vault_docs_fts MATCH ? AND d.doc_type = 'case'
        LIMIT ?
    """, (keyword, limit)).fetchall()
    return [(r[0], r[1]) for r in rows]


def get_case_meta(conn, case_slug):
    """Get case metadata from frontmatter."""
    row = conn.execute(
        "SELECT id FROM vault_documents WHERE title = ?", (case_slug,)
    ).fetchone()
    if not row:
        return None
    doc_id = row[0]
    fm = dict(conn.execute(
        "SELECT key, value FROM vault_frontmatter WHERE doc_id = ?", (doc_id,)
    ).fetchall())
    fm['doc_id'] = doc_id
    fm['slug'] = case_slug
    return fm


def get_statute_meta(conn, statute_slug):
    """Get statute metadata."""
    # Try alias first (for base slugs without @date)
    row = conn.execute("""
        SELECT d.id, d.title FROM vault_aliases a
        JOIN vault_documents d ON d.id = a.doc_id
        WHERE a.alias = ? LIMIT 1
    """, (statute_slug,)).fetchone()
    if not row:
        row = conn.execute(
            "SELECT id, title FROM vault_documents WHERE title = ? LIMIT 1",
            (statute_slug,)
        ).fetchone()
    if not row:
        return None
    doc_id, actual_slug = row
    fm = dict(conn.execute(
        "SELECT key, value FROM vault_frontmatter WHERE doc_id = ?", (doc_id,)
    ).fetchall())
    fm['doc_id'] = doc_id
    fm['slug'] = actual_slug
    return fm


def case_cited_statutes(conn, case_slug):
    """Get statutes cited by a case."""
    return conn.execute("""
        SELECT DISTINCT l.target_raw FROM vault_links l
        JOIN vault_documents d ON d.id = l.source_doc_id
        WHERE d.title = ? AND l.display_text = 'cites'
    """, (case_slug,)).fetchall()


def statute_citing_cases(conn, statute_slug, limit=200):
    """Get cases citing a statute (base slug match)."""
    return conn.execute("""
        SELECT DISTINCT d.title FROM vault_links l
        JOIN vault_documents d ON d.id = l.source_doc_id
        WHERE l.target_raw = ? AND l.display_text = 'cites' AND d.doc_type = 'case'
        LIMIT ?
    """, (statute_slug, limit)).fetchall()


def build_issue_graph(conn, seed_slugs, max_hops=2, max_cases=500):
    """Build the issue analysis graph data."""
    statute_to_cases = defaultdict(set)
    case_to_statutes = defaultdict(set)
    all_cases = set(seed_slugs)
    frontier = set(seed_slugs)

    for hop in range(max_hops):
        new_statutes = set()
        for cs in frontier:
            for (s,) in case_cited_statutes(conn, cs):
                statute_to_cases[s].add(cs)
                case_to_statutes[cs].add(s)
                new_statutes.add(s)

        next_frontier = set()
        for ss in new_statutes:
            for (c,) in statute_citing_cases(conn, ss, limit=max_cases):
                statute_to_cases[ss].add(c)
                case_to_statutes[c].add(ss)
                if c not in all_cases:
                    next_frontier.add(c)
                    all_cases.add(c)
                if len(all_cases) >= max_cases:
                    break
            if len(all_cases) >= max_cases:
                break
        frontier = next_frontier
        if not frontier or len(all_cases) >= max_cases:
            break

    # Score statutes
    total = max(len(all_cases), 1)
    scored_statutes = []
    for slug, cases in statute_to_cases.items():
        raw_c = len(cases) / total
        centrality = raw_c * 0.1 if slug in BOILERPLATE else raw_c
        is_bp = slug in BOILERPLATE
        scored_statutes.append({
            'slug': slug,
            'label': slug.replace('statute::', '').replace('::', ' 제') + '조',
            'citing_count': len(cases),
            'centrality': centrality,
            'is_boilerplate': is_bp,
        })
    scored_statutes.sort(key=lambda x: -x['centrality'])

    max_c = max((s['centrality'] for s in scored_statutes if not s['is_boilerplate']), default=1)
    for s in scored_statutes:
        if s['is_boilerplate']:
            s['tier'] = 'boilerplate'
        elif max_c > 0:
            ratio = s['centrality'] / max_c
            s['tier'] = 'core' if ratio >= 0.5 else ('supporting' if ratio >= 0.15 else 'peripheral')
        else:
            s['tier'] = 'peripheral'

    # Score cases
    c_map = {s['slug']: s['centrality'] for s in scored_statutes}
    scored_cases = []
    for cs in all_cases:
        statutes = case_to_statutes.get(cs, set())
        score = sum(c_map.get(s, 0) for s in statutes if s not in BOILERPLATE)
        issue_count = sum(1 for s in statutes if s not in BOILERPLATE)
        meta = get_case_meta(conn, cs) or {}
        scored_cases.append({
            'slug': cs,
            'case_number': meta.get('case_number', cs.replace('case::', '')),
            'case_name': meta.get('case_name', ''),
            'court': meta.get('court_name', ''),
            'date': meta.get('verdict_date', ''),
            'leading_score': score,
            'issue_count': issue_count,
        })
    scored_cases.sort(key=lambda x: (-x['leading_score'], x['date'] or ''))

    # Edges
    edges = []
    for cs, statutes in case_to_statutes.items():
        for s in statutes:
            edges.append({'source': cs, 'target': s, 'type': 'cites'})

    return {
        'statutes': scored_statutes,
        'cases': scored_cases,
        'edges': edges,
        'total_cases': len(all_cases),
        'total_statutes': len(statute_to_cases),
    }


# ──────────────────────────────────────────
# 1. D3.js Interactive Force Graph
# ──────────────────────────────────────────
D3_TEMPLATE = """<!DOCTYPE html>
<html lang="ko">
<head>
<meta charset="UTF-8">
<title>Legal Issue Graph - {title}</title>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: 'Pretendard', -apple-system, sans-serif; background: #0d1117; color: #c9d1d9; overflow: hidden; }
#container { width: 100vw; height: 100vh; position: relative; }
svg { width: 100%; height: 100%; }
#info-panel {
    position: absolute; top: 10px; right: 10px; width: 360px; max-height: 90vh;
    background: #161b22; border: 1px solid #30363d; border-radius: 8px;
    padding: 16px; overflow-y: auto; font-size: 13px; display: none;
}
#info-panel.visible { display: block; }
#info-panel h3 { color: #58a6ff; margin-bottom: 8px; font-size: 15px; }
#info-panel .meta { color: #8b949e; margin-bottom: 4px; }
#info-panel .tier { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; }
.tier-core { background: #f0883e30; color: #f0883e; }
.tier-supporting { background: #58a6ff30; color: #58a6ff; }
.tier-peripheral { background: #8b949e30; color: #8b949e; }
.tier-boilerplate { background: #30363d; color: #484f58; }
#legend {
    position: absolute; bottom: 10px; left: 10px; background: #161b22;
    border: 1px solid #30363d; border-radius: 8px; padding: 12px; font-size: 12px;
}
#legend div { margin: 4px 0; display: flex; align-items: center; gap: 8px; }
#legend .dot { width: 12px; height: 12px; border-radius: 50%; }
#stats {
    position: absolute; top: 10px; left: 10px; background: #161b22;
    border: 1px solid #30363d; border-radius: 8px; padding: 12px; font-size: 12px;
}
#search-box {
    position: absolute; top: 10px; left: 50%; transform: translateX(-50%);
    background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 8px 16px;
}
#search-box input {
    background: #0d1117; border: 1px solid #30363d; color: #c9d1d9;
    padding: 6px 12px; border-radius: 4px; width: 300px; font-size: 13px;
}
.link { stroke-opacity: 0.3; }
.link:hover { stroke-opacity: 0.8; }
.node text { font-size: 10px; fill: #c9d1d9; pointer-events: none; }
</style>
</head>
<body>
<div id="container">
    <svg id="graph"></svg>
    <div id="stats"></div>
    <div id="search-box"><input type="text" id="search" placeholder="노드 검색 (법명 또는 사건번호)..." /></div>
    <div id="legend">
        <div><span class="dot" style="background:#f0883e"></span> Core 쟁점 법조</div>
        <div><span class="dot" style="background:#58a6ff"></span> Supporting 법조</div>
        <div><span class="dot" style="background:#8b949e"></span> Peripheral 법조</div>
        <div><span class="dot" style="background:#484f58"></span> Boilerplate (형식적)</div>
        <div><span class="dot" style="background:#3fb950"></span> Leading 판례</div>
        <div><span class="dot" style="background:#388bfd"></span> 일반 판례</div>
    </div>
    <div id="info-panel"></div>
</div>
<script src="https://d3js.org/d3.v7.min.js"></script>
<script>
const DATA = {graph_json};

const TIER_COLORS = {{
    core: '#f0883e', supporting: '#58a6ff', peripheral: '#8b949e', boilerplate: '#484f58'
}};

const width = window.innerWidth;
const height = window.innerHeight;

// Build nodes
const nodes = [];
const nodeMap = {{}};
DATA.statutes.forEach(s => {{
    const n = {{ id: s.slug, label: s.label, type: 'statute', tier: s.tier, centrality: s.centrality, citing_count: s.citing_count, r: 6 + Math.sqrt(s.citing_count) * 2 }};
    nodes.push(n);
    nodeMap[s.slug] = n;
}});
const maxScore = Math.max(...DATA.cases.map(c => c.leading_score), 0.001);
DATA.cases.slice(0, 200).forEach(c => {{
    const isLeading = c.leading_score > maxScore * 0.5;
    const n = {{ id: c.slug, label: c.case_number || c.slug.replace('case::',''), type: 'case', case_name: c.case_name, court: c.court, date: c.date, leading_score: c.leading_score, issue_count: c.issue_count, r: isLeading ? 10 : 5, isLeading }};
    nodes.push(n);
    nodeMap[c.slug] = n;
}});

// Build links
const links = [];
DATA.edges.forEach(e => {{
    if (nodeMap[e.source] && nodeMap[e.target]) {{
        links.push({{ source: e.source, target: e.target, type: e.type }});
    }}
}});

// Stats
document.getElementById('stats').innerHTML =
    `<b>Nodes:</b> ${{nodes.length}} (${{DATA.total_statutes}} statutes, ${{Math.min(DATA.total_cases, 200)}} cases)<br>` +
    `<b>Edges:</b> ${{links.length}}`;

// D3 Force
const svg = d3.select('#graph');
const g = svg.append('g');

svg.call(d3.zoom().scaleExtent([0.1, 5]).on('zoom', e => g.attr('transform', e.transform)));

const sim = d3.forceSimulation(nodes)
    .force('link', d3.forceLink(links).id(d => d.id).distance(80))
    .force('charge', d3.forceManyBody().strength(-120))
    .force('center', d3.forceCenter(width/2, height/2))
    .force('collision', d3.forceCollide().radius(d => d.r + 2));

const link = g.append('g').selectAll('line')
    .data(links).join('line')
    .attr('class', 'link')
    .attr('stroke', d => d.type === 'cites' ? '#58a6ff' : '#f0883e')
    .attr('stroke-width', 1);

const node = g.append('g').selectAll('g')
    .data(nodes).join('g')
    .call(d3.drag().on('start', dragstart).on('drag', dragged).on('end', dragend));

node.append('circle')
    .attr('r', d => d.r)
    .attr('fill', d => {{
        if (d.type === 'case') return d.isLeading ? '#3fb950' : '#388bfd';
        return TIER_COLORS[d.tier] || '#8b949e';
    }})
    .attr('stroke', '#0d1117').attr('stroke-width', 1.5)
    .style('cursor', 'pointer');

node.append('text')
    .attr('dx', d => d.r + 4).attr('dy', 4)
    .text(d => d.label.length > 25 ? d.label.slice(0,25)+'...' : d.label);

node.on('click', (e, d) => showInfo(d));

// Info panel
function showInfo(d) {{
    const panel = document.getElementById('info-panel');
    panel.classList.add('visible');
    if (d.type === 'statute') {{
        panel.innerHTML = `
            <h3>${{d.label}}</h3>
            <span class="tier tier-${{d.tier}}">${{d.tier.toUpperCase()}}</span>
            <p class="meta">Slug: ${{d.id}}</p>
            <p class="meta">Citing cases: ${{d.citing_count}}</p>
            <p class="meta">Centrality: ${{(d.centrality*100).toFixed(1)}}%</p>
        `;
    }} else {{
        panel.innerHTML = `
            <h3>${{d.label}}</h3>
            ${{d.isLeading ? '<span class="tier tier-core">LEADING CASE</span>' : ''}}
            <p class="meta">Slug: ${{d.id}}</p>
            <p class="meta">${{d.court || ''}} ${{d.date || ''}}</p>
            <p class="meta">${{d.case_name || ''}}</p>
            <p class="meta">Issue statutes: ${{d.issue_count}}</p>
            <p class="meta">Leading score: ${{d.leading_score?.toFixed(3) || ''}}</p>
        `;
    }}
}}

// Search
document.getElementById('search').addEventListener('input', e => {{
    const q = e.target.value.toLowerCase();
    node.style('opacity', d => (!q || d.label.toLowerCase().includes(q) || d.id.toLowerCase().includes(q)) ? 1 : 0.1);
    link.style('opacity', !q ? 0.3 : 0.05);
}});

sim.on('tick', () => {{
    link.attr('x1', d => d.source.x).attr('y1', d => d.source.y).attr('x2', d => d.target.x).attr('y2', d => d.target.y);
    node.attr('transform', d => `translate(${{d.x}},${{d.y}})`);
}});

function dragstart(e, d) {{ if (!e.active) sim.alphaTarget(0.3).restart(); d.fx = d.x; d.fy = d.y; }}
function dragged(e, d) {{ d.fx = e.x; d.fy = e.y; }}
function dragend(e, d) {{ if (!e.active) sim.alphaTarget(0); d.fx = null; d.fy = null; }}
</script>
</body>
</html>"""


# ──────────────────────────────────────────
# 2. Issue Analysis Dashboard
# ──────────────────────────────────────────
DASHBOARD_TEMPLATE = """<!DOCTYPE html>
<html lang="ko">
<head>
<meta charset="UTF-8">
<title>Issue Analysis Dashboard - {title}</title>
<style>
* {{ margin: 0; padding: 0; box-sizing: border-box; }}
body {{ font-family: 'Pretendard', -apple-system, sans-serif; background: #0d1117; color: #c9d1d9; padding: 24px; }}
h1 {{ color: #58a6ff; margin-bottom: 8px; }}
h2 {{ color: #f0883e; margin: 24px 0 12px; border-bottom: 1px solid #30363d; padding-bottom: 8px; }}
.subtitle {{ color: #8b949e; margin-bottom: 24px; }}
.grid {{ display: grid; grid-template-columns: 1fr 1fr; gap: 24px; margin-bottom: 24px; }}
.card {{ background: #161b22; border: 1px solid #30363d; border-radius: 8px; padding: 16px; }}
.stat-num {{ font-size: 32px; font-weight: 700; color: #58a6ff; }}
.stat-label {{ font-size: 13px; color: #8b949e; }}
table {{ width: 100%; border-collapse: collapse; }}
th {{ text-align: left; padding: 8px 12px; background: #161b22; border-bottom: 2px solid #30363d; color: #58a6ff; font-size: 13px; }}
td {{ padding: 8px 12px; border-bottom: 1px solid #21262d; font-size: 13px; }}
tr:hover {{ background: #161b22; }}
.tier {{ display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 11px; font-weight: 600; }}
.tier-core {{ background: #f0883e30; color: #f0883e; }}
.tier-supporting {{ background: #58a6ff30; color: #58a6ff; }}
.tier-peripheral {{ background: #8b949e30; color: #8b949e; }}
.tier-boilerplate {{ background: #30363d; color: #484f58; }}
.bar {{ height: 20px; border-radius: 4px; display: inline-block; min-width: 2px; }}
.leading {{ color: #3fb950; font-weight: 600; }}
input[type=text] {{ background: #0d1117; border: 1px solid #30363d; color: #c9d1d9; padding: 8px 12px; border-radius: 4px; width: 100%; margin-bottom: 12px; }}
</style>
</head>
<body>
<h1>Issue Analysis Dashboard</h1>
<p class="subtitle">{subtitle}</p>

<div class="grid">
    <div class="card"><span class="stat-num">{total_cases}</span><br><span class="stat-label">Total Cases</span></div>
    <div class="card"><span class="stat-num">{total_statutes}</span><br><span class="stat-label">Statutes Found</span></div>
    <div class="card"><span class="stat-num">{core_count}</span><br><span class="stat-label">Core Issue Statutes</span></div>
    <div class="card"><span class="stat-num">{leading_count}</span><br><span class="stat-label">Leading Case Candidates</span></div>
</div>

<h2>Core Issue Statutes (핵심 쟁점 적용법조)</h2>
<table>
<tr><th>Tier</th><th>Statute</th><th>Citing Cases</th><th>Centrality</th><th></th></tr>
{statute_rows}
</table>

<h2>Leading Cases (리딩 판결)</h2>
<input type="text" id="case-filter" placeholder="판례 검색..." oninput="filterCases(this.value)" />
<table id="case-table">
<tr><th>#</th><th>Case</th><th>Court</th><th>Date</th><th>Issue Statutes</th><th>Score</th></tr>
{case_rows}
</table>

<h2>Supplementary Issues (부수적 쟁점)</h2>
<p style="color:#8b949e; margin-bottom:12px;">변호사의 추가 공격방어방법 탐색 후보</p>
<table>
<tr><th>Tier</th><th>Statute</th><th>Citing Cases</th><th>Centrality</th></tr>
{peripheral_rows}
</table>

<script>
function filterCases(q) {{
    const rows = document.querySelectorAll('#case-table tr');
    rows.forEach((r, i) => {{
        if (i === 0) return;
        r.style.display = (!q || r.textContent.toLowerCase().includes(q.toLowerCase())) ? '' : 'none';
    }});
}}
</script>
</body>
</html>"""


# ──────────────────────────────────────────
# 3. Mermaid Diagram
# ──────────────────────────────────────────
def generate_mermaid(data, title):
    lines = ['```mermaid', 'graph LR']
    lines.append(f'    %% Issue Graph: {title}')
    lines.append('')

    # Statute nodes
    for s in data['statutes'][:30]:
        safe_id = s['slug'].replace('::', '_').replace('-', '_').replace('@', '_')
        label = s['label']
        if s['tier'] == 'core':
            lines.append(f'    {safe_id}["{label}<br/>Core"]:::core')
        elif s['tier'] == 'supporting':
            lines.append(f'    {safe_id}["{label}<br/>Supporting"]:::supporting')
        elif s['tier'] == 'boilerplate':
            lines.append(f'    {safe_id}["{label}"]:::boilerplate')
        else:
            lines.append(f'    {safe_id}["{label}"]:::peripheral')

    lines.append('')

    # Case nodes (top 20)
    for c in data['cases'][:20]:
        safe_id = c['slug'].replace('::', '_').replace('-', '_').replace('@', '_')
        label = c['case_number']
        if c['leading_score'] > 0 and data['cases'][0]['leading_score'] > 0:
            if c['leading_score'] >= data['cases'][0]['leading_score'] * 0.5:
                lines.append(f'    {safe_id}(("{label}<br/>Leading")):::leading')
            else:
                lines.append(f'    {safe_id}("{label}"):::case')
        else:
            lines.append(f'    {safe_id}("{label}"):::case')

    lines.append('')

    # Edges (limited)
    edge_count = 0
    slug_set = set(s['slug'] for s in data['statutes'][:30]) | set(c['slug'] for c in data['cases'][:20])
    for e in data['edges']:
        if e['source'] in slug_set and e['target'] in slug_set:
            src = e['source'].replace('::', '_').replace('-', '_').replace('@', '_')
            tgt = e['target'].replace('::', '_').replace('-', '_').replace('@', '_')
            lines.append(f'    {src} --> {tgt}')
            edge_count += 1
            if edge_count > 100:
                break

    lines.append('')
    lines.append('    classDef core fill:#f0883e,stroke:#f0883e,color:#000')
    lines.append('    classDef supporting fill:#58a6ff,stroke:#58a6ff,color:#000')
    lines.append('    classDef peripheral fill:#8b949e,stroke:#8b949e,color:#000')
    lines.append('    classDef boilerplate fill:#484f58,stroke:#484f58,color:#fff')
    lines.append('    classDef leading fill:#3fb950,stroke:#3fb950,color:#000')
    lines.append('    classDef case fill:#388bfd,stroke:#388bfd,color:#000')
    lines.append('```')
    return '\n'.join(lines)


# ──────────────────────────────────────────
# Generators
# ──────────────────────────────────────────
def gen_graph_html(data, title, output):
    graph_json = json.dumps(data, ensure_ascii=False)
    html = D3_TEMPLATE.replace('{title}', title).replace('{graph_json}', graph_json)
    Path(output).write_text(html, encoding='utf-8')
    print(f"  Graph saved: {output}")


def gen_dashboard_html(data, title, output):
    # Statute rows
    max_c = max((s['centrality'] for s in data['statutes']), default=1)
    statute_rows = []
    peripheral_rows = []
    for s in data['statutes']:
        bar_width = int(s['centrality'] / max(max_c, 0.001) * 200)
        tier_class = f"tier-{s['tier']}"
        color = {'core': '#f0883e', 'supporting': '#58a6ff', 'peripheral': '#8b949e', 'boilerplate': '#484f58'}[s['tier']]
        row = f"""<tr>
            <td><span class="tier {tier_class}">{s['tier'].upper()}</span></td>
            <td>{html_mod.escape(s['label'])}</td>
            <td>{s['citing_count']}</td>
            <td>{s['centrality']*100:.1f}% <span class="bar" style="background:{color};width:{bar_width}px"></span></td>
        </tr>"""
        if s['tier'] in ('core', 'supporting'):
            statute_rows.append(row)
        elif s['tier'] == 'peripheral':
            peripheral_rows.append(row)

    # Case rows
    case_rows = []
    max_score = data['cases'][0]['leading_score'] if data['cases'] else 1
    for i, c in enumerate(data['cases'][:100]):
        is_leading = c['leading_score'] >= max_score * 0.5 if max_score > 0 else False
        cls = ' class="leading"' if is_leading else ''
        case_rows.append(f"""<tr{cls}>
            <td>{i+1}</td>
            <td>{html_mod.escape(c['case_number'])} {html_mod.escape(c.get('case_name',''))}</td>
            <td>{html_mod.escape(c.get('court',''))}</td>
            <td>{c.get('date','')}</td>
            <td>{c['issue_count']}</td>
            <td>{c['leading_score']:.3f}</td>
        </tr>""")

    core_count = sum(1 for s in data['statutes'] if s['tier'] == 'core')
    leading_count = sum(1 for c in data['cases'] if max_score > 0 and c['leading_score'] >= max_score * 0.5)

    html = DASHBOARD_TEMPLATE.format(
        title=title,
        subtitle=f"Seed: {title} | {data['total_cases']} cases, {data['total_statutes']} statutes",
        total_cases=data['total_cases'],
        total_statutes=data['total_statutes'],
        core_count=core_count,
        leading_count=leading_count,
        statute_rows='\n'.join(statute_rows),
        case_rows='\n'.join(case_rows),
        peripheral_rows='\n'.join(peripheral_rows[:30]),
    )
    Path(output).write_text(html, encoding='utf-8')
    print(f"  Dashboard saved: {output}")


def gen_mermaid(data, title, output):
    md = f"# Issue Graph: {title}\n\n"
    md += generate_mermaid(data, title)
    md += f"\n\n## JSON Data\n\n```json\n{json.dumps(data, ensure_ascii=False, indent=2)[:10000]}\n...\n```\n"
    Path(output).write_text(md, encoding='utf-8')
    print(f"  Mermaid saved: {output}")


# ──────────────────────────────────────────
# Main
# ──────────────────────────────────────────
def main():
    parser = argparse.ArgumentParser(description='Legal Graph Visualization')
    parser.add_argument('mode', choices=['graph', 'dashboard', 'mermaid', 'all'])
    parser.add_argument('query', help='Keyword, case slug, or statute slug')
    parser.add_argument('--output', '-o', default=None)
    parser.add_argument('--hops', type=int, default=2)
    parser.add_argument('--max-cases', type=int, default=300)
    args = parser.parse_args()

    conn = get_conn()
    t0 = time.time()

    # Determine seed cases
    query = args.query
    if query.startswith('case::'):
        seed_slugs = [query]
        title = query
    elif query.startswith('statute::'):
        cases = statute_citing_cases(conn, query, limit=50)
        seed_slugs = [c[0] for c in cases]
        title = query
    else:
        results = search_cases_by_keyword(conn, query, limit=30)
        seed_slugs = [r[0] for r in results]
        title = query

    if not seed_slugs:
        print(f"No cases found for: {query}")
        return

    print(f"Seeds: {len(seed_slugs)} cases")
    print(f"Building issue graph (hops={args.hops}, max_cases={args.max_cases})...")
    data = build_issue_graph(conn, seed_slugs, max_hops=args.hops, max_cases=args.max_cases)
    print(f"  Graph built in {time.time()-t0:.1f}s")
    print(f"  Statutes: {data['total_statutes']}, Cases: {data['total_cases']}, Edges: {len(data['edges'])}")

    out_dir = Path(args.output).parent if args.output else Path('.')
    base = Path(args.output).stem if args.output else query.replace('::', '_').replace(' ', '_')

    if args.mode in ('graph', 'all'):
        gen_graph_html(data, title, str(out_dir / f"{base}_graph.html"))
    if args.mode in ('dashboard', 'all'):
        gen_dashboard_html(data, title, str(out_dir / f"{base}_dashboard.html"))
    if args.mode in ('mermaid', 'all'):
        gen_mermaid(data, title, str(out_dir / f"{base}_mermaid.md"))

    conn.close()
    print(f"\nDone in {time.time()-t0:.1f}s")


if __name__ == '__main__':
    main()
