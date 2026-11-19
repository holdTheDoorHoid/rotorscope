#!/usr/bin/env python3
"""Fail on Unicode that can hide or reorder source text ("Trojan Source").

Scans every file Git tracks (or the files named on the command line) and
reports each character that can make text look different from what a compiler
or reviewer reads:

  bidi        bidirectional controls U+202A..U+202E and U+2066..U+2069
  zero-width  U+200B..U+200D, U+2060, and U+FEFF anywhere except as the very
              first character of a file (a byte-order mark there is harmless)
  control     every other control or format character (Unicode categories
              Cc and Cf) and the line/paragraph separators U+2028, U+2029,
              except tab, line feed and carriage return

Each finding is printed as `path:line:column: U+XXXX NAME (rule)`; the
character itself is never printed. Files containing a NUL byte are binary and
skipped. Files that are not UTF-8 are listed as skipped (a warning, not a
failure).

Exceptions live in tools/hidden-chars-allow.txt, one per line:

    <path> U+XXXX <reason, required>

which allows that code point anywhere in that file. Lines starting with `#`
are comments.

    python3 -I tools/check-hidden-chars.py              # every tracked file
    python3 -I tools/check-hidden-chars.py FILE...      # just these files
    python3 -I tools/check-hidden-chars.py --self-test  # checks of this script

Exit status: 0 clean, 1 findings, 2 a broken allow-list or usage error.
Standard library only.
"""
import os
import re
import subprocess
import sys
import tempfile
import unicodedata

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ALLOW_FILE = os.path.join(ROOT, "tools", "hidden-chars-allow.txt")

BIDI = set(range(0x202A, 0x202F)) | set(range(0x2066, 0x206A))
ZERO_WIDTH = {0x200B, 0x200C, 0x200D, 0x2060, 0xFEFF}
SEPARATORS = {0x2028, 0x2029}
PERMITTED = {0x09, 0x0A, 0x0D}
ALLOW_LINE = re.compile(r"^(\S+)\s+U\+([0-9A-Fa-f]{4,6})\s+(\S.*)$")


def rule_for(cp, offset):
    """The rule a code point breaks at this character offset, or None."""
    if cp in PERMITTED:
        return None
    if cp in BIDI:
        return "bidi"
    if cp in ZERO_WIDTH:
        if cp == 0xFEFF and offset == 0:
            return None
        return "zero-width"
    if cp in SEPARATORS or unicodedata.category(chr(cp)) in ("Cc", "Cf"):
        return "control"
    return None


def char_name(cp):
    return unicodedata.name(chr(cp), "<control>" if cp < 0x20 or 0x7F <= cp < 0xA0 else "<unnamed>")


def scan_text(text):
    """Yield (line, column, code point, rule) for each finding; 1-based."""
    line, col = 1, 0
    for offset, ch in enumerate(text):
        col += 1
        cp = ord(ch)
        rule = rule_for(cp, offset)
        if rule:
            yield line, col, cp, rule
        if ch == "\n":
            line, col = line + 1, 0


def load_allow(path):
    """{(path, code point): reason}, or raise ValueError naming the bad line."""
    allow = {}
    if not os.path.exists(path):
        return allow
    with open(path, encoding="utf-8") as f:
        for n, raw in enumerate(f, 1):
            body = raw.strip()
            if not body or body.startswith("#"):
                continue
            m = ALLOW_LINE.match(body)
            if not m:
                raise ValueError(f"{path}:{n}: expected '<path> U+XXXX <reason>'")
            allow[(m.group(1), int(m.group(2), 16))] = m.group(3)
    return allow


def tracked_files(root):
    out = subprocess.run(["git", "-C", root, "ls-files", "-z"], check=True,
                         stdout=subprocess.PIPE).stdout
    return [p for p in out.decode("utf-8", "surrogateescape").split("\0") if p]


def check(root, files, allow, out=sys.stdout, err=sys.stderr):
    """Scan files (paths relative to root). Returns the number of findings."""
    findings, used = 0, set()
    for rel in files:
        full = os.path.join(root, rel)
        if os.path.islink(full) or not os.path.isfile(full):
            continue
        with open(full, "rb") as f:
            data = f.read()
        if b"\0" in data:
            continue
        try:
            text = data.decode("utf-8")
        except UnicodeDecodeError as e:
            print(f"skip {rel}: not UTF-8 (byte {e.start})", file=err)
            continue
        for line, col, cp, rule in scan_text(text):
            if (rel, cp) in allow:
                used.add((rel, cp))
                continue
            findings += 1
            print(f"{rel}:{line}:{col}: U+{cp:04X} {char_name(cp)} ({rule})", file=out)
    for key in sorted(set(allow) - used):
        if key[0] in files:
            print(f"note: allow-list entry {key[0]} U+{key[1]:04X} matched nothing", file=err)
    return findings


def self_test():
    failed = 0

    def expect(name, ok):
        nonlocal failed
        print(("ok   " if ok else "FAIL ") + name)
        failed += 0 if ok else 1

    class Sink:
        def __init__(self):
            self.lines = []

        def write(self, s):
            self.lines.extend(x for x in s.split("\n") if x)

        def flush(self):
            pass

    with tempfile.TemporaryDirectory() as t:
        def put(name, data):
            with open(os.path.join(t, name), "wb") as f:
                f.write(data.encode("utf-8") if isinstance(data, str) else data)
            return name

        def run(names, allow=None):
            out, err = Sink(), Sink()
            n = check(t, names, allow or {}, out, err)
            return n, out.lines, err.lines

        cases = [
            ("bidi override", "x = 1 # \u202e evil\n", "U+202E", "bidi"),
            ("bidi isolate", "a\u2066b\n", "U+2066", "bidi"),
            ("bidi pop isolate", "a\u2069b\n", "U+2069", "bidi"),
            ("zero-width space", "let a\u200b = 1;\n", "U+200B", "zero-width"),
            ("zero-width joiner", "a\u200db\n", "U+200D", "zero-width"),
            ("word joiner", "a\u2060b\n", "U+2060", "zero-width"),
            ("byte-order mark after the start", "a\ufeffb\n", "U+FEFF", "zero-width"),
            ("escape character", "a\x1b[31mb\n", "U+001B", "control"),
            ("left-to-right mark (format)", "a\u200eb\n", "U+200E", "control"),
            ("soft hyphen (format)", "a\u00adb\n", "U+00AD", "control"),
            ("line separator", "a\u2028b\n", "U+2028", "control"),
            ("delete", "a\x7fb\n", "U+007F", "control"),
        ]
        for i, (name, text, code, rule) in enumerate(cases):
            f = put(f"case{i}.txt", text)
            n, out, _ = run([f])
            expect(f"{name} is found", n == 1 and len(out) == 1 and f" {code} " in out[0] and f"({rule})" in out[0])

        f = put("clean.rs", "fn main() {\r\n\tprintln!(\"h\u00e9llo, \U0001f7e2 w\u00f6rld\");\n}\n")
        expect("tab, CR, LF and ordinary non-ASCII pass", run([f])[0] == 0)
        f = put("bom.txt", "\ufeffhello\n")
        expect("a byte-order mark as the first character passes", run([f])[0] == 0)
        f = put("pos.txt", "one\ntwo\nab\u202ecd\n")
        _, out, _ = run([f])
        expect("line and column are 1-based code-point positions", out == ["pos.txt:3:3: U+202E RIGHT-TO-LEFT OVERRIDE (bidi)"])
        f = put("bin.dat", b"\x00\x01\x1b\xe2\x80\xae")
        expect("a file with a NUL byte is skipped as binary", run([f])[0] == 0)
        f = put("latin1.txt", b"caf\xe9 \x1b\n")
        n, _, err = run([f])
        expect("a non-UTF-8 file is skipped with a note", n == 0 and any("not UTF-8" in e for e in err))
        f = put("two.txt", "a\u202eb\u200bc\n")
        n, _, _ = run([f], {("two.txt", 0x202E): "test"})
        expect("an allow-list entry covers only its own code point", n == 1)
        n, _, _ = run([f], {("other.txt", 0x202E): "test", ("other.txt", 0x200B): "test"})
        expect("an allow-list entry covers only its own file", n == 2)
        n, _, err = run(["clean.rs"], {("clean.rs", 0x202E): "test"})
        expect("an unused allow-list entry is noted", n == 0 and any("matched nothing" in e for e in err))
        allow_path = put("allow.txt", "# comment\n\ndocs/a.md U+200D emoji sequence in a quoted name\n")
        expect("the allow-list file parses", load_allow(os.path.join(t, allow_path)) == {("docs/a.md", 0x200D): "emoji sequence in a quoted name"})
        for bad in ("docs/a.md U+200D\n", "docs/a.md 200D reason\n", "U+200D reason\n"):
            put("bad.txt", bad)
            try:
                load_allow(os.path.join(t, "bad.txt"))
                ok = False
            except ValueError:
                ok = True
            expect(f"a malformed allow-list line is refused: {bad.strip()!r}", ok)

        # The command line: exit 1 on a finding, 0 when clean.
        me = os.path.abspath(__file__)
        r = subprocess.run([sys.executable, "-I", me, os.path.join(t, "case0.txt")],
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        expect("the command exits 1 on a finding", r.returncode == 1)
        r = subprocess.run([sys.executable, "-I", me, os.path.join(t, "clean.rs")],
                           stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        expect("the command exits 0 on a clean file", r.returncode == 0)

    print("check-hidden-chars self-test: " + ("all passed" if failed == 0 else f"{failed} FAILED"))
    return 1 if failed else 0


def main(argv):
    if argv[1:2] == ["--self-test"]:
        return self_test()
    if any(a.startswith("-") for a in argv[1:]):
        print("usage: check-hidden-chars.py [--self-test | FILE...]", file=sys.stderr)
        return 2
    try:
        allow = load_allow(ALLOW_FILE)
    except ValueError as e:
        print(f"check-hidden-chars: {e}", file=sys.stderr)
        return 2
    if len(argv) > 1:
        files = [os.path.relpath(os.path.abspath(p), ROOT) for p in argv[1:]]
    else:
        files = tracked_files(ROOT)
    n = check(ROOT, files, allow)
    if n:
        print(f"check-hidden-chars: {n} hidden or reordering character(s) found. Remove them, or "
              f"add a justified line to tools/hidden-chars-allow.txt.", file=sys.stderr)
        return 1
    print(f"check-hidden-chars: {len(files)} files, nothing hidden")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
