#!/usr/bin/env python3
"""Check assets/i18n/<lang>.json against the strings used in src/.

Source strings are the literals passed to tr("...") / trf!("...").
Fails (exit 1) on stale keys (not in the sources) or placeholder mismatches;
reports untranslated strings per language. `--list` prints the source strings.
"""
import glob, json, os, re, sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
PAT = re.compile(r'(?<![\w])(?:tr|trf!)\(\s*"((?:[^"\\]|\\.)*)"')


def unescape(s):
    s = re.sub(r"\\u\{([0-9a-fA-F]+)\}", lambda m: chr(int(m.group(1), 16)), s)
    return json.loads('"' + s.replace("\\'", "'") + '"')


def source_strings():
    out = []
    for f in sorted(glob.glob(os.path.join(ROOT, "src", "**", "*.rs"), recursive=True)):
        if f.endswith("i18n.rs"):
            continue
        for m in PAT.finditer(open(f, encoding="utf-8").read()):
            t = unescape(m.group(1))
            if t not in out:
                out.append(t)
    return out


def placeholders(s):
    return set(re.findall(r"\{(\w+)\}", s))


def main():
    src = source_strings()
    if "--list" in sys.argv:
        print(json.dumps(src, ensure_ascii=False, indent=1))
        return 0
    bad = 0
    print(f"{len(src)} source strings")
    for f in sorted(glob.glob(os.path.join(ROOT, "assets", "i18n", "*.json"))):
        lang = os.path.basename(f)[:-5]
        cat = json.load(open(f, encoding="utf-8"))
        missing = [s for s in src if not cat.get(s)]
        print(f"{lang}: {len(cat)} keys, {len(missing)} untranslated")
        for s in missing[:20]:
            print(f"    untranslated: {s!r}")
        for k, v in cat.items():
            if k not in src:
                print(f"    STALE key: {k!r}")
                bad = 1
            elif v and placeholders(k) != placeholders(v):
                print(f"    PLACEHOLDERS differ: {k!r} -> {v!r}")
                bad = 1
    return bad


if __name__ == "__main__":
    sys.exit(main())
