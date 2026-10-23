#!/usr/bin/env python3
"""Regenerate resources/lucide/icons.tsv from the lucide-static npm package.

Usage: scripts/update-lucide-icons.py [VERSION | PATH_TO_EXTRACTED_PACKAGE]

Each data line is `name<TAB>tag|tag|...<TAB>inner SVG markup`. Only the canonical icon
set (icon-nodes.json, as listed on https://lucide.dev/icons) is embedded; deprecated
aliases are skipped. Lucide is ISC licensed (see licenses/Lucide-LICENSE.txt).
"""
import json
from pathlib import Path
import re
import subprocess
import sys
import tarfile
import tempfile
from xml.sax.saxutils import quoteattr

ROOT = Path(__file__).resolve().parent.parent
OUTPUT = ROOT / "resources" / "lucide" / "icons.tsv"
NAME_RE = re.compile(r"[a-z0-9]+(-[a-z0-9]+)*")
SVG_TAGS = {"path", "circle", "ellipse", "line", "polygon", "polyline", "rect"}


def clean_tag(tag: str) -> str:
    return " ".join(tag.replace("\t", " ").replace("|", " ").split())


def render(nodes) -> str:
    out = []
    for tag, attrs in nodes:
        if tag not in SVG_TAGS:
            raise SystemExit(f"Unexpected SVG element: {tag}")
        attributes = "".join(
            f" {key}={quoteattr(str(value))}" for key, value in attrs.items() if key != "key"
        )
        out.append(f"<{tag}{attributes}/>")
    return "".join(out)


def generate(package: Path) -> None:
    meta = json.loads((package / "package.json").read_text())
    nodes = json.loads((package / "icon-nodes.json").read_text())
    tags = json.loads((package / "tags.json").read_text())
    lines = [f"# lucide-static {meta['version']} · ISC · https://lucide.dev/icons"]
    for name in sorted(nodes):
        if not NAME_RE.fullmatch(name):
            raise SystemExit(f"Unexpected icon name: {name}")
        keywords = "|".join(filter(None, (clean_tag(tag) for tag in tags.get(name, []))))
        lines.append(f"{name}\t{keywords}\t{render(nodes[name])}")
    OUTPUT.parent.mkdir(parents=True, exist_ok=True)
    OUTPUT.write_text("\n".join(lines) + "\n")
    print(f"Wrote {len(nodes)} icons from lucide-static {meta['version']} to {OUTPUT.relative_to(ROOT)}")


def main() -> None:
    arg = sys.argv[1] if len(sys.argv) > 1 else "latest"
    if Path(arg).is_dir():
        generate(Path(arg))
        return
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(
            ["npm", "pack", f"lucide-static@{arg}", "--silent", "--pack-destination", tmp],
            check=True,
            stdout=subprocess.DEVNULL,
        )
        archive = next(Path(tmp).glob("lucide-static-*.tgz"))
        with tarfile.open(archive) as tar:
            tar.extractall(tmp, filter="data")
        generate(Path(tmp) / "package")


if __name__ == "__main__":
    main()
