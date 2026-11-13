# Specification snapshot refresh (claim #227)

Refreshed the committed snapshot of the supplied Vesper design document from the coordinator's already-fetched normalized text. This is a documentation and inventory update, with no compiler, bootstrap, security, or proof implementation claim.

- Document: https://docs.google.com/document/d/1zO7boUVwNTHRA-1ZNL4bDYi68OjQZBCimLWkETH6bso/edit
- Recorded revision: `AHj4eMSb9JoNMOKZJ6wCp3fBbv7MFBZf76WoYONerEUzZ35_RI-G55cPRJqH1EIJVtIveO4t_2eQMcRO78JADiHSJhj25XSuxRM5WN7j24A`
- Recorded modification time: `2026-11-13T04:46:25.948Z`
- Snapshot size: 556,292 bytes
- Snapshot SHA-256: `8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d`

The snapshot copies `/tmp/vesper-spec-latest.txt` exactly, without applying a second HTML-entity decode. Its bytes were also checked against the fetched JSON paragraphs, HTML-unescaped once, joined with LF and terminated with LF. Heading metadata comes from the fetched `HEADING_1` paragraphs with the same single entity decode.

Schema 2 already supports this inventory; the checker implementation is unchanged. All 42 numbered sections and Appendix A have recomputed SHA-256 inventories using the existing checker's `text.split("\n")`, heading-delimited part collection, and `"\n".join(lines) + "\n"` policy, including its final empty-line behavior. The document's actual heading order is preserved: §35 remains first, and new §42 appears between §19 and §20, immediately before the existing §19.8 content in the source. The new inventory boundary therefore changes §19's digest. The master specification map under §35 also changes; every other existing numbered section and Appendix A retain their previous digests. The complete §15 part was compared byte-for-byte and is unchanged.

New §42 records requirements for a bootstrap reference oracle, rewrite witnesses, direct verified code generation, self-hosting, and diverse double-compiling. Capturing those requirements does not establish their implementation or validate the document's security and proof assertions.

The snapshot regression tests now derive the latest section and its predecessor from the recorded count, rather than assuming §41 and §40. All existing negative checks and the legacy 37-section schema regression remain.

## Validation

Run from `/workspace/vesper-worktrees/spec42-integration`:

```sh
python3 scripts/check-specification-snapshot.py
python3 -m unittest scripts.tests.test_specification_snapshot
git diff --check
```

Results: the checker verified all 42 numbered inventories and one appendix; all 14 unit tests passed; the whitespace check passed. No configured Python formatter applies to this narrow change. No network access, remote document edits, build/source changes, push, pull request, or merge occurred in this lane. Recorded revision metadata and hashes establish local snapshot consistency, not source authenticity, freshness, or implementation completion.
