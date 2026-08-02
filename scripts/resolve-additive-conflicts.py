#!/usr/bin/env python3
"""Resolve the mechanical half of a rebase conflict, and refuse the rest.

Most conflicts in this rebase are additive on both sides: upstream added a
field or a binding, our layer added a different one, and neither replaces the
other. Those can be resolved by keeping both sides in order (upstream first,
since our code often references bindings upstream introduces).

What this deliberately does NOT do is guess. A hunk where both sides changed
the *same* line is a real semantic conflict — the kind that, applied blindly,
silently drops one side's behaviour. Those are left in place and reported, so a
human decides.

Usage:  resolve-additive-conflicts.py <file> [...]
Exit 0 = every conflict in the listed files was additive and resolved.
Exit 2 = at least one conflict needs a decision; nothing in that file changed.
"""
import sys


def regions(lines):
    """Locate conflict regions, tolerating diff3's ||||||| base section."""
    out, cur = [], None
    for i, line in enumerate(lines):
        if line.startswith("<<<<<<<"):
            cur = {"start": i, "base": None, "sep": None}
        elif line.startswith("|||||||") and cur:
            cur["base"] = i
        elif line.startswith("=======") and cur:
            cur["sep"] = i
        elif line.startswith(">>>>>>>") and cur:
            cur["end"] = i
            out.append(cur)
            cur = None
    return out


def is_additive(upstream, ours, base):
    """True when both sides only ADD to the common ancestor.

    If the base is empty both sides are pure additions. Otherwise every base
    line must survive on both sides — if either side dropped or rewrote a base
    line, the two edits are competing and a merge would silently pick one.
    """
    if not [l for l in base if l.strip()]:
        return True
    base_set = [l for l in base if l.strip()]
    return all(l in upstream for l in base_set) and all(l in ours for l in base_set)


def balanced(lines):
    """Rough delimiter balance over a set of lines.

    Not a parser: string literals and comments can skew it. It is used only to
    compare a file against itself before and after a rewrite, where any skew is
    identical on both sides and cancels out.
    """
    text = "\n".join(lines)
    return (text.count("{") - text.count("}"),
            text.count("(") - text.count(")"),
            text.count("[") - text.count("]"))


def main(paths):
    failed = False
    for path in paths:
        original = open(path, errors="replace").read().splitlines()
        lines = list(original)
        found = regions(lines)
        if not found:
            continue

        undecidable = []
        for r in found:
            up_end = r["base"] if r["base"] is not None else r["sep"]
            upstream = lines[r["start"] + 1:up_end]
            base = lines[r["base"] + 1:r["sep"]] if r["base"] is not None else []
            ours = lines[r["sep"] + 1:r["end"]]
            if not is_additive(upstream, ours, base):
                undecidable.append(r["start"] + 1)

        if undecidable:
            print(f"NEEDS REVIEW {path}: competing edits at line(s) "
                  f"{', '.join(map(str, undecidable))}")
            failed = True
            continue

        # Rewrite back-to-front so earlier indices stay valid.
        for r in reversed(found):
            up_end = r["base"] if r["base"] is not None else r["sep"]
            upstream = lines[r["start"] + 1:up_end]
            ours = lines[r["sep"] + 1:r["end"]]
            lines[r["start"]:r["end"] + 1] = upstream + ours

        # Concatenating two hunks can splice one side into the middle of the
        # other's unfinished expression — a `mod` block landing inside an open
        # `assert!(` produced a file that could not be parsed at all.
        #
        # Comparing the joined file against itself proves nothing: both sides
        # carry the same skew and it cancels. The real question is whether the
        # JOINED hunk is a well-formed replacement, so each hunk is measured on
        # its own. Two hunks that are individually balanced concatenate safely;
        # if either leaves a delimiter open, gluing them interleaves two
        # unfinished expressions and the result cannot parse.
        spliced = []
        for r in found:
            up_end = r["base"] if r["base"] is not None else r["sep"]
            upstream = original[r["start"] + 1:up_end]
            ours = original[r["sep"] + 1:r["end"]]
            if balanced(upstream) != (0, 0, 0) or balanced(ours) != (0, 0, 0):
                spliced.append(r["start"] + 1)
        if spliced:
            print(f"NEEDS REVIEW {path}: hunk(s) at line(s) "
                  f"{', '.join(map(str, spliced))} leave delimiters open; "
                  f"joining them would splice one into the other")
            failed = True
            continue

        open(path, "w").write("\n".join(lines) + "\n")
        print(f"resolved {path}: {len(found)} additive conflict(s)")

    return 2 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
