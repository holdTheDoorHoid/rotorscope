#!/usr/bin/env python3
# A firmware image's parts list (software bill of materials), from cargo's own view of the
# build plus the code cargo does not see. Standard library only; no network. Run with -I:
#
#   cargo metadata --locked --offline --format-version 1 \
#       [--features ...] [--no-default-features] --manifest-path <firmware>/Cargo.toml >meta.json
#   cargo tree --locked --offline --target <triple> [features] --manifest-path ... \
#       --prefix none -f '{p}' -e normal,no-proc-macro >image.tree     (what is linked)
#   cargo tree ... -e normal,build >all.tree                           (plus what runs at build)
#   python3 -I tools/release-parts.py --metadata meta.json --tree-image image.tree \
#       --tree-all all.tree --lock <firmware>/Cargo.lock --root <repository> --image gateway \
#       --commit <id> --text gateway.parts.txt --cyclonedx gateway.cdx.json
#   python3 -I tools/release-parts.py --self-test
#
# What it lists, one line per part, sorted, with no dates and no absolute paths (the same
# commit gives the same files on any machine):
#   - every crate cargo builds for this firmware, its features and its target, with version,
#     source (crates.io, or a path relative to the repository), licence as the crate declares
#     it, and the SHA-256 the lockfile records for a crates.io crate. A crate is "image" when
#     it is reached from the firmware through normal dependencies, "build" when only through
#     build scripts or procedural macros (it runs on the build machine and is not in the image).
#     Which crates are built comes from `cargo tree` (cargo's metadata graph also lists optional
#     dependencies that only a weak feature names, such as every chip's radio crate); the
#     details (licence, source) come from `cargo metadata`;
#   - binary libraries (*.a) inside those crates, such as Espressif's radio libraries in
#     esp-wifi-sys-*, with their own SHA-256 (tools/check-wifi-blobs.sh checks the radio ones
#     against recorded hashes);
#   - what the repository patches: esp-radio (firmware/patches/esp-radio) and, for the P4 hub,
#     esp-hal and friends (firmware/patches/esp-hal-p4-pre-v3), with the release or commit they
#     come from;
#   - vendored code cargo never sees: third_party/opendroneid-core-c, marked as not in the image
#     (only host tests build it).
# --cyclonedx also writes the same as a CycloneDX 1.5 JSON document.
import argparse
import hashlib
import json
import os
import sys
import tempfile
import tomllib
import uuid

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"


def sha256_file(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def lock_checksums(lock_path):
    with open(lock_path, "rb") as f:
        lock = tomllib.load(f)
    out = {}
    for p in lock.get("package", []):
        if "checksum" in p:
            out[(p["name"], p["version"], p.get("source", ""))] = p["checksum"]
    return out


def classify(meta):
    """(image ids, build ids) reached from the resolve root."""
    nodes = {n["id"]: n for n in meta["resolve"]["nodes"]}
    pkgs = {p["id"]: p for p in meta["packages"]}
    root = meta["resolve"]["root"]
    if root is None:
        raise SystemExit("release-parts: cargo metadata has no root package (pass --manifest-path of one firmware)")

    def is_proc_macro(pid):
        return any("proc-macro" in t["kind"] for t in pkgs[pid]["targets"])

    def edges(pid, kinds):
        for d in nodes[pid]["deps"]:
            if any(k["kind"] in kinds for k in d["dep_kinds"]):
                yield d["pkg"]

    image, todo = {root}, [root]
    while todo:
        pid = todo.pop()
        for dep in edges(pid, {None}):
            if dep not in image and not is_proc_macro(dep):
                image.add(dep)
                todo.append(dep)
    every, todo = {root}, [root]
    while todo:
        pid = todo.pop()
        for dep in edges(pid, {None, "build"}):
            if dep not in every:
                every.add(dep)
                todo.append(dep)
    return root, image, every - image


def parse_tree(path):
    """(name, version) pairs from `cargo tree --prefix none -f '{p}'` output."""
    out = set()
    with open(path) as f:
        for line in f:
            w = line.split()
            if len(w) >= 2 and w[1].startswith("v"):
                out.add((w[0], w[1][1:]))
    return out


def classify_trees(meta, image_tree, all_tree):
    """(root, image ids, build ids) from two `cargo tree` listings."""
    root = meta["resolve"]["root"]
    if root is None:
        raise SystemExit("release-parts: cargo metadata has no root package (pass --manifest-path of one firmware)")
    by_nv = {}
    for p in meta["packages"]:
        by_nv.setdefault((p["name"], p["version"]), []).append(p["id"])
    resolved = {n["id"] for n in meta["resolve"]["nodes"]}

    def ids(pairs):
        out = set()
        for nv in pairs:
            cands = [i for i in by_nv.get(nv, []) if i in resolved]
            if len(cands) != 1:
                raise SystemExit(f"release-parts: cargo tree names {nv[0]} {nv[1]}, which matches {len(cands)} packages in cargo metadata")
            out.add(cands[0])
        return out

    image = ids(image_tree) | {root}
    every = ids(all_tree) | image
    return root, image, every - image


def rel_source(pkg, root_dir):
    if pkg["source"] is None:
        d = os.path.dirname(pkg["manifest_path"])
        rel = os.path.relpath(d, root_dir)
        if rel.startswith(".."):
            raise SystemExit(f"release-parts: path crate {pkg['name']} lies outside the repository; refusing to print its location")
        return "path:" + rel.replace(os.sep, "/")
    if pkg["source"] == CRATES_IO:
        return "crates.io"
    return pkg["source"]


def licence(pkg):
    if pkg.get("license"):
        return pkg["license"]
    if pkg.get("license_file"):
        return "see " + os.path.basename(pkg["license_file"])
    return "UNDECLARED"


def blobs(pkg):
    """Binary libraries inside a crate: (relative path, sha256)."""
    top = os.path.dirname(pkg["manifest_path"])
    out = []
    for d, dirs, files in os.walk(top):
        dirs[:] = sorted(x for x in dirs if x not in ("target", ".git"))
        for f in sorted(files):
            if f.endswith(".a"):
                p = os.path.join(d, f)
                out.append((os.path.relpath(p, top).replace(os.sep, "/"), sha256_file(p)))
    return out


def patched(root_dir, rel):
    """Provenance of a crate the repository patches, or None."""
    if rel == "path:firmware/patches/esp-radio":
        return {"from": "crates.io esp-radio 1.0.0-beta.1",
                "from_sha256": "50247f4e91000fc87661e77254126c74acc4535f822ee247f7778807202dc938",
                "note": "two files changed, see firmware/patches/README.md; checked by tools/check-wifi-blobs.sh"}
    pre = "path:firmware/patches/esp-hal-p4-pre-v3/upstream/"
    if rel.startswith(pre):
        with open(os.path.join(root_dir, "firmware/patches/esp-hal-p4-pre-v3/manifest.json")) as f:
            m = json.load(f)
        return {"from": f"{m['upstream']['repository']} at {m['upstream']['revision']}",
                "from_sha256": None,
                "note": f"pre-v3 patch SHA-256 {m['patch']['sha256']} plus rotorscope-pre-v3-memory.patch, see firmware/patches/esp-hal-p4-pre-v3/PROVENANCE.md"}
    return None


VENDORED = [
    # (name, path, licence, in image, note)
    ("opendroneid-core-c", "third_party/opendroneid-core-c", "Apache-2.0", False,
     "vendored C library; built only by crates/odid-ref for host tests, never linked into firmware"),
]


def tree_digest(path):
    """SHA-256 over the sorted (relative path, file SHA-256) list of a folder."""
    h = hashlib.sha256()
    for d, dirs, files in os.walk(path):
        dirs.sort()
        for f in sorted(files):
            p = os.path.join(d, f)
            h.update(f"{os.path.relpath(p, path)}\0{sha256_file(p)}\n".encode())
    return h.hexdigest()


def build(meta, lock, root_dir, image_name, commit, trees=None):
    pkgs = {p["id"]: p for p in meta["packages"]}
    root, image, build_only = classify_trees(meta, *trees) if trees else classify(meta)
    sums = lock_checksums(lock)
    rows, blob_rows = [], []
    for pid in sorted(image | build_only, key=lambda i: (pkgs[i]["name"], pkgs[i]["version"], i)):
        if pid == root:
            continue
        p = pkgs[pid]
        src = rel_source(p, root_dir)
        rows.append({
            "kind": "image" if pid in image else "build",
            "name": p["name"], "version": p["version"], "source": src,
            "licence": licence(p),
            "sha256": sums.get((p["name"], p["version"], p["source"] or ""), "-"),
            "patched": patched(root_dir, src),
        })
        if pid in image:
            for rel, h in blobs(p):
                blob_rows.append({"crate": f"{p['name']} {p['version']}", "file": rel, "sha256": h})
    vendored = []
    for name, rel, lic, in_image, note in VENDORED:
        full = os.path.join(root_dir, rel)
        vendored.append({"name": name, "path": rel, "licence": lic, "in_image": in_image, "note": note,
                         "tree_sha256": tree_digest(full) if os.path.isdir(full) else "missing"})
    rp = pkgs[root]
    return {"image": image_name, "commit": commit, "package": rp["name"], "version": rp["version"],
            "licence": licence(rp), "parts": rows, "blobs": blob_rows, "vendored": vendored}


def to_text(bom):
    out = [f"# Rotorscope parts list: {bom['image']} (package {bom['package']} {bom['version']}, {bom['licence']})",
           f"# commit {bom['commit']}",
           "# Made by tools/release-parts.py from cargo metadata and the lockfile. kind: image = linked into",
           "# the firmware; build = runs only on the build machine (build scripts, procedural macros).",
           "",
           "## Crates",
           "kind\tname\tversion\tsource\tlicence\tsha256 (lockfile)"]
    for r in bom["parts"]:
        out.append("\t".join([r["kind"], r["name"], r["version"], r["source"], r["licence"], r["sha256"]]))
    out += ["", "## Patched by this repository", "name\tversion\tsource\tcomes from\tnote"]
    for r in bom["parts"]:
        if r["patched"]:
            pt = r["patched"]
            frm = pt["from"] + (f" (SHA-256 {pt['from_sha256']})" if pt["from_sha256"] else "")
            out.append("\t".join([r["name"], r["version"], r["source"], frm, pt["note"]]))
    out += ["", "## Binary libraries inside crates the image is built from (the linker takes what the firmware uses)", "crate\tfile\tsha256"]
    for b in bom["blobs"]:
        out.append("\t".join([b["crate"], b["file"], b["sha256"]]))
    out += ["", "## Vendored code cargo does not see", "name\tpath\tlicence\tin image\ttree sha256\tnote"]
    for v in bom["vendored"]:
        out.append("\t".join([v["name"], v["path"], v["licence"], "yes" if v["in_image"] else "no",
                              v["tree_sha256"], v["note"]]))
    return "\n".join(out) + "\n"


def to_cyclonedx(bom):
    comps = []
    for r in bom["parts"]:
        c = {"type": "library", "bom-ref": f"{r['name']}@{r['version']}#{r['source']}",
             "name": r["name"], "version": r["version"],
             "scope": "required" if r["kind"] == "image" else "excluded",
             "licenses": [{"expression": r["licence"]}] if r["licence"] not in ("UNDECLARED",) and not r["licence"].startswith("see ") else [],
             "properties": [{"name": "rotorscope:source", "value": r["source"]},
                            {"name": "rotorscope:kind", "value": r["kind"]}]}
        if r["source"] == "crates.io":
            c["purl"] = f"pkg:cargo/{r['name']}@{r['version']}"
        if r["sha256"] != "-":
            c["hashes"] = [{"alg": "SHA-256", "content": r["sha256"]}]
        if r["patched"]:
            c["pedigree"] = {"notes": f"Patched copy of {r['patched']['from']}: {r['patched']['note']}"}
        if not c["licenses"]:
            del c["licenses"]
        comps.append(c)
    for b in bom["blobs"]:
        comps.append({"type": "file", "bom-ref": f"blob:{b['crate']}:{b['file']}",
                      "name": f"{b['crate'].split(' ')[0]}/{b['file']}", "version": b["crate"].split(" ")[1],
                      "scope": "required", "hashes": [{"alg": "SHA-256", "content": b["sha256"]}]})
    for v in bom["vendored"]:
        c = {"type": "library", "bom-ref": f"vendored:{v['path']}", "name": v["name"],
             "scope": "required" if v["in_image"] else "excluded",
             "licenses": [{"expression": v["licence"]}],
             "properties": [{"name": "rotorscope:source", "value": "path:" + v["path"]},
                            {"name": "rotorscope:note", "value": v["note"]}]}
        if v["tree_sha256"] != "missing":
            c["properties"].append({"name": "rotorscope:tree-sha256", "value": v["tree_sha256"]})
        comps.append(c)
    doc = {"bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
           "metadata": {"component": {"type": "firmware", "bom-ref": "rotorscope-" + bom["image"],
                                      "name": "rotorscope-" + bom["image"], "version": bom["commit"],
                                      "licenses": [{"expression": bom["licence"]}]}},
           "components": comps}
    # A serial number derived from the content, so the same parts give the same file.
    body = json.dumps(doc, sort_keys=True, separators=(",", ":")).encode()
    doc["serialNumber"] = "urn:uuid:" + str(uuid.UUID(bytes=hashlib.sha256(body).digest()[:16], version=5))
    return json.dumps(doc, indent=2, sort_keys=True) + "\n"


def self_test():
    fails = 0

    def check(name, ok):
        nonlocal fails
        print(("ok   " if ok else "FAIL ") + name)
        fails += 0 if ok else 1

    with tempfile.TemporaryDirectory() as t:
        repo = os.path.join(t, "repo")
        reg = os.path.join(t, "registry", "foo-1.0.0")
        for d in ("firmware/fw", "crates/a", "firmware/patches/esp-radio", "third_party/opendroneid-core-c", "libs"):
            os.makedirs(os.path.join(repo, d), exist_ok=True)
        os.makedirs(os.path.join(reg, "libs"))
        with open(os.path.join(reg, "libs", "libx.a"), "wb") as f:
            f.write(b"blob")
        with open(os.path.join(repo, "third_party/opendroneid-core-c/LICENSE"), "w") as f:
            f.write("Apache")
        lock = os.path.join(t, "Cargo.lock")
        with open(lock, "w") as f:
            f.write('version = 4\n[[package]]\nname = "foo"\nversion = "1.0.0"\n'
                    f'source = "{CRATES_IO}"\nchecksum = "{"ab" * 32}"\n'
                    '[[package]]\nname = "pm"\nversion = "2.0.0"\n'
                    f'source = "{CRATES_IO}"\nchecksum = "{"cd" * 32}"\n')

        def pkg(i, name, ver, src, mp, kind="lib", lic="MIT"):
            return {"id": i, "name": name, "version": ver, "source": src, "manifest_path": mp,
                    "license": lic, "license_file": None, "targets": [{"kind": [kind]}]}

        def dep(p, *kinds):
            return {"pkg": p, "dep_kinds": [{"kind": k, "target": None} for k in kinds]}

        meta = {"packages": [
            pkg("fw", "fw", "0.1.0", None, os.path.join(repo, "firmware/fw/Cargo.toml"), "bin", "GPL-3.0-or-later"),
            pkg("a", "a", "0.1.0", None, os.path.join(repo, "crates/a/Cargo.toml")),
            pkg("foo", "foo", "1.0.0", CRATES_IO, os.path.join(reg, "Cargo.toml"), lic="MIT OR Apache-2.0"),
            pkg("pm", "pm", "2.0.0", CRATES_IO, os.path.join(t, "pm/Cargo.toml"), "proc-macro"),
            pkg("er", "esp-radio", "1.0.0-beta.1", None, os.path.join(repo, "firmware/patches/esp-radio/Cargo.toml")),
            pkg("bd", "bd", "0.3.0", None, os.path.join(repo, "crates/bd/Cargo.toml")),
        ], "resolve": {"root": "fw", "nodes": [
            {"id": "fw", "deps": [dep("a", None), dep("pm", None), dep("bd", "build"), dep("er", None)]},
            {"id": "a", "deps": [dep("foo", None)]},
            {"id": "foo", "deps": []}, {"id": "pm", "deps": []}, {"id": "er", "deps": []}, {"id": "bd", "deps": []},
        ]}}
        bom = build(meta, lock, repo, "fw", "c0ffee")
        kinds = {r["name"]: r["kind"] for r in bom["parts"]}
        check("normal dependencies are in the image", kinds.get("a") == "image" and kinds.get("foo") == "image")
        check("procedural macros and build dependencies are build-only", kinds.get("pm") == "build" and kinds.get("bd") == "build")
        check("the firmware itself is not listed as a part", "fw" not in kinds)
        check("crates.io checksum comes from the lockfile", [r["sha256"] for r in bom["parts"] if r["name"] == "foo"] == ["ab" * 32])
        check("path crates are relative to the repository", [r["source"] for r in bom["parts"] if r["name"] == "a"] == ["path:crates/a"])
        check("binary libraries inside image crates are listed", [(b["file"], b["sha256"]) for b in bom["blobs"]] == [("libs/libx.a", hashlib.sha256(b"blob").hexdigest())])
        check("patched esp-radio carries its provenance", any(r["patched"] for r in bom["parts"] if r["name"] == "esp-radio"))
        check("vendored opendroneid-core-c is listed, not in the image", bom["vendored"][0]["name"] == "opendroneid-core-c" and not bom["vendored"][0]["in_image"])
        text, cdx = to_text(bom), to_cyclonedx(bom)
        check("no absolute path in the text list", t not in text)
        check("no absolute path in the CycloneDX document", t not in cdx)
        bom2 = build(meta, lock, repo, "fw", "c0ffee")
        check("same input, same files", to_text(bom2) == text and to_cyclonedx(bom2) == cdx)
        d = json.loads(cdx)
        check("CycloneDX has the required fields", d["bomFormat"] == "CycloneDX" and d["specVersion"] == "1.5"
              and d["version"] == 1 and all("name" in c and "type" in c for c in d["components"]))
        refs = [c["bom-ref"] for c in d["components"]]
        check("CycloneDX bom-refs are unique", len(refs) == len(set(refs)))
        ti, ta = os.path.join(t, "image.tree"), os.path.join(t, "all.tree")
        with open(ti, "w") as f:
            f.write(f"fw v0.1.0 ({repo}/firmware/fw)\na v0.1.0 ({repo}/crates/a)\nfoo v1.0.0\nfoo v1.0.0 (*)\nesp-radio v1.0.0-beta.1 ({repo}/firmware/patches/esp-radio)\n")
        with open(ta, "w") as f:
            f.write(open(ti).read() + "pm v2.0.0 (proc-macro)\n")
        bt = build(meta, lock, repo, "fw", "c0ffee", (parse_tree(ti), parse_tree(ta)))
        kt = {r["name"]: r["kind"] for r in bt["parts"]}
        check("cargo tree decides what is built: unlisted crates are left out", "bd" not in kt and kt.get("pm") == "build" and kt.get("foo") == "image")
        outside = dict(meta)
        outside["packages"] = meta["packages"] + [pkg("o", "o", "0", None, os.path.join(t, "elsewhere/Cargo.toml"))]
        outside["resolve"] = {"root": "fw", "nodes": meta["resolve"]["nodes"][1:] + [
            {"id": "fw", "deps": [dep("o", None)]}, {"id": "o", "deps": []}]}
        try:
            build(outside, lock, repo, "fw", "c0ffee")
            check("a path crate outside the repository is refused", False)
        except SystemExit:
            check("a path crate outside the repository is refused", True)
    print("release-parts self-test: " + ("all passed" if not fails else f"{fails} FAILED"))
    return 1 if fails else 0


def main():
    ap = argparse.ArgumentParser(description="Parts list (SBOM) for one firmware image")
    ap.add_argument("--self-test", action="store_true")
    ap.add_argument("--metadata")
    ap.add_argument("--tree-image")
    ap.add_argument("--tree-all")
    ap.add_argument("--lock")
    ap.add_argument("--root")
    ap.add_argument("--image")
    ap.add_argument("--commit")
    ap.add_argument("--text")
    ap.add_argument("--cyclonedx")
    a = ap.parse_args()
    if a.self_test:
        return self_test()
    for k in ("metadata", "lock", "root", "image", "commit", "text"):
        if not getattr(a, k):
            ap.error(f"--{k.replace('_', '-')} is required")
    with open(a.metadata) as f:
        meta = json.load(f)
    trees = None
    if a.tree_image or a.tree_all:
        if not (a.tree_image and a.tree_all):
            ap.error("--tree-image and --tree-all go together")
        trees = (parse_tree(a.tree_image), parse_tree(a.tree_all))
    bom = build(meta, a.lock, a.root, a.image, a.commit, trees)
    with open(a.text, "w") as f:
        f.write(to_text(bom))
    if a.cyclonedx:
        with open(a.cyclonedx, "w") as f:
            f.write(to_cyclonedx(bom))
    return 0


if __name__ == "__main__":
    sys.exit(main())
