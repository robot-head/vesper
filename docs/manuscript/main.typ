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
  description: "The complete Vesper language specification: Parts I–VIII (Chapters 1–42), mechanized Lean 4 metatheory, and official brand specification.",
  keywords: ("Vesper", "systems programming", "linear types", "lexical effects", "refinement types", "Lean 4"),
  date: datetime(year: 2026, month: 11, day: 14),
)
#set text(font: "Libertinus Serif", size: 10.5pt, fill: ink, lang: "en", hyphenate: true)
#set par(justify: true, leading: 0.58em, spacing: 0.7em)
#set page(paper: "a4", margin: (left: 22mm, right: 22mm, top: 23mm, bottom: 21mm), numbering: none)
#set heading(numbering: none)
#show heading: set text(hyphenate: false)
#show heading.where(level: 1): set text(size: 18pt, weight: "bold")
#show heading.where(level: 2): set text(size: 16pt, weight: "bold")
#show heading.where(level: 3): set text(size: 12.5pt, weight: "bold")
#show heading.where(level: 4): set text(size: 11pt, weight: "bold")
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
#text(size: 12pt)[Parts I–VIII · Chapters 1–42 · Appendices A and B]
#v(1fr)
#text(size: 10.5pt, fill: muted)[Manuscript edition\ 14 November 2026]
#v(8mm)
#link("https://vesper-lang.org")[vesper-lang.org]

#pagebreak()
#set page(numbering: "i", footer: context align(center, text(size: 9pt, fill: muted, counter(page).display("i"))))
#counter(page).update(1)
#text(size: 18pt, weight: "bold")[About this specification]
#v(8mm)
This manuscript describes a proposed end-state design for the Vesper programming language ($lambda_(upright("Vesper"))$). It contains illustrative rules, implementation sketches, and research goals alongside existing code. It is under technical review and is not a conformance standard or evidence of an end-to-end soundness or security proof. In particular, do not rely on its runtime isolation, cancellation, snapshot, or distributed-memory claims without independent validation against the implementation.

The specification is organized bottom-up into eight architectural parts
spanning Chapters 1–42 and Appendices A–B: core language design and unified
four-axis modal, destination, liquid-refinement, and graded effect theory
(Part I); the five-stage compiler pipeline, host workspace,
prophecy-free Lean 4 verification extractor, verified bootstrap, Vesper Loom,
and mathematical SemVer variance (Part II); zero-GC memory, SIMD, fiber I/O,
wire FFI, and WasmFX substrates (Part III); incremental IDE, formatting,
documentation, highlighting, concolic fuzzing, flight-recording, and PMU
benchmarking tools (Part IV); bare-metal kernels, eBPF, unikernels,
constant-time cryptography, and confidential enclaves (Part V); region
serialization, non-malleable binary parsers, telemetry, kernel-bypass
networking, HTTP/3 services, choreographic consensus, hermetic SQL, and DBSP
stream processing (Part VI); and native user interfaces, tri-backend 3D
graphics, deterministic physics, hard-realtime DSP, and shape-checked tensor
compute (Part VII). Appendix A presents illustrative Lean 4 kernel models, and Appendix B specifies Vesper's geometric and visual
identity.

#v(5mm)
#text(weight: "bold")[Provenance]

+ #link("https://docs.google.com/document/d/1zO7boUVwNTHRA-1ZNL4bDYi68OjQZBCimLWkETH6bso/edit")[Language design and formal specification], retrieved 14 November 2026.
+ #link("https://drive.google.com/file/d/1td8Tw2W9pU8alI9QtQG6m3dMdzkQsHSU/view")[Unified refinement, modal, destination, and graded type theory].
+ #link("https://drive.google.com/file/d/1QGefoudw60U8k-4xUjNnrRE1L8yplqWp/view")[Official brand identity and geometric specification].

#v(5mm)
#text(weight: "bold")[Typography and navigation]

The manuscript is authored in native Typst markup with custom inference-rule
blocks, mathematical equations, syntax-clean raw listings, and resolved
internal cross-references across all parts, chapters, subsections, and
appendices. Linked contents and PDF bookmarks provide direct navigation. Code
blocks wrap at the page boundary and continue across pages while preserving
exact source text in the editable Typst manuscript.

#v(5mm)
#text(size: 9pt, fill: muted)[Typeset with Typst 0.15.1. Libertinus Serif, DejaVu Sans Mono, and New Computer Modern Math are embedded using Typst's bundled fonts.]

#pagebreak()
#outline(title: [Contents], depth: 2, indent: auto)

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
