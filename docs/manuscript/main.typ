// Vesper manuscript. Build with Typst 0.15.1 and bundled fonts only.
#let ink = rgb("#1B2030")
#let muted = rgb("#5B6474")
#let rule = rgb("#D8DCE3")
#let chapter-deck(body) = block(above: 0pt, below: 18pt)[
  #text(size: 11.5pt, fill: muted, body)
]

#set document(
  title: "Vesper: Language Design and Formal Specification",
  author: "Vesper",
  description: "The complete Vesper language manuscript: sections 1–44, mechanized metatheory, and official brand specification.",
  keywords: ("Vesper", "systems programming", "linear types", "lexical effects", "refinement types", "Lean 4"),
  date: datetime(year: 2026, month: 11, day: 14),
)
#set text(font: "Libertinus Serif", size: 10.5pt, fill: ink, lang: "en", hyphenate: true)
#set par(justify: true, leading: 0.58em, spacing: 0.7em)
#set page(paper: "a4", margin: (left: 22mm, right: 22mm, top: 23mm, bottom: 21mm), numbering: none)
#set heading(numbering: none)
#show heading: set text(hyphenate: false)
#show heading.where(level: 1): set text(size: 18pt, weight: "bold")
#show heading.where(level: 2): set text(size: 12.5pt, weight: "bold")
#show heading.where(level: 3): set text(size: 11pt, weight: "bold")
#show heading.where(level: 1): it => {
  pagebreak(weak: true)
  block(above: 0pt, below: 12pt, it)
}
#show link: it => text(fill: rgb("#006B80"), it)
#set raw(theme: none)
#show raw: set text(font: ("DejaVu Sans Mono", "New Computer Modern Math"), size: 8.2pt, hyphenate: false)
#show raw.where(block: true): set par(leading: 0.4em, spacing: 0pt, justify: false)
#show raw.where(block: true): it => block(
  width: 100%, breakable: true,
  inset: (x: 9pt, y: 9pt),
  fill: luma(97%),
  stroke: (left: 0.6pt + rule),
  above: 10pt, below: 10pt,
  it,
)
#show math.equation: set text(font: "New Computer Modern Math")
#show table: set text(size: 8.6pt)
#show table: set par(justify: false, leading: 0.45em)
#set table.cell(breakable: false)
#set table(
  inset: (x: 6pt, y: 5pt),
  stroke: 0.3pt + rule,
  fill: (_, row) => if row == 0 { luma(95%) } else { none },
)
#show figure: set block(breakable: true)
#set enum(indent: 1.5em, body-indent: 0.5em)
#set list(indent: 1.2em, body-indent: 0.5em)

// Title page.
#v(17mm)
#image("/website/brand/vesper-mark-dawn.svg", width: 22mm)
#v(13mm)
#text(size: 43pt, weight: "bold")[Vesper]
#v(7mm)
#text(size: 26pt)[Language Design and\ Formal Specification]
#v(10mm)
#line(length: 100%, stroke: 0.6pt + rule)
#v(7mm)
#text(size: 12pt)[Sections 1–44 · Appendices A and B]
#v(1fr)
#text(size: 10.5pt, fill: muted)[Manuscript edition\ 14 November 2026]
#v(8mm)
#link("https://vesper-lang.org")[vesper-lang.org]

#pagebreak()
#set page(numbering: "i", footer: context align(center, text(size: 9pt, fill: muted, counter(page).display("i"))))
#counter(page).update(1)
#text(size: 18pt, weight: "bold")[About this edition]
#v(8mm)
This manuscript brings the complete Vesper language design, formal rules,
reference code, architecture, and metatheory into one navigable edition.
The 44 numbered sections appear in numerical order. Appendix A contains
the mechanized Lean 4 metatheory; Appendix B records the official geometric
and visual identity.

Section 44, _Unified Advanced Type Theory_, integrates refinement types,
ghost linearity, destination calculus, four-axis modalities, graded resource
effects, guarded clocks, and quotients. It extends the preceding compiler
architecture with the #raw("vesper-refine") host crate.

#v(5mm)
#text(weight: "bold")[Source documents]

+ #link("https://docs.google.com/document/d/1zO7boUVwNTHRA-1ZNL4bDYi68OjQZBCimLWkETH6bso/edit")[Language design and formal specification], retrieved 14 November 2026.
+ #link("https://drive.google.com/file/d/1td8Tw2W9pU8alI9QtQG6m3dMdzkQsHSU/view")[Section 44: Unified Advanced Type Theory].
+ #link("https://drive.google.com/file/d/1QGefoudw60U8k-4xUjNnrRE1L8yplqWp/view")[Appendix B: Official Brand Identity and Geometric Specification].

#v(5mm)
#text(weight: "bold")[Editorial treatment]

Source emphasis, hyperlinks, tables, code, formal rules, and mathematical
expressions are retained. Section order and heading hierarchy are normalized.
Section 35's lost code indentation is restored from the preserved repository
snapshot after a token-for-token comparison with the current source. Duplicate
source titles and word-processor formatting markers are removed. The opening
of Section 44 is recast as manuscript prose. Appendix B begins with the
canonical appendix, following its source document's integration preface.

The linked contents and PDF bookmarks provide navigation. Code blocks wrap
at the page boundary and continue across pages; their complete source text
remains in the editable Typst manuscript.

#v(5mm)
#text(size: 9pt, fill: muted)[Typeset with Typst 0.15.1. Libertinus Serif, DejaVu Sans Mono, and New Computer Modern Math are embedded using Typst's bundled fonts.]

#pagebreak()
#outline(title: [Contents], depth: 1, indent: auto)

#pagebreak()
#set page(
  numbering: "1",
  header: context [
    #set text(size: 8.5pt, fill: muted)
    #grid(columns: (1fr, 1fr), [Vesper], align(right)[Language Design and Formal Specification])
    #v(3pt)
    #line(length: 100%, stroke: 0.3pt + rule)
  ],
  footer: context align(center, text(size: 9pt, fill: muted, counter(page).display("1"))),
)
#counter(page).update(1)
#import "content.typ": manuscript
#manuscript(chapter-deck)
