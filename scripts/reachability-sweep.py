#!/usr/bin/env python3
"""Reachability sweep: pub fns in crates/ whose only callers are tests (or nothing).

    python3 scripts/reachability-sweep.py crates

A REVIEW TOOL, deliberately not a CI gate. See the measured reason below.

Written for untested-reachability-sweep, after four consecutive runs each found
by accident the same defect shape: code that is correct-looking, gate-green, and
never EXECUTED on the path that matters. The precedent: `tracker::sweep::reconcile`
was correct, fully tested, and called by nothing outside the test suite for three
sagas, so "state recovers without webhooks" was true of the library and false of
the product.

Classifies each source LINE as production or test:
  * any file under a `tests/` directory      -> test
  * any line inside a `#[cfg(test)]` module   -> test (brace-counted)
  * `#[test]` / `#[tokio::test]` fn bodies    -> test (brace-counted)

WHY THIS IS NOT A BLOCKING GATE — measured on this repo, 2026-07-25:
  41 candidates from 523 distinct pub fn names. Of those, exactly ONE was a
  broken product promise (`propose_status_from_sweep`), one was a superseded
  abstraction (`FamilyRegistry`), and three were trivially dead. The rest were
  legitimate: FakeTracker builders, `with_*` constructors, test-only accessors.
  Roughly 12% actionable. A gate at that precision needs a ~36-entry allowlist
  that rots faster than it helps, and it would have to be maintained by the same
  people it is meant to catch.

KNOWN BLIND SPOTS, both demonstrated rather than assumed:
  * String-literal references are invisible, because the scanner strips string
    literals so a name inside a doc comment or message cannot fake a call. All
    six `infra/coerce.rs` hits were false positives for exactly this reason —
    serde reaches them through `deserialize_with = "..."`.
  * Dynamic dispatch is not tracked. A `dyn Trait` method has no textual caller.

So: run it when you want to hunt, read every hit, and judge each against the
only question that matters — what promise breaks if this never runs?
"""
import re
import sys
from pathlib import Path
from collections import defaultdict

ROOT = Path(sys.argv[1] if len(sys.argv) > 1 else "crates")

DEF_RE = re.compile(r"^\s*pub(?:\([^)]*\))?\s+(?:const\s+|async\s+|unsafe\s+|extern\s+\"[^\"]*\"\s+)*fn\s+(\w+)")
IMPL_RE = re.compile(r"^\s*impl\b(.*)$")
CFG_TEST_RE = re.compile(r"^\s*#\[cfg\(test\)\]")
TEST_ATTR_RE = re.compile(r"^\s*#\[(?:tokio::)?test\b|^\s*#\[test\]")
IDENT_RE = re.compile(r"\b(\w+)\b")


def strip_code(line: str) -> str:
    """Drop // comments and string literals so they don't fake a reference."""
    out, i, n = [], 0, len(line)
    while i < n:
        if line[i] == "/" and i + 1 < n and line[i + 1] == "/":
            break
        if line[i] == '"':
            i += 1
            while i < n and line[i] != '"':
                i += 2 if line[i] == "\\" else 1
            i += 1
            continue
        out.append(line[i])
        i += 1
    return "".join(out)


def analyse(path: Path):
    """Yield (lineno, line, is_test, impl_ctx) plus the pub-fn defs found."""
    raw = path.read_text(errors="replace").splitlines()
    in_tests_dir = "tests" in path.parts
    lines, defs = [], []

    test_depth = None       # brace depth at which the current test region ends
    depth = 0
    pending_test = False    # saw #[cfg(test)] / #[test], awaiting its block
    impl_stack = []         # (depth, impl header)

    for idx, raw_line in enumerate(raw, 1):
        code = strip_code(raw_line)
        opens, closes = code.count("{"), code.count("}")
        start_depth = depth

        is_test = in_tests_dir or (test_depth is not None)

        m = IMPL_RE.match(code)
        if m:
            impl_stack.append((start_depth, m.group(1).strip()))

        d = DEF_RE.match(code)
        if d:
            impl_ctx = impl_stack[-1][1] if impl_stack else ""
            defs.append((d.group(1), idx, is_test, impl_ctx))

        lines.append((idx, code, is_test))

        if CFG_TEST_RE.match(code) or TEST_ATTR_RE.match(code):
            pending_test = True
        if pending_test and opens > 0 and test_depth is None:
            test_depth = start_depth
            pending_test = False

        depth += opens - closes
        if test_depth is not None and depth <= test_depth:
            test_depth = None
        while impl_stack and depth <= impl_stack[-1][0]:
            impl_stack.pop()

    return lines, defs


def main():
    files = sorted(ROOT.rglob("*.rs"))
    all_defs = {}            # name -> list of (path, lineno, is_test, impl_ctx)
    prod_refs = defaultdict(list)
    test_refs = defaultdict(list)

    parsed = {}
    for f in files:
        lines, defs = analyse(f)
        parsed[f] = lines
        for name, lineno, is_test, impl_ctx in defs:
            all_defs.setdefault(name, []).append((f, lineno, is_test, impl_ctx))

    def_sites = {(f, ln) for v in all_defs.values() for (f, ln, _, _) in v}

    for f, lines in parsed.items():
        for lineno, code, is_test in lines:
            if (f, lineno) in def_sites:
                continue          # the definition line is not a reference
            for ident in IDENT_RE.findall(code):
                if ident in all_defs:
                    (test_refs if is_test else prod_refs)[ident].append((f, lineno))

    orphan, test_only = [], []
    for name, sites in sorted(all_defs.items()):
        prod_sites = [s for s in sites if not s[2]]
        if not prod_sites:
            continue              # a fn defined only in test code is not the target
        p, t = len(prod_refs[name]), len(test_refs[name])
        if p == 0:
            (test_only if t else orphan).append((name, prod_sites, t))

    print(f"scanned {len(files)} files, {len(all_defs)} distinct pub fn names\n")
    print(f"=== A. TEST-ONLY CALLERS ({len(test_only)}) — the sweep::reconcile shape ===")
    for name, sites, t in test_only:
        loc = ", ".join(f"{f}:{ln}" for f, ln, _, _ in sites)
        ctx = sites[0][3]
        print(f"  {name:<44} {t:>3} test refs   {loc}" + (f"   [impl {ctx}]" if ctx else ""))
    print(f"\n=== B. NO CALLERS AT ALL ({len(orphan)}) ===")
    for name, sites, _ in orphan:
        loc = ", ".join(f"{f}:{ln}" for f, ln, _, _ in sites)
        ctx = sites[0][3]
        print(f"  {name:<44}              {loc}" + (f"   [impl {ctx}]" if ctx else ""))


if __name__ == "__main__":
    main()
