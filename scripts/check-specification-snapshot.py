#!/usr/bin/env python3
"""Check the committed design snapshot against its recorded revision inventory.

This validates local consistency, not Google identity, signatures or freshness.
It performs no network access and never edits the specification.
"""
import hashlib
import json
import re
from pathlib import Path


def check(root: Path) -> None:
    metadata = json.loads((root / "docs/specification-source.json").read_text(encoding="utf-8"))
    raw = (root / "docs/specification.txt").read_bytes()
    version = metadata["schema_version"]
    if type(version) is not int or version not in (1, 2):
        raise ValueError("unsupported specification metadata schema")
    if hashlib.sha256(raw).hexdigest() != metadata["snapshot_sha256"]:
        raise ValueError("specification bytes differ from the recorded revision snapshot")
    sections = metadata["sections"]
    count = 37 if version == 1 else metadata.get("numbered_section_count")
    if type(count) is not int or not 1 <= count <= 99:
        raise ValueError("invalid numbered section count")
    if set(sections) != {str(n) for n in range(1, count + 1)}:
        raise ValueError(f"specification inventory must cover sections 1 through {count}")
    appendices = {} if version == 1 else metadata.get("appendices", {})
    headings = metadata["document_heading_order"]
    if len(headings) != len(set(headings)):
        raise ValueError("duplicate document heading identity")
    numbered_headings = {}
    appendix_headings = {}
    for title in headings:
        match = re.match(r"^[\s\ue000-\uf8ff]*([0-9]{1,2})\.\s", title)
        if match:
            number = match.group(1)
            if number in numbered_headings:
                raise ValueError("duplicate numbered document heading")
            numbered_headings[number] = title
        appendix = re.match(r"^[\s\ue000-\uf8ff]*Appendix ([A-Z])\.\s", title)
        if appendix:
            letter = appendix.group(1)
            if letter in appendix_headings:
                raise ValueError("duplicate appendix document heading")
            appendix_headings[letter] = title
    if set(numbered_headings) != set(sections):
        raise ValueError("document headings do not cover the numbered inventory")
    for number, section in sections.items():
        if section["title"] != numbered_headings[number]:
            raise ValueError(f"section {number} has the wrong numbered heading identity")
    if set(appendix_headings) != set(appendices):
        raise ValueError("document headings do not cover the appendix inventory")
    for letter, appendix in appendices.items():
        if appendix["title"] != appendix_headings[letter]:
            raise ValueError(f"appendix {letter} has the wrong appendix heading identity")
    text = raw.decode("utf-8")
    parts = {}
    current = None
    lines = []
    encountered = []
    for line in text.split("\n"):
        if line in headings:
            if current is not None:
                parts[current] = "\n".join(lines) + "\n"
            current = line
            encountered.append(line)
            lines = []
        lines.append(line)
    if current is not None:
        parts[current] = "\n".join(lines) + "\n"
    if encountered != headings:
        raise ValueError("document headings are missing, repeated or reordered")
    for number, section in sections.items():
        part = parts.get(section["title"])
        if part is None or hashlib.sha256(part.encode()).hexdigest() != section["sha256"]:
            raise ValueError(f"section {number} differs from its recorded inventory")
    for letter, appendix in appendices.items():
        part = parts.get(appendix["title"])
        if part is None or hashlib.sha256(part.encode()).hexdigest() != appendix["sha256"]:
            raise ValueError(f"appendix {letter} differs from its recorded inventory")
    print(f"Verified the recorded specification revision, all {count} section inventories "
          f"and {len(appendices)} appendices.")


if __name__ == "__main__":
    check(Path(__file__).resolve().parent.parent)
