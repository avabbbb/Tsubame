#!/usr/bin/env python3
from __future__ import annotations

import json
import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

REQUIRED = [
    "README.md",
    "AGENTS.md",
    "docs/00-vision.md",
    "docs/01-product.md",
    "docs/02-architecture.md",
    "docs/03-ui-system.md",
    "docs/04-domain-model.md",
    "docs/05-processing-providers.md",
    "docs/06-runtime-bootstrap.md",
    "docs/07-agent-control.md",
    "docs/08-source-adapters.md",
    "docs/09-security-rights.md",
    "docs/10-roadmap.md",
    "docs/DECISIONS.md",
    "docs/UPSTREAM.md",
    ".agents/skills/yuzuki/SKILL.md",
]

for rel in REQUIRED:
    path = ROOT / rel
    if not path.exists():
        raise SystemExit(f"missing canonical file: {rel}")

for path in list((ROOT / "schemas").glob("*.json")) + list((ROOT / "examples").glob("*.json")):
    with path.open("r", encoding="utf-8") as f:
        json.load(f)

md_link = re.compile(r"\[[^\]]+\]\(([^)]+)\)")
for path in [ROOT / "README.md", *ROOT.glob("docs/*.md")]:
    text = path.read_text(encoding="utf-8")
    for raw in md_link.findall(text):
        target = raw.split("#", 1)[0].strip()
        if not target or "://" in target or target.startswith("mailto:"):
            continue
        candidate = (path.parent / target).resolve()
        try:
            candidate.relative_to(ROOT.resolve())
        except ValueError:
            raise SystemExit(f"{path.relative_to(ROOT)}: link escapes repo: {raw}")
        if not candidate.exists():
            raise SystemExit(f"{path.relative_to(ROOT)}: broken relative link: {raw}")

print("docs-check: ok")
