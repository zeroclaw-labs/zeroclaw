#!/usr/bin/env python3
"""
PyMuPDF-based PDF to HTML/Markdown converter for MoA.

Converts digital (text-based) PDFs to HTML and Markdown using pymupdf4llm,
which leverages PyMuPDF (fitz) for high-quality text extraction with layout
preservation.

Usage:
    python pymupdf_convert.py <input_pdf> [--output-dir <dir>] [--format html|markdown|both]

Output (JSON to stdout):
    {
        "success": true,
        "html": "...",
        "markdown": "...",
        "page_count": 5,
        "engine": "pymupdf4llm"
    }

Requirements:
    pip install pymupdf4llm
    (pymupdf4llm automatically installs PyMuPDF/fitz as a dependency)
"""

import json
import sys
import os
from pathlib import Path


def convert_pdf(input_path: str, output_dir: str | None = None, fmt: str = "both") -> dict:
    """Convert a PDF file to HTML and/or Markdown using pymupdf4llm."""
    try:
        import pymupdf4llm
        import pymupdf  # fitz
    except ImportError:
        return {
            "success": False,
            "error": "pymupdf4llm not installed. Run: pip install pymupdf4llm",
            "html": "",
            "markdown": "",
            "page_count": 0,
            "engine": "pymupdf4llm",
        }

    if not os.path.exists(input_path):
        return {
            "success": False,
            "error": f"File not found: {input_path}",
            "html": "",
            "markdown": "",
            "page_count": 0,
            "engine": "pymupdf4llm",
        }

    try:
        # Get page count
        doc = pymupdf.open(input_path)
        page_count = len(doc)
        doc.close()

        # Extract markdown using pymupdf4llm (high quality, layout-aware)
        markdown = pymupdf4llm.to_markdown(
            input_path,
            show_progress=False,
            page_chunks=False,
        )

        # Convert markdown to HTML
        html = markdown_to_html(markdown)

        # Save output files if output_dir specified
        if output_dir:
            os.makedirs(output_dir, exist_ok=True)
            stem = Path(input_path).stem

            if fmt in ("html", "both"):
                html_path = os.path.join(output_dir, f"{stem}.html")
                with open(html_path, "w", encoding="utf-8") as f:
                    f.write(html)

            if fmt in ("markdown", "both"):
                md_path = os.path.join(output_dir, f"{stem}.md")
                with open(md_path, "w", encoding="utf-8") as f:
                    f.write(markdown)

        return {
            "success": True,
            "html": html,
            "markdown": markdown,
            "page_count": page_count,
            "engine": "pymupdf4llm",
        }

    except Exception as e:
        return {
            "success": False,
            "error": str(e),
            "html": "",
            "markdown": "",
            "page_count": 0,
            "engine": "pymupdf4llm",
        }


def markdown_to_html(md_text: str) -> str:
    """Convert Markdown to HTML. Uses markdown lib if available, otherwise basic conversion."""
    try:
        import markdown

        html = markdown.markdown(
            md_text,
            extensions=["tables", "fenced_code", "toc"],
            output_format="html5",
        )
        return f"<!DOCTYPE html>\n<html>\n<head><meta charset=\"utf-8\"></head>\n<body>\n{html}\n</body>\n</html>"
    except ImportError:
        # Fallback: basic markdown-to-HTML conversion
        return basic_md_to_html(md_text)


def basic_md_to_html(md_text: str) -> str:
    """Minimal Markdown to HTML conversion without external dependencies."""
    import re

    lines = md_text.split("\n")
    html_lines = []
    in_code_block = False
    in_list = False

    for line in lines:
        # Code blocks
        if line.strip().startswith("```"):
            if in_code_block:
                html_lines.append("</code></pre>")
                in_code_block = False
            else:
                html_lines.append("<pre><code>")
                in_code_block = True
            continue

        if in_code_block:
            html_lines.append(
                line.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")
            )
            continue

        stripped = line.strip()

        # Headings
        if stripped.startswith("######"):
            html_lines.append(f"<h6>{stripped[6:].strip()}</h6>")
        elif stripped.startswith("#####"):
            html_lines.append(f"<h5>{stripped[5:].strip()}</h5>")
        elif stripped.startswith("####"):
            html_lines.append(f"<h4>{stripped[4:].strip()}</h4>")
        elif stripped.startswith("###"):
            html_lines.append(f"<h3>{stripped[3:].strip()}</h3>")
        elif stripped.startswith("##"):
            html_lines.append(f"<h2>{stripped[2:].strip()}</h2>")
        elif stripped.startswith("#"):
            html_lines.append(f"<h1>{stripped[1:].strip()}</h1>")
        # List items
        elif stripped.startswith("- ") or stripped.startswith("* "):
            if not in_list:
                html_lines.append("<ul>")
                in_list = True
            html_lines.append(f"<li>{stripped[2:]}</li>")
        elif re.match(r"^\d+\.\s", stripped):
            if not in_list:
                html_lines.append("<ol>")
                in_list = True
            content = re.sub(r"^\d+\.\s", "", stripped)
            html_lines.append(f"<li>{content}</li>")
        else:
            if in_list:
                html_lines.append("</ul>" if html_lines[-2].startswith("<ul") else "</ol>")
                in_list = False
            if stripped:
                # Bold and italic
                processed = re.sub(r"\*\*(.+?)\*\*", r"<strong>\1</strong>", stripped)
                processed = re.sub(r"\*(.+?)\*", r"<em>\1</em>", processed)
                html_lines.append(f"<p>{processed}</p>")
            else:
                html_lines.append("")

    if in_list:
        html_lines.append("</ul>")

    body = "\n".join(html_lines)
    return f'<!DOCTYPE html>\n<html>\n<head><meta charset="utf-8"></head>\n<body>\n{body}\n</body>\n</html>'


def main():
    if len(sys.argv) < 2:
        print(
            json.dumps(
                {
                    "success": False,
                    "error": "Usage: pymupdf_convert.py <input_pdf> [--output-dir <dir>] [--format html|markdown|both]",
                }
            )
        )
        sys.exit(1)

    input_path = sys.argv[1]
    output_dir = None
    fmt = "both"

    i = 2
    while i < len(sys.argv):
        if sys.argv[i] == "--output-dir" and i + 1 < len(sys.argv):
            output_dir = sys.argv[i + 1]
            i += 2
        elif sys.argv[i] == "--format" and i + 1 < len(sys.argv):
            fmt = sys.argv[i + 1]
            i += 2
        else:
            i += 1

    result = convert_pdf(input_path, output_dir, fmt)
    print(json.dumps(result, ensure_ascii=False))
    sys.exit(0 if result["success"] else 1)


if __name__ == "__main__":
    main()
