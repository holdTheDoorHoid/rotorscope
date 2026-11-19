#!/usr/bin/env python3
"""Check the documentation's links. Run by `tools/docs-build.sh check`.

Reports, for every Markdown page under docs/:
  * relative links to files that do not exist;
  * links to a heading (#anchor) that the target page does not have;
  * relative links that leave docs/ (they work on GitHub but not on the site;
    write the full https://github.com/... address instead);
  * links to this repository's files on GitHub that do not exist here;
  * pages that are not listed in docs/SUMMARY.md.

Standard library only. Exit status 1 if anything is wrong.
"""
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
DOCS = os.path.join(ROOT, "docs")
REPO_URL = "https://github.com/holdTheDoorHoid/rotorscope/"

LINK = re.compile(r"(!?)\[(?:[^\]\\]|\\.)*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
FENCE = re.compile(r"^\s*(```+|~~~+)")
HEADING = re.compile(r"^\s{0,3}(#{1,6})\s+(.*?)\s*#*\s*$")


def strip_code(text):
    """Blank out fenced blocks and inline code so example links are ignored."""
    out, fence = [], None
    for line in text.split("\n"):
        m = FENCE.match(line)
        if m:
            mark = m.group(1)
            if fence is None:
                fence = mark
            elif mark[0] == fence[0] and len(mark) >= len(fence):
                fence = None
            out.append("")
            continue
        out.append("" if fence else re.sub(r"`[^`\n]*`", "", line))
    return "\n".join(out)


def slug(heading):
    """The anchor mdBook and GitHub give a heading."""
    text = re.sub(r"!?\[([^\]]*)\]\([^)]*\)", r"\1", heading)
    text = re.sub(r"<[^>]+>", "", text)
    text = text.replace("`", "").replace("*", "").strip().lower()
    out = []
    for ch in text:
        if ch.isalnum() or ch in "-_":
            out.append(ch)
        elif ch == " ":
            out.append("-")
    return "".join(out)


def anchors(path, cache={}):
    if path not in cache:
        found, counts, fence = set(), {}, None
        with open(path, encoding="utf-8") as f:
            for line in f:
                m = FENCE.match(line)
                if m:
                    mark = m.group(1)
                    if fence is None:
                        fence = mark
                    elif mark[0] == fence[0] and len(mark) >= len(fence):
                        fence = None
                    continue
                if fence:
                    continue
                h = HEADING.match(line)
                if h:
                    s = slug(h.group(2))
                    n = counts.get(s, 0)
                    counts[s] = n + 1
                    found.add(s if n == 0 else "%s-%d" % (s, n))
                for a in re.findall(r"<a\s+(?:id|name)=\"([^\"]+)\"", line):
                    found.add(a)
        cache[path] = found
    return cache[path]


def main():
    problems = []
    pages = []
    for base, _dirs, files in os.walk(DOCS):
        for name in files:
            if name.endswith(".md"):
                pages.append(os.path.join(base, name))
    pages.sort()

    with open(os.path.join(DOCS, "SUMMARY.md"), encoding="utf-8") as f:
        listed = {os.path.normpath(os.path.join(DOCS, m.group(2)))
                  for m in LINK.finditer(f.read())}

    for page in pages:
        rel = os.path.relpath(page, ROOT)
        if page not in listed and os.path.basename(page) != "SUMMARY.md":
            problems.append("%s: not listed in docs/SUMMARY.md" % rel)
        with open(page, encoding="utf-8") as f:
            text = strip_code(f.read())
        for lineno, line in enumerate(text.split("\n"), 1):
            for m in LINK.finditer(line):
                target = m.group(2)
                where = "%s:%d" % (rel, lineno)
                if target.startswith(REPO_URL):
                    rest = target[len(REPO_URL):].split("#")[0].split("?")[0]
                    parts = rest.split("/", 2)
                    if len(parts) == 3 and parts[0] in ("blob", "tree") and parts[1] == "main":
                        if not os.path.exists(os.path.join(ROOT, parts[2].rstrip("/"))):
                            problems.append("%s: GitHub link to a file not in this repository: %s"
                                            % (where, parts[2]))
                    continue
                if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target):
                    continue
                path, _, frag = target.partition("#")
                dest = page if path == "" else os.path.normpath(
                    os.path.join(os.path.dirname(page), path))
                if not (dest == DOCS or dest.startswith(DOCS + os.sep)):
                    problems.append("%s: relative link leaves docs/ (use the full GitHub "
                                    "address): %s" % (where, target))
                    continue
                if not os.path.exists(dest):
                    problems.append("%s: broken link: %s" % (where, target))
                    continue
                if frag and dest.endswith(".md") and frag not in anchors(dest):
                    problems.append("%s: no heading '#%s' in %s"
                                    % (where, frag, os.path.relpath(dest, ROOT)))

    for p in problems:
        print(p)
    print("%d page(s) checked, %d problem(s)." % (len(pages), len(problems)))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
