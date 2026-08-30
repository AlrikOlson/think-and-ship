#!/usr/bin/env python3
"""Make the release-plz PR self-consistent — run on its branch, after release-plz.

Two things release-plz does not do for this repository, applied idempotently:

1. ``npm/think-and-ship/package.json`` follows ``crates/think-and-ship/Cargo.toml``.
   release-plz bumps Cargo.toml only; the npm wrapper ships the same binary under
   the same version, and ci.yml's ``versions`` check refuses the PR until they agree.
   (v0.5.0 and v0.5.1 both needed this by hand.)

2. The new ``CHANGELOG.md`` section carries every ``feat``/``fix`` commit on the
   base branch since the last ``v*`` tag. release-plz derives the section from a
   walk back through history to the previous release; a non-linear stretch can
   stop that walk early and drop entries (v0.5.1 lost its ``fix(roadmap)`` this
   way). The walk here is every commit reachable from the base and not from the
   tag — on the squash-only, linear ``main`` the ruleset enforces that is the
   first-parent line, and on a merge it would still see the side branch — and a
   commit already rendered by release-plz (matched on its description) is left alone.

Exit 0 whether or not anything changed; the caller decides whether to commit.
``--check`` exits 1 instead of writing, for a dry run.

    scripts/release/sync-release-pr.py --base origin/main [--check]
"""

from __future__ import annotations

import argparse
import json
import re
import subprocess
import sys
from pathlib import Path

CARGO_TOML = Path("crates/think-and-ship/Cargo.toml")
PACKAGE_JSON = Path("npm/think-and-ship/package.json")
CHANGELOG = Path("CHANGELOG.md")

# Conventional-Commit type -> Keep-a-Changelog heading, mirroring
# release-plz.toml's commit_parsers. Types not listed are skipped there too.
HEADINGS = {"feat": "Added", "fix": "Fixed"}
# Keep-a-Changelog order, so an inserted heading lands where a reader expects it.
HEADING_ORDER = ["Added", "Changed", "Deprecated", "Removed", "Fixed", "Security", "Documentation"]

SUBJECT = re.compile(r"^(?P<type>[a-z]+)(?:\((?P<scope>[^)]*)\))?(?P<bang>!)?:\s*(?P<desc>.+?)\s*$")


def git(*args: str) -> str:
    return subprocess.run(["git", *args], check=True, capture_output=True, text=True).stdout


def cargo_version() -> str:
    in_package = False
    for line in CARGO_TOML.read_text().splitlines():
        if line.startswith("["):
            in_package = line.strip() == "[package]"
            continue
        if in_package:
            m = re.match(r'^version\s*=\s*"([^"]+)"', line)
            if m:
                return m.group(1)
    sys.exit(f"{CARGO_TOML}: no [package] version")


def sync_package_json(version: str, check: bool) -> bool:
    doc = json.loads(PACKAGE_JSON.read_text())
    if doc.get("version") == version:
        return False
    print(f"package.json: {doc.get('version')} -> {version}")
    if not check:
        doc["version"] = version
        # ensure_ascii=False: the description carries an em dash, and a version
        # sync must change the version and nothing else.
        PACKAGE_JSON.write_text(json.dumps(doc, indent=2, ensure_ascii=False) + "\n")
    return True


def previous_tag(base: str) -> str | None:
    try:
        return git("describe", "--tags", "--abbrev=0", "--match", "v*", base).strip() or None
    except subprocess.CalledProcessError:
        return None


def notable_commits(base: str, since: str | None) -> list[tuple[str, str]]:
    """(heading, description) for every feat/fix since the last release."""
    rev_range = f"{since}..{base}" if since else base
    out = []
    for subject in git("log", "--no-merges", "--format=%s", rev_range).splitlines():
        m = SUBJECT.match(subject)
        if not m:
            continue
        heading = HEADINGS.get(m.group("type"))
        if heading is None:
            continue
        if m.group("bang"):
            heading = "Changed" if heading == "Added" else heading
        out.append((heading, m.group("desc")))
    return out


def section_bounds(lines: list[str], version: str) -> tuple[int, int] | None:
    start = next((i for i, l in enumerate(lines) if l.startswith(f"## [{version}]")), None)
    if start is None:
        return None
    end = next((i for i in range(start + 1, len(lines)) if lines[i].startswith("## [")), len(lines))
    return start, end


def entry_key(text: str) -> str:
    """The first five words, lower-cased and stripped of punctuation.

    A rendered changelog line is often hand-edited into a fuller sentence than
    the commit subject; its opening words survive that, so a commit is
    "present" when some bullet in the section opens the same way (or contains
    the description verbatim). Five words is enough that two commits with the
    same opening are the same change said twice.
    """
    return " ".join(re.findall(r"[a-z0-9]+", text.lower())[:5])


SCOPE_MARKER = re.compile(r"^\*\([^)]*\)\*\s*")


def bullet_text(line: str) -> str:
    """A bullet's text without release-plz's optional ``*(scope)*`` marker."""
    return SCOPE_MARKER.sub("", line[2:])


def present(desc: str, section_lines: list[str]) -> bool:
    if any(desc in l for l in section_lines):
        return True
    key = entry_key(desc)
    return bool(key) and any(
        entry_key(bullet_text(l)) == key for l in section_lines if l.startswith("- ")
    )


def complete_changelog(version: str, base: str, check: bool) -> bool:
    lines = CHANGELOG.read_text().splitlines()
    bounds = section_bounds(lines, version)
    if bounds is None:
        # Not a no-op: without its section the release has no changelog and
        # nothing here can be completed. Fail so the job — and `--check` —
        # is red rather than reporting a consistent release PR.
        sys.exit(
            f"CHANGELOG.md: no `## [{version}]` section — release-plz did not render one "
            "or it was edited away; the release cannot proceed without it"
        )
    start, end = bounds
    # Presence is checked against the section plus what this run has already
    # accepted, so two unreleased commits saying the same thing land once.
    section_lines = list(lines[start:end])
    missing: list[tuple[str, str]] = []
    for heading, desc in notable_commits(base, previous_tag(base)):
        if present(desc, section_lines):
            continue
        missing.append((heading, desc))
        section_lines.append(f"- {desc}")
    if not missing:
        return False
    for heading, desc in missing:
        print(f"CHANGELOG.md [{version}]: + {heading}: {desc}")
    if check:
        return True

    body = lines[start + 1:end]
    for heading, desc in missing:
        marker = f"### {heading}"
        try:
            at = body.index(marker)
        except ValueError:
            # Insert the heading in Keep-a-Changelog order among those present.
            headings = [(i, l[4:]) for i, l in enumerate(body) if l.startswith("### ")]
            rank = HEADING_ORDER.index(heading) if heading in HEADING_ORDER else len(HEADING_ORDER)
            insert_at = len(body)
            for i, name in headings:
                if name in HEADING_ORDER and HEADING_ORDER.index(name) > rank:
                    insert_at = i
                    break
            # Keep one blank line around the heading.
            while insert_at > 0 and body[insert_at - 1].strip() == "":
                insert_at -= 1
            body[insert_at:insert_at] = ["", marker, ""]
            at = insert_at + 1
        # Append at the end of the heading's block — everything up to the next
        # heading, so a multi-line bullet (a wrapped sentence on indented
        # continuation lines) is never split — before the block's trailing
        # blank lines. A heading with no bullets yet gets one blank line under
        # it first, the way release-plz renders.
        block_end = at + 1
        while block_end < len(body) and not body[block_end].startswith("### "):
            block_end += 1
        k = block_end
        while k > at + 1 and body[k - 1].strip() == "":
            k -= 1
        if k == at + 1:
            body[k:k] = ["", f"- {desc}"]
        else:
            body.insert(k, f"- {desc}")
    # Normalise: at most one blank line between blocks, one trailing blank.
    out: list[str] = []
    for l in body:
        if l.strip() == "" and out and out[-1].strip() == "":
            continue
        out.append(l)
    if not out or out[-1].strip() != "":
        out.append("")
    lines[start + 1:end] = out
    CHANGELOG.write_text("\n".join(lines) + "\n")
    return True


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--base", default="origin/main", help="the release PR's base ref (default origin/main)")
    ap.add_argument("--check", action="store_true", help="report what would change and exit 1 if anything")
    args = ap.parse_args()

    version = cargo_version()
    changed = sync_package_json(version, args.check)
    changed = complete_changelog(version, args.base, args.check) or changed
    if not changed:
        print(f"release PR is consistent for {version}")
        return 0
    return 1 if args.check else 0


if __name__ == "__main__":
    sys.exit(main())
