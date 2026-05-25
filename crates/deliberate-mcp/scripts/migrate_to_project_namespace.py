#!/usr/bin/env python3
"""
Migrate the historical sessions directory into the project-namespaced
layout.

Round-3 fix context
-------------------
deliberate-mcp used to accept any caller-supplied `session_id` verbatim
and write the resulting JSON to `<sessions_dir>/<id>.json` — no project
namespace. That meant agents in different working directories could
clobber each other through bare names like `_default`, `phase3-chunk1`,
`alpha-demo`. The engine now auto-prefixes every caller-supplied id
with the project id (`<basename>-<6hex>__<rest>`), stamps the project
id into each session's metadata, and refuses cross-project writes.

This script rewrites the existing on-disk layout to match. For each
session file it:

  1. Determines the target project by:
       a) reading `history.steps[0].cwd` and re-deriving
          `<basename>-<6hex>` exactly the way the Rust side does, then
       b) falling back to matching the filename against the legacy
          rotation pattern `<base>-<6hex>(-YYYYMMDD-HHMMSS-XXXX)?`, then
       c) moving the file into `_legacy/` when nothing resolves.
  2. Computes the new filename:
       - bare project id, `<project>__<suffix>`, or `<project>-…-lega`
         (legacy rotation) all pass through unchanged.
       - Anything else gets the `<project>__<orig>` prefix, with the
         orig sanitized to fit the safe-id allowlist (alphanumeric +
         `_-.`), and the total clamped to 128 chars.
  3. Patches `history.metadata.project_id` to the resolved value.
  4. Writes atomically (temp + rename) and deletes the old file.

Cross-project sessions
----------------------
If a single file's steps come from multiple cwds (only `_default.json`
ever does in the current dataset, but the logic generalizes), the
script splits it into one file per cwd. Each split copy keeps only the
steps that match the corresponding cwd, plus the original metadata.

Safety
------
The script tarballs the entire `sessions/` directory to
`sessions-backup-<unix-ts>.tar.gz` before touching anything. If
something goes wrong you can `tar -xzf` over the directory to roll
back.

Usage
-----
    python3 scripts/migrate_to_project_namespace.py            # run
    python3 scripts/migrate_to_project_namespace.py --dry-run  # plan
    python3 scripts/migrate_to_project_namespace.py --data-dir <path>

The default data dir resolves the same way as the Rust side:
$DELIBERATE_DATA_DIR, $XDG_DATA_HOME/deliberate-mcp, or
~/.local/share/deliberate-mcp.
"""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import re
import shutil
import sys
import tarfile
import tempfile
import time
from collections import defaultdict
from typing import Any


SAFE_ID_RE = re.compile(r"^[A-Za-z0-9_.\-]+$")
PROJECT_SEP = "__"
# Mirrors src/config.rs::is_safe_session_id length cap.
MAX_ID_LEN = 128
# Mirrors the legacy auto-rotation suffix shape:
# `<base>-<6hex>-YYYYMMDD-HHMMSS-<4>` where <4> is alphanumeric.
LEGACY_SUFFIX_RE = re.compile(r"-\d{8}-\d{6}-[A-Za-z0-9]{4}$")
# `<base>-<6hex>` — the canonical project id shape.
PROJECT_ID_RE = re.compile(r"^([A-Za-z0-9_.\-]+?)-([0-9a-f]{6})$")


# ─── path → project_id (mirrors src/config.rs) ──────────────────────────


def fnv1a_64(data: bytes) -> int:
    """FNV-1a 64-bit, same as src/config.rs::path_hash6's inner loop."""
    h = 0xCBF29CE484222325
    prime = 0x100000001B3
    for b in data:
        h ^= b
        h = (h * prime) & 0xFFFFFFFFFFFFFFFF
    return h


def path_hash6(path: pathlib.Path) -> str:
    """Mirrors src/config.rs::path_hash6 — FNV-1a truncated to 24 bits."""
    raw = bytes(path)  # OS-native bytes, matches Rust's as_encoded_bytes
    h = fnv1a_64(raw)
    return f"{(h & 0xFFFFFF):06x}"


def sanitize_project_name(raw: str) -> str:
    """Mirrors src/config.rs::sanitize_project_name."""
    out = []
    last_was_replace = False
    for c in raw:
        if c.isascii() and (c.isalnum() or c in ("_", ".")):
            out.append(c.lower())
            last_was_replace = False
        elif not last_was_replace and out:
            out.append("-")
            last_was_replace = True
    trimmed = "".join(out).strip("-.")
    capped = trimmed[:32]
    return capped.rstrip("-.")


def project_id_from_cwd(cwd: str) -> str | None:
    """Re-derive the project id the same way the server does at startup."""
    try:
        path = pathlib.Path(cwd).resolve(strict=False)
    except OSError:
        return None
    if not str(path):
        return None
    name = path.name or "auto"
    basename = sanitize_project_name(name) or "auto"
    basename = basename[:24]
    return f"{basename}-{path_hash6(path)}"


# ─── filename / id manipulation ─────────────────────────────────────────


def sanitize_session_suffix(raw: str) -> str:
    """Reduce a free-form session id suffix to the safe allowlist so the
    combined `<project>__<suffix>` is acceptable to the engine."""
    out = []
    last_was_replace = False
    for c in raw:
        if c.isascii() and (c.isalnum() or c in ("_", "-", ".")):
            out.append(c)
            last_was_replace = False
        elif not last_was_replace and out:
            out.append("-")
            last_was_replace = True
    return "".join(out).strip("-.")


def namespace_session_id(project_id: str, raw: str) -> str:
    """Python port of src/config.rs::namespace_session_id."""
    raw = raw.strip()
    if not raw or raw == project_id:
        return project_id
    if raw.startswith(project_id + PROJECT_SEP):
        return raw
    if raw.startswith(project_id + "-"):
        return raw
    combined = f"{project_id}{PROJECT_SEP}{sanitize_session_suffix(raw)}"
    if len(combined) > MAX_ID_LEN:
        combined = combined[:MAX_ID_LEN].rstrip("-.")
    return combined


# ─── data-dir resolution ────────────────────────────────────────────────


def default_data_dir() -> pathlib.Path:
    explicit = os.environ.get("DELIBERATE_DATA_DIR")
    if explicit:
        return pathlib.Path(explicit)
    xdg = os.environ.get("XDG_DATA_HOME")
    if xdg:
        return pathlib.Path(xdg) / "deliberate-mcp"
    return pathlib.Path.home() / ".local" / "share" / "deliberate-mcp"


# ─── project resolution per file ────────────────────────────────────────


def project_for_session_file(payload: dict[str, Any], filename_stem: str) -> tuple[str | None, str]:
    """Return (project_id, reason). reason is for the action log."""
    history = payload.get("history") or {}
    steps = history.get("steps") or []
    # Prefer stamped metadata if a newer server already wrote it.
    meta = history.get("metadata") or {}
    if isinstance(meta, dict) and meta.get("project_id"):
        return meta["project_id"], "metadata.project_id"
    # Use the first step's cwd. This is the canonical signal.
    for step in steps:
        cwd = step.get("cwd")
        if cwd:
            pid = project_id_from_cwd(cwd)
            if pid:
                return pid, f"steps[].cwd ({cwd})"
    # Fall back to filename pattern: `<base>-<6hex>(-rotation)?`.
    stem = filename_stem
    # Strip legacy rotation suffix if present.
    stripped = LEGACY_SUFFIX_RE.sub("", stem)
    if PROJECT_ID_RE.match(stripped):
        return stripped, f"filename pattern ({stem})"
    return None, "no project signal — moving to _legacy/"


def split_by_cwd(payload: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """Cross-project files (the existing `_default.json` is the only
    real instance) get split into one payload per project. The returned
    dict is keyed by project_id; each value is a fresh history payload
    with only the steps belonging to that project."""
    history = payload.get("history") or {}
    steps = history.get("steps") or []
    by_project: dict[str, list[dict[str, Any]]] = defaultdict(list)
    for step in steps:
        cwd = step.get("cwd")
        pid = project_id_from_cwd(cwd) if cwd else None
        if not pid:
            continue
        by_project[pid].append(step)
    out: dict[str, dict[str, Any]] = {}
    for pid, group in by_project.items():
        sub_history = dict(history)
        sub_history["steps"] = group
        # Reset branches: cross-project branches are not meaningful.
        sub_history["branches"] = []
        meta = dict(sub_history.get("metadata") or {})
        meta["project_id"] = pid
        sub_history["metadata"] = meta
        out[pid] = {**payload, "history": sub_history}
    return out


# ─── target filename ────────────────────────────────────────────────────


def target_session_id(project_id: str, original_stem: str) -> str:
    """Decide what the renamed session id should be."""
    # `_default` → `<project>__default`. The `_` would be a leading
    # underscore otherwise, and we want a clean "default" suffix.
    if original_stem == "_default":
        return f"{project_id}{PROJECT_SEP}default"
    return namespace_session_id(project_id, original_stem)


# ─── IO helpers ─────────────────────────────────────────────────────────


def atomic_write(path: pathlib.Path, payload: dict[str, Any]) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, tmp_name = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp", dir=str(path.parent))
    tmp = pathlib.Path(tmp_name)
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(payload, f, indent=2)
            f.flush()
            os.fsync(f.fileno())
        tmp.replace(path)
    except BaseException:
        try:
            tmp.unlink()
        except FileNotFoundError:
            pass
        raise


def tarball_backup(sessions_dir: pathlib.Path) -> pathlib.Path:
    ts = int(time.time())
    out = sessions_dir.parent / f"sessions-backup-{ts}.tar.gz"
    with tarfile.open(out, "w:gz") as tar:
        tar.add(sessions_dir, arcname=sessions_dir.name)
    return out


# ─── main migration loop ────────────────────────────────────────────────


def plan_one(
    path: pathlib.Path,
    legacy_dir: pathlib.Path,
) -> list[tuple[pathlib.Path, pathlib.Path, dict[str, Any], str]]:
    """Return a list of (source, target, payload, reason) — one entry
    per output file. Most inputs produce one output; cross-project
    inputs produce one per project."""
    try:
        with path.open("r") as f:
            payload = json.load(f)
    except (json.JSONDecodeError, OSError) as e:
        print(f"  ! could not read {path.name}: {e}", file=sys.stderr)
        return []

    stem = path.stem

    # Cross-project detection: collect distinct project_ids across all
    # steps. If more than one, split.
    history = payload.get("history") or {}
    steps = history.get("steps") or []
    distinct_pids: set[str] = set()
    for step in steps:
        cwd = step.get("cwd")
        pid = project_id_from_cwd(cwd) if cwd else None
        if pid:
            distinct_pids.add(pid)

    if len(distinct_pids) >= 2:
        outputs = []
        for pid, sub_payload in split_by_cwd(payload).items():
            new_id = target_session_id(pid, stem)
            target = path.parent / f"{new_id}.json"
            outputs.append((path, target, sub_payload, f"split — {pid}"))
        return outputs

    pid, reason = project_for_session_file(payload, stem)
    if pid is None:
        target = legacy_dir / path.name
        # Still stamp metadata for completeness even on legacy moves.
        history = payload.setdefault("history", {})
        meta = history.setdefault("metadata", {}) or {}
        if isinstance(meta, dict):
            meta.setdefault("project_id", None)
        return [(path, target, payload, reason)]

    new_id = target_session_id(pid, stem)
    target = path.parent / f"{new_id}.json"

    # Patch metadata.project_id even if the rename is a no-op — keeps
    # the data self-describing.
    history = payload.setdefault("history", {})
    meta = history.get("metadata")
    if not isinstance(meta, dict):
        meta = {}
        history["metadata"] = meta
    meta["project_id"] = pid

    return [(path, target, payload, reason)]


def run(data_dir: pathlib.Path, dry_run: bool) -> int:
    sessions_dir = data_dir / "sessions"
    if not sessions_dir.is_dir():
        print(f"no sessions dir at {sessions_dir}", file=sys.stderr)
        return 1

    legacy_dir = sessions_dir / "_legacy"
    files = sorted(p for p in sessions_dir.glob("*.json") if p.is_file())
    if not files:
        print(f"no .json files in {sessions_dir}")
        return 0

    if not dry_run:
        backup = tarball_backup(sessions_dir)
        print(f"backup: {backup}")

    pending: list[tuple[pathlib.Path, pathlib.Path, dict[str, Any], str]] = []
    for path in files:
        pending.extend(plan_one(path, legacy_dir))

    # Detect collisions: two outputs writing to the same target. If we
    # find any, refuse to proceed — let the operator look.
    by_target: dict[pathlib.Path, int] = defaultdict(int)
    for _, target, _, _ in pending:
        by_target[target] += 1
    collisions = [t for t, n in by_target.items() if n > 1]
    if collisions:
        print("ERROR: would write multiple sources to one target:", file=sys.stderr)
        for t in collisions:
            print(f"  - {t}", file=sys.stderr)
        return 2

    # Apply (or print). Group by source for cleaner output.
    by_source: dict[pathlib.Path, list[tuple[pathlib.Path, dict[str, Any], str]]] = defaultdict(list)
    for src, target, payload, reason in pending:
        by_source[src].append((target, payload, reason))

    deletions: list[pathlib.Path] = []
    for src, outs in by_source.items():
        for target, payload, reason in outs:
            arrow = "would →" if dry_run else "→"
            rel_target = target.relative_to(sessions_dir) if target.is_relative_to(sessions_dir) else target
            print(f"  {src.name} {arrow} {rel_target}    [{reason}]")
            if not dry_run:
                atomic_write(target, payload)
        # Only delete the source after all outputs landed (single source
        # to multiple targets has to wait until every split is written).
        if not dry_run and not any(target == src for target, _, _ in outs):
            deletions.append(src)

    if not dry_run:
        for src in deletions:
            try:
                src.unlink()
            except FileNotFoundError:
                pass

    suffix = "would migrate" if dry_run else "migrated"
    print(f"\n{suffix} {len(pending)} file(s) from {len(files)} input(s).")
    return 0


def main(argv: list[str]) -> int:
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument("--data-dir", type=pathlib.Path, default=default_data_dir())
    p.add_argument("--dry-run", action="store_true", help="print the plan without touching disk")
    args = p.parse_args(argv)
    return run(args.data_dir, args.dry_run)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv[1:]))
