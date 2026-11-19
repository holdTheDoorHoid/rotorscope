#!/usr/bin/env python3
"""Dependency gate: fail when a new package or GitHub Action slips in.

Run from anywhere:  python3 -I tools/check-deps.py

  (no option)   Check every lockfile (the root Cargo.lock and firmware/*/Cargo.lock)
                against tools/approved-deps.txt, and every `uses:` line under
                .github/workflows/ (and .github/actions/) against
                tools/approved-actions.txt, which must also be pinned to a full
                40-hex commit id. Exit 1 on any problem.
  --write       Rewrite tools/approved-deps.txt from the lockfiles. For the owner:
                this APPROVES EVERYTHING CURRENTLY LOCKED.
  --self-test   Check the rules on temporary files. Touches nothing in the repository.

Allow-list line (tools/approved-deps.txt), sorted, one per line:

    <package name> <series> <source>

  series  the compatibility series of the locked version: the major version when it
          is 1 or more ("1"), "0.<minor>" for 0.x ("0.4"), "0.0.<patch>" for 0.0.x.
          A routine patch or minor update stays in its series and passes; a
          breaking-change release (another series) is a different line and fails
          until the owner approves it.
  source  "registry" (crates.io), "path" (a crate in this repository; its series
          is written "*"), "git <url>" (the address without ?rev= or #commit), or
          "registry <url>" for any registry other than crates.io.

A git dependency moving to another commit of the same address passes; a new address
fails. Standard library only; no network.
"""
import os
import re
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CRATES_IO = ("registry+https://github.com/rust-lang/crates.io-index",
             "sparse+https://index.crates.io/")
ASK = ("Ask the owner first. Only with the owner's approval, add the allow-list line "
       "in the same commit as the change that adds it.")
HEX40 = re.compile(r"^[0-9a-f]{40}$")
USES = re.compile(r"^\s*(?:-\s*)?uses:\s*(\S+)")


def series(version):
    core = re.split(r"[-+]", version, 1)[0]
    nums = (core.split(".") + ["0", "0"])[:3]
    try:
        major, minor, patch = (int(n) for n in nums)
    except ValueError:
        return version  # unparseable: keep exact, so any change shows up
    if major > 0:
        return str(major)
    if minor > 0:
        return "0.%d" % minor
    return "0.0.%d" % patch


def source_kind(source):
    if source is None:
        return "path"
    if source in CRATES_IO or source.rstrip("/") in [s.rstrip("/") for s in CRATES_IO]:
        return "registry"
    if source.startswith("git+"):
        url = re.split(r"[?#]", source[4:], 1)[0]
        return "git " + url
    if source.startswith(("registry+", "sparse+")):
        return "registry " + source.split("+", 1)[1]
    return "unknown " + source


def entries_of(lockfile):
    """The set of allow-list lines a lockfile needs."""
    with open(lockfile, "rb") as f:
        data = tomllib.load(f)
    out = set()
    for pkg in data.get("package", []):
        kind = source_kind(pkg.get("source"))
        ser = "*" if kind == "path" else series(pkg.get("version", "0"))
        out.add("%s %s %s" % (pkg["name"], ser, kind))
    return out


def lockfiles(root):
    found = []
    top = os.path.join(root, "Cargo.lock")
    if os.path.isfile(top):
        found.append(top)
    fw = os.path.join(root, "firmware")
    if os.path.isdir(fw):
        for name in sorted(os.listdir(fw)):
            p = os.path.join(fw, name, "Cargo.lock")
            if os.path.isfile(p):
                found.append(p)
    return found


def read_list(path):
    if not os.path.isfile(path):
        return None
    with open(path, encoding="utf-8") as f:
        return {ln.strip() for ln in f if ln.strip() and not ln.lstrip().startswith("#")}


def check_deps(root):
    problems, warnings = [], []
    approved = read_list(os.path.join(root, "tools", "approved-deps.txt"))
    if approved is None:
        return ["tools/approved-deps.txt is missing."], warnings, {}
    used, counts = set(), {}
    for lf in lockfiles(root):
        rel = os.path.relpath(lf, root)
        need = entries_of(lf)
        counts[rel] = len(need)
        used |= need
        for e in sorted(need - approved):
            problems.append("%s: not approved: %s\n    %s" % (rel, e, ASK))
    for e in sorted(approved - used):
        warnings.append("approved but no lockfile uses it any more: %s" % e)
    return problems, warnings, counts


def uses_lines(root):
    """(relative path, line number, value) for every `uses:` in workflows/actions."""
    out = []
    for sub in ("workflows", "actions"):
        base = os.path.join(root, ".github", sub)
        for d, _dirs, files in os.walk(base):
            for name in sorted(files):
                if not name.endswith((".yml", ".yaml")):
                    continue
                p = os.path.join(d, name)
                with open(p, encoding="utf-8") as f:
                    for n, line in enumerate(f, 1):
                        m = USES.match(line)
                        if m:
                            val = m.group(1).strip("\"'")
                            out.append((os.path.relpath(p, root), n, val))
    return sorted(out)


def check_actions(root):
    problems = []
    approved = read_list(os.path.join(root, "tools", "approved-actions.txt"))
    if approved is None:
        problems.append("tools/approved-actions.txt is missing.")
        approved = set()
    for rel, n, val in uses_lines(root):
        where = "%s:%d" % (rel, n)
        if val.startswith("./"):
            continue  # an action in this repository; .github/ is owner-reviewed
        if val.startswith("docker://") or "@" not in val:
            problems.append("%s: `uses: %s` is not a pinned repository action. %s" % (where, val, ASK))
            continue
        path, ref = val.rsplit("@", 1)
        repo = "/".join(path.split("/")[:2]).lower()
        if repo not in {a.lower() for a in approved}:
            problems.append("%s: action repository %s is not in tools/approved-actions.txt. %s"
                            % (where, repo, ASK))
        if not HEX40.match(ref):
            problems.append("%s: `uses: %s` is not pinned to a full 40-character commit id "
                            "(a tag or branch can be moved). Pin it, with a comment naming the tag."
                            % (where, val))
    return problems


def write_list(root):
    allp = set()
    for lf in lockfiles(root):
        allp |= entries_of(lf)
    header = ("# Approved packages: <name> <series> <source>. Sorted. Checked by tools/check-deps.py.\n"
              "# A line is added only with the owner's explicit approval, in the same commit as the\n"
              "# lockfile change. Format and rules: docs/dev/agents.md.\n")
    with open(os.path.join(root, "tools", "approved-deps.txt"), "w", encoding="utf-8") as f:
        f.write(header + "".join(e + "\n" for e in sorted(allp)))
    return len(allp)


# ---------------------------------------------------------------- self-test

def _lock(pkgs):
    out = "version = 4\n\n"
    for name, ver, src in pkgs:
        out += '[[package]]\nname = "%s"\nversion = "%s"\n' % (name, ver)
        if src:
            out += 'source = "%s"\n' % src
        out += "\n"
    return out


def self_test():
    import tempfile
    CIO = "registry+https://github.com/rust-lang/crates.io-index"
    failures = []

    def expect(label, cond):
        print("  %-62s %s" % (label, "ok" if cond else "FAILED"))
        if not cond:
            failures.append(label)

    def tree(lock_pkgs, workflow=None, actions="actions/checkout\n", approved=None):
        d = tempfile.mkdtemp(prefix="check-deps-test-")
        os.makedirs(os.path.join(d, "tools"))
        os.makedirs(os.path.join(d, "firmware", "fw"))
        os.makedirs(os.path.join(d, ".github", "workflows"))
        with open(os.path.join(d, "Cargo.lock"), "w") as f:
            f.write(_lock(lock_pkgs))
        with open(os.path.join(d, "firmware", "fw", "Cargo.lock"), "w") as f:
            f.write(_lock([("fwonly", "1.0.0", CIO)]))
        with open(os.path.join(d, "tools", "approved-actions.txt"), "w") as f:
            f.write(actions)
        if workflow is not None:
            with open(os.path.join(d, ".github", "workflows", "w.yml"), "w") as f:
                f.write(workflow)
        base = [("serde", "1.0.200", CIO), ("mine", "0.1.0", None)]
        with open(os.path.join(d, "tools", "approved-deps.txt"), "w") as f:
            f.write("\n".join(approved if approved is not None else
                              ["fwonly 1 registry", "mine * path", "serde 1 registry"]) + "\n")
        return d

    base = [("serde", "1.0.200", CIO), ("mine", "0.1.0", None)]
    print("dependencies")
    p, w, c = check_deps(tree(base))
    expect("approved set passes", not p and not w)
    p, _, _ = check_deps(tree([("serde", "1.0.215", CIO), ("mine", "0.9.0", None)]))
    expect("patch/minor update of an approved package passes", not p)
    p, _, _ = check_deps(tree(base + [("evil", "0.1.0", CIO)]))
    expect("a new package fails", len(p) == 1 and "evil" in p[0])
    p, _, _ = check_deps(tree([("serde", "2.0.0", CIO), ("mine", "0.1.0", None)]))
    expect("a new major version fails", len(p) == 1 and "serde 2" in p[0])
    p, _, _ = check_deps(tree([("serde", "1.0.200", "git+https://example.com/serde?rev=abc#" + "a" * 40),
                               ("mine", "0.1.0", None)]))
    expect("same name from a git source fails", len(p) == 1 and "git https://example.com/serde" in p[0])
    p, _, _ = check_deps(tree([("serde", "1.0.200", "registry+https://other.example/index"),
                               ("mine", "0.1.0", None)]))
    expect("same name from a renamed registry fails", len(p) == 1 and "registry https://other" in p[0])
    p, _, _ = check_deps(tree([("serde", "1.0.200", None), ("mine", "0.1.0", None)]))
    expect("a registry package turned into a path package fails", len(p) == 1)
    p, _, _ = check_deps(tree([("serde", "1.0.200", "sparse+https://index.crates.io/"),
                               ("mine", "0.1.0", None)]))
    expect("crates.io sparse index counts as registry", not p)
    d = tree([("serde", "1.0.200", "git+https://example.com/s?rev=b#" + "b" * 40), ("mine", "0.1.0", None)],
             approved=["fwonly 1 registry", "mine * path", "serde 1 git https://example.com/s"])
    p, _, _ = check_deps(d)
    expect("approved git dependency passes; new commit of it passes", not p)
    p, w, _ = check_deps(tree(base, approved=["fwonly 1 registry", "mine * path", "serde 1 registry",
                                             "gone 1 registry"]))
    expect("unused allow-list entry warns, does not fail", not p and len(w) == 1 and "gone" in w[0])
    d = tree(base + [("evil", "0.1.0", CIO)])
    os.remove(os.path.join(d, "firmware", "fw", "Cargo.lock"))
    with open(os.path.join(d, "firmware", "fw", "Cargo.lock"), "w") as f:
        f.write(_lock([("fwonly", "1.0.0", CIO), ("evil2", "1.0.0", CIO)]))
    p, _, _ = check_deps(d)
    expect("each lockfile is named in the report",
           any(x.startswith("Cargo.lock") for x in p) and any("fw" in x and "evil2" in x for x in p))
    n = write_list(d)
    p, _, _ = check_deps(d)
    expect("--write approves the current lockfiles", not p and n == 5)

    print("actions")
    sha = "a" * 40
    wf = lambda ref, repo="actions/checkout": "jobs:\n  b:\n    steps:\n      - uses: %s@%s\n" % (repo, ref)
    expect("pinned, approved action passes", not check_actions(tree(base, wf(sha))))
    expect("a tag fails", len(check_actions(tree(base, wf("v4")))) == 1)
    expect("a tag shaped like hex but 39 long fails", len(check_actions(tree(base, wf("a" * 39)))) == 1)
    expect("a tag shaped like hex but 41 long fails", len(check_actions(tree(base, wf("a" * 41)))) == 1)
    expect("40 characters that are not all hex fails", len(check_actions(tree(base, wf("g" * 40)))) == 1)
    expect("upper-case hex fails (git ids are lower-case)", len(check_actions(tree(base, wf("A" * 40)))) == 1)
    p = check_actions(tree(base, wf(sha, "evil/action")))
    expect("pinned but unapproved repository fails", len(p) == 1 and "evil/action" in p[0])
    expect("subdirectory action uses its repository's approval",
           not check_actions(tree(base, wf(sha, "actions/checkout/sub"))))
    expect("docker:// fails", len(check_actions(tree(base, "      - uses: docker://alpine:3\n"))) == 1)
    expect("no @ at all fails", len(check_actions(tree(base, "      - uses: actions/checkout\n"))) == 1)
    expect("local ./ action is left to CODEOWNERS", not check_actions(tree(base, "      - uses: ./.github/actions/x\n")))
    expect("a commented-out uses is ignored", not check_actions(tree(base, "      # uses: evil/x@v1\n")))
    expect("quoted value is read", len(check_actions(tree(base, '      - uses: "actions/checkout@v4"\n'))) == 1)

    print("%d failure(s)" % len(failures))
    return 1 if failures else 0


def main(argv):
    args = argv[1:]
    if args == ["--self-test"]:
        return self_test()
    if args == ["--write"]:
        n = write_list(ROOT)
        print("Wrote tools/approved-deps.txt: %d entries." % n)
        print("*** REMINDER: this approves EVERYTHING currently in the lockfiles. Review "
              "`git diff tools/approved-deps.txt` yourself, as the owner, before committing. ***")
        return 0
    if args:
        print(__doc__)
        return 2
    problems, warnings, counts = check_deps(ROOT)
    for rel in sorted(counts):
        print("%-34s %4d packages" % (rel, counts[rel]))
    aprob = check_actions(ROOT)
    for w in warnings:
        print("WARNING:", w)
    for pr in problems + aprob:
        print("FAIL:", pr)
    if problems or aprob:
        print("\nFAILED: %d problem(s). Nothing may be added to a lockfile or workflow without the "
              "owner's approval (see AGENTS.md)." % (len(problems) + len(aprob)))
        return 1
    print("ok: every locked package and every action is approved.")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
