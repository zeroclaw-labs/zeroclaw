#!/usr/bin/env python3
"""Fill empty and fuzzy .po file entries with AI translations.

Usage:
    fill-translations.py --po docs/book/po/ja.po --locale ja
    fill-translations.py --po docs/book/po/ja.po --locale ja --force
    fill-translations.py --po docs/book/po/ja.po --locale ja --model claude-opus-4-7
"""

import argparse
import json
import os
import sys

try:
    import polib
except ImportError:
    print("error: polib not installed — run: pip install polib", file=sys.stderr)
    sys.exit(1)

try:
    import anthropic
except ImportError:
    print("error: anthropic not installed — run: pip install anthropic", file=sys.stderr)
    sys.exit(1)


LOCALE_NAMES = {
    "ja": "Japanese",
    "ko": "Korean",
    "zh": "Simplified Chinese",
    "zh-TW": "Traditional Chinese",
    "fr": "French",
    "de": "German",
    "es": "Spanish",
    "pt": "Portuguese (Brazilian)",
    "ru": "Russian",
    "ar": "Arabic",
    "hi": "Hindi",
    "tr": "Turkish",
    "nl": "Dutch",
    "pl": "Polish",
    "sv": "Swedish",
    "fi": "Finnish",
    "nb": "Norwegian Bokmål",
    "da": "Danish",
    "it": "Italian",
}

DEFAULT_MODEL = os.environ.get("FILL_MODEL", "claude-haiku-4-5-20251001")
CHUNK_SIZE = 50


def needs_translation(entry, force):
    if entry.obsolete:
        return False
    if force:
        return True
    return "fuzzy" in entry.flags or not entry.msgstr or entry.msgstr.strip() == ""


def is_code_only(text):
    """Single backtick-wrapped token — no value in translating."""
    stripped = text.strip()
    return stripped.startswith("`") and stripped.endswith("`") and stripped.count("`") == 2


def reference_context(entry):
    """Return a compact location hint for the AI (e.g. 'introduction.md:3')."""
    if not entry.occurrences:
        return ""
    parts = [f"{f}:{l}" for f, l in entry.occurrences[:2]]
    return ", ".join(parts)


def translate_batch(client, numbered_triples, locale_name, model):
    """Translate a list of (number, msgid, location) triples.

    Returns {str(number): translation}.
    """
    items_lines = []
    for n, msgid, location in numbered_triples:
        loc_comment = f"  # {location}" if location else ""
        items_lines.append(f'{n}. {json.dumps(msgid)}{loc_comment}')
    items = "\n".join(items_lines)

    prompt = f"""You are translating technical documentation strings for ZeroClaw, \
an open-source personal AI agent/bot framework written in Rust, into {locale_name}.

Rules:
- Preserve all markdown formatting exactly (**, *, `, [](), headings, lists, tables)
- Do NOT translate content inside backticks or code fences
- Keep technical terms in English: ZeroClaw, Matrix, Mattermost, LINE, MCP, API, CLI, \
SOP, MQTT, TLS, Rust, cargo, mdBook, etc.
- Preserve any HTML entities, angle-bracket links (<https://...>), and special characters
- For UI strings (button labels, menu items), use natural phrasing in {locale_name}
- Return ONLY a JSON object mapping number (as string key) to translated string

Strings to translate:
{items}

Respond with a JSON object like: {{"1": "translated text", "2": "another translation"}}"""

    message = client.messages.create(
        model=model,
        max_tokens=4096,
        messages=[{"role": "user", "content": prompt}],
    )

    response_text = message.content[0].text.strip()
    # Strip optional markdown code fence
    if response_text.startswith("```"):
        lines = response_text.split("\n")
        response_text = "\n".join(lines[1:-1]).strip()

    try:
        return json.loads(response_text)
    except json.JSONDecodeError as exc:
        print(
            f"warning: JSON parse failed ({exc})\n"
            f"  raw response: {response_text[:200]}",
            file=sys.stderr,
        )
        return {}


def main():
    parser = argparse.ArgumentParser(
        description="AI-fill empty/fuzzy .po entries",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--po", required=True, help="Path to .po file")
    parser.add_argument("--locale", required=True, help="Target locale code (e.g. ja)")
    parser.add_argument(
        "--force",
        action="store_true",
        help="Re-translate ALL entries, not just empty/fuzzy ones",
    )
    parser.add_argument(
        "--model",
        default=DEFAULT_MODEL,
        help=f"Claude model to use (default: {DEFAULT_MODEL})",
    )
    args = parser.parse_args()

    api_key = os.environ.get("ANTHROPIC_API_KEY", "")
    if not api_key:
        print(
            "error: ANTHROPIC_API_KEY is not set\n"
            "  Export it before running: export ANTHROPIC_API_KEY=sk-ant-...",
            file=sys.stderr,
        )
        sys.exit(1)

    locale_name = LOCALE_NAMES.get(args.locale, args.locale)

    po = polib.pofile(args.po)

    to_translate = [
        entry
        for entry in po
        if needs_translation(entry, args.force) and not is_code_only(entry.msgid)
    ]

    if not to_translate:
        print(f"==> {args.locale}: nothing to translate")
        return

    mode = "force-retranslating all" if args.force else "translating delta"
    print(
        f"==> {args.locale}: {mode} — {len(to_translate)} entries "
        f"via {args.model}"
    )

    client = anthropic.Anthropic(api_key=api_key)

    translated_count = 0
    total_chunks = (len(to_translate) + CHUNK_SIZE - 1) // CHUNK_SIZE

    for chunk_idx, chunk_start in enumerate(range(0, len(to_translate), CHUNK_SIZE), 1):
        chunk = to_translate[chunk_start:chunk_start + CHUNK_SIZE]
        numbered = [
            (j + 1, entry.msgid, reference_context(entry))
            for j, entry in enumerate(chunk)
        ]

        print(f"    chunk {chunk_idx}/{total_chunks} ({len(chunk)} strings)...")

        try:
            results = translate_batch(client, numbered, locale_name, args.model)
        except Exception as exc:
            print(f"warning: translation chunk failed ({exc}); skipping", file=sys.stderr)
            continue

        for j, entry in enumerate(chunk):
            key = str(j + 1)
            if key not in results:
                continue
            entry.msgstr = results[key]
            if "fuzzy" in entry.flags:
                entry.flags.remove("fuzzy")
            translated_count += 1

    po.save(args.po)
    print(f"==> {args.locale}: wrote {translated_count}/{len(to_translate)} translations → {args.po}")


if __name__ == "__main__":
    main()
