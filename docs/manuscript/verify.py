"""Verify the published manuscript against Typst's actual document structure.

Requires Typst 0.15.1 and PyMuPDF. This checks publication fidelity and layout,
not the language implementation or the correctness of the supplied proofs.
"""
import argparse
import json
import re
import shutil
import subprocess
import unicodedata
from pathlib import Path

import pymupdf


def normalized(text):
    return re.sub(r"\s", "", unicodedata.normalize("NFKC", text).replace("\u00ad", ""))


def verify(pdf_path, typst):
    root = Path(__file__).resolve().parents[2]
    document = pymupdf.open(pdf_path)
    outline = document.get_toc()
    parts = [m[1] for level, title, _ in outline
             if level == 1 and (m := re.match(r"^Part ([IVX]+)\.\s", title))]
    assert parts == ["I", "II", "III", "IV", "V", "VI", "VII", "VIII"], (
        f"The PDF must contain Parts I–VIII in order, got {parts}"
    )
    numbered = [int(m[1]) for level, title, _ in outline
                if level == 1 and (m := re.match(r"^(\d+)\.\s", title))]
    assert numbered == list(range(1, 42)), "The PDF must contain all 41 chapters once, in order"
    assert all(any(level == 1 and title.startswith(f"Appendix {letter}.")
                   for level, title, _ in outline) for letter in ("A", "B"))
    assert document.metadata["creator"] == "Typst 0.15.1"

    raw = json.loads(subprocess.check_output([
        typst, "eval", "--ignore-system-fonts", "--root", str(root),
        "--in", str(root / "docs/manuscript/main.typ"),
        "query(raw.where(block: true)).map(it => it.text)",
    ]))
    body = "".join(page.get_text(clip=pymupdf.Rect(
        60, 60, page.rect.width - 60, page.rect.height - 50)) for page in document)
    body = normalized(body)
    missing = [i + 1 for i, text in enumerate(raw) if normalized(text) not in body]
    assert not missing, f"Code blocks changed during PDF extraction: {missing}"

    errors = []
    fonts = set()
    for number, page in enumerate(document, 1):
        text = page.get_text()
        if "\x00" in text or "\ufffd" in text:
            errors.append(f"Missing text glyph on page {number}")
        if len(text.strip()) < 100:
            errors.append(f"Nearly empty spill page {number}")
        for block in page.get_text("dict")["blocks"]:
            for line in block.get("lines", []):
                for span in line["spans"]:
                    if span["bbox"][0] < 45 or span["bbox"][2] > page.rect.width - 45:
                        errors.append(f"Text exceeds the manuscript margins on page {number}")
        for font in page.get_fonts():
            fonts.add(font[0])
    assert not errors, "\n".join(errors)
    assert all(document.extract_font(xref)[3] for xref in fonts), "Every font must be embedded"
    return {
        "pages": len(document),
        "parts": 8,
        "chapters": 41,
        "appendices": ["A", "B"],
        "bookmarks": len(outline),
        "links": sum(len(p.get_links()) for p in document),
        "code_blocks_preserved": len(raw),
        "embedded_fonts": len(fonts),
        "layout_errors": errors,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--pdf", type=Path, default=Path(__file__).resolve().parents[2] / "website/vesper-specification.pdf")
    parser.add_argument("--typst", default=shutil.which("typst"))
    args = parser.parse_args()
    if not args.typst:
        parser.error("Pass --typst with the path to Typst 0.15.1")
    print(json.dumps(verify(args.pdf, args.typst), indent=2))
