#!/usr/bin/env python3
"""
Migrate the historical _default.json into per-project session files.

What this does:
  1. Reads ~/.local/share/deliberate-mcp/sessions/_default.json
  2. Clusters its steps by step_number resets (each #1 starts a new conversation)
  3. Classifies each cluster to a project root by content keywords
  4. Writes a per-cluster session file named
        <basename>-<6hex>-YYYYMMDD-HHMMSS-lega.json
     where <6hex> is FNV-1a of the canonicalized project path —
     matching what `deliberate-mcp` itself generates for new
     auto-sessions, so the dropdown groups them naturally.
  5. Deletes _default.json once the new files are safely written.

Safety:
  - You MUST quit Claude Code (Cmd-Q) before running this so the live
    MCP server's in-memory copy of _default doesn't resurrect on its
    next persist.
  - All new files are atomic-written via tempfile + rename.
  - On any error after step 5's deletion, the previous file content is
    still recoverable from the per-session files.
"""

from __future__ import annotations

import json
import os
import pathlib
import sys
import tempfile
from datetime import datetime, timezone

SESSIONS_DIR = pathlib.Path.home() / ".local/share/deliberate-mcp/sessions"
DEFAULT_FILE = SESSIONS_DIR / "_default.json"
SCHEMA_VERSION = 1

# Project paths confirmed by the user.
PROJECT_ROOTS = {
    "deliberate-mcp": "/Users/alrik/Code/deliberate-mcp",
    "rikttp": "/Users/alrik/Code/rikttp",
    "ministr": "/Users/alrik/Code/ministr",
}

# Marker keywords per project. The classifier picks the project with
# the highest hit count for the cluster's combined text.
MARKERS = {
    "deliberate-mcp": [
        "smoke-test", "stepappended", "deliberate.sock", "broadcast socket",
        "tauri", "viewer", "auto-session",
    ],
    "rikttp": [
        "channel::send", "quic", "phase iv", "phase v", "phase vi", "phase vii",
        "peer migration", "path validation", "rtt", "xxh3", "parse_fast",
        "wire format", "frame type", "retransmit", "handshake",
    ],
    "ministr": [
        "pulumi", "aca", "azure files", "createapp", "ministr_", "app insights",
        "mcp.ministr.ai", "blob storage", "postgres", "vendor accounts",
        "deploy/azure", "image rollout",
    ],
}


def fnv1a64(data: bytes) -> int:
    h = 0xcbf29ce484222325
    PRIME = 0x100000001b3
    for b in data:
        h ^= b
        h = (h * PRIME) & 0xffffffffffffffff
    return h


def path_hash6(path: str) -> str:
    canonical = str(pathlib.Path(path).resolve())
    return f"{(fnv1a64(canonical.encode('utf-8')) & 0xffffff):06x}"


def classify(cluster: list[dict]) -> str:
    blob = " ".join(
        f"{s.get('thought','')} {s.get('outcome','')} {s.get('purpose','')} {s.get('context','')}"
        for s in cluster
    ).lower()
    scores = {p: sum(blob.count(k) for k in kws) for p, kws in MARKERS.items()}
    best = max(scores.items(), key=lambda x: x[1])
    if best[1] == 0:
        raise RuntimeError(f"Could not classify cluster of {len(cluster)} steps; scores={scores}")
    return best[0]


def cluster_by_step_reset(steps: list[dict]) -> list[list[dict]]:
    clusters: list[list[dict]] = []
    current: list[dict] = []
    for s in steps:
        if s.get("step_number") == 1 and current:
            clusters.append(current)
            current = []
        current.append(s)
    if current:
        clusters.append(current)
    return clusters


def session_id_for(project_key: str, first_step: dict) -> str:
    ts = first_step.get("timestamp", "")
    if not ts:
        raise RuntimeError("step missing timestamp; cannot generate session id")
    dt = datetime.fromisoformat(ts.replace("Z", "+00:00")).astimezone(timezone.utc)
    yyyymmdd = dt.strftime("%Y%m%d")
    hhmmss = dt.strftime("%H%M%S")
    hash6 = path_hash6(PROJECT_ROOTS[project_key])
    basename = pathlib.Path(PROJECT_ROOTS[project_key]).name
    # The trailing "-lega" mirrors the auto-session's 4-char suffix
    # slot and flags the file as migrated (not hex).
    return f"{basename}-{hash6}-{yyyymmdd}-{hhmmss}-lega"


def atomic_write(path: pathlib.Path, payload: dict) -> None:
    parent = path.parent
    fd, tmp_str = tempfile.mkstemp(prefix=path.name + ".", suffix=".tmp", dir=parent)
    tmp = pathlib.Path(tmp_str)
    try:
        with os.fdopen(fd, "w") as f:
            json.dump(payload, f, indent=2)
        os.replace(tmp, path)
    except Exception:
        tmp.unlink(missing_ok=True)
        raise


def build_history(session_id: str, steps: list[dict], cwd: str) -> dict:
    # Annotate each step with the session_id and project cwd so the
    # data is self-describing after migration.
    enriched = []
    for s in steps:
        new = dict(s)
        new["session_id"] = session_id
        # cwd is the new server-set field; populate it now too so the
        # migrated data matches what new steps look like.
        new["cwd"] = cwd
        enriched.append(new)

    first_ts = steps[0].get("timestamp")
    last_ts = steps[-1].get("timestamp")
    return {
        "schema_version": SCHEMA_VERSION,
        "history": {
            "steps": enriched,
            "branches": [],
            "completed": False,
            "session_id": session_id,
            "created_at": first_ts,
            "updated_at": last_ts,
            "metadata": {
                "total_duration_ms": 0,
                "revisions_count": 0,
                "branches_created": 0,
                "tools_used": [],
            },
        },
    }


def assert_server_down() -> None:
    import subprocess
    try:
        out = subprocess.check_output(["pgrep", "-f", "deliberate-mcp$"], text=True)
    except subprocess.CalledProcessError:
        return  # no process — good
    pids = [p for p in out.strip().splitlines() if p]
    if pids:
        sys.exit(
            f"ABORTING: deliberate-mcp is still running (pids {pids}). "
            "Quit Claude Code (Cmd-Q) first, then re-run this script."
        )


def main() -> int:
    if not DEFAULT_FILE.exists():
        print(f"No file at {DEFAULT_FILE}, nothing to migrate.")
        return 0

    assert_server_down()

    raw = json.loads(DEFAULT_FILE.read_text())
    steps = raw.get("history", {}).get("steps", [])
    if not steps:
        print(f"{DEFAULT_FILE} has no steps; deleting it.")
        DEFAULT_FILE.unlink()
        return 0

    clusters = cluster_by_step_reset(steps)
    print(f"Detected {len(clusters)} conversation(s) by step-number reset.\n")

    plan: list[tuple[str, str, list[dict]]] = []
    for i, c in enumerate(clusters, 1):
        project = classify(c)
        sid = session_id_for(project, c[0])
        cwd = PROJECT_ROOTS[project]
        first_ts = c[0].get("timestamp", "")[:19]
        last_ts = c[-1].get("timestamp", "")[:19]
        print(f"  cluster {i}: {len(c):>3} steps  →  {sid}")
        print(f"             {first_ts} → {last_ts}  ({cwd})")
        plan.append((sid, cwd, c))

    print()
    # Write all the new files first; only delete _default once they're
    # all safely on disk.
    written: list[pathlib.Path] = []
    try:
        for sid, cwd, cluster in plan:
            out = SESSIONS_DIR / f"{sid}.json"
            if out.exists():
                raise RuntimeError(f"target file already exists, refusing to overwrite: {out}")
            atomic_write(out, build_history(sid, cluster, cwd))
            written.append(out)
            print(f"  wrote {out.name}")
    except Exception as e:
        print(f"\nERROR: {e}\nRolling back partial writes...")
        for p in written:
            p.unlink(missing_ok=True)
        return 1

    DEFAULT_FILE.unlink()
    print(f"\nDeleted {DEFAULT_FILE.name}. Migration complete.")
    print(f"Sessions in dir now:")
    for p in sorted(SESSIONS_DIR.glob("*.json")):
        print(f"  {p.name}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
