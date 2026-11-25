#!/usr/bin/env python3
"""CLI analyzer for `PalRemake_CoreDiag_v1` bug reports (F9 / system menu "copy replay").

A report holds the frame-exact input replay (`replay.seed` + `replay.log` of logic-tick inputs)
and the canonical state at the time of the report.

Usage:
  python3 tools/inspect_report.py path/to/report.palrec.json
"""

from __future__ import annotations
import json
import re
import sys
from pathlib import Path


def parse_report(raw_text: str) -> dict:
    m = re.search(r"```(?:json)?\s*([\s\S]*?)```", raw_text)
    body = m.group(1).strip() if m else raw_text.strip()
    return json.loads(body)


def summarize_report(doc: dict) -> str:
    if doc.get("kind") != "PalRemake_CoreDiag_v1":
        raise ValueError(f"unsupported report kind: {doc.get('kind')!r}")
    st = doc.get("state", {})
    rep = doc.get("replay", {})
    log = rep.get("log", [])
    lead = (st.get("party") or [{}])[0]
    lines = [
        "=== PalRemake Core Diagnostic Summary ===",
        f"Note        : {doc.get('note') or '(none)'}",
        f"Scene       : #{st.get('sceneId')} (Map {st.get('mapId')}) cash={st.get('cash')}",
        f"Leader Pos  : ({lead.get('x')}, {lead.get('y')}) dir={lead.get('dir')}",
        f"Inventory   : {st.get('inventory')}",
        f"Dirty Events: {len(st.get('dirtyEvents', []))}",
        f"Replay      : seed={rep.get('seed')} {len(log)} inputs, last tick {log[-1]['t'] if log else 0}",
    ]
    for i, e in enumerate(log[:25]):
        kind = "key" if "k" in e else "pointer" if "p" in e else "input"
        lines.append(f"  [{i:02d}] tick {e.get('t')} {kind} {e.get('k') or e.get('p') or e.get('i')}")
    return "\n".join(lines)

if __name__ == "__main__":
    src = Path(sys.argv[1]).read_text(encoding="utf-8") if len(sys.argv) > 1 else sys.stdin.read()
    print(summarize_report(parse_report(src)))
