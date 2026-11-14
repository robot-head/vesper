import Lean4LeanLevel42.MapComparison

/-!
Audited soundness of the actual level-comparison implementation.
The fixed packet includes the real traversal and Option short-circuit folds,
then composes normalization denotation and well-formedness for `geq'`.
Canonicality, comparison completeness, reification and the opaque equality
fast path are separate from these statements.
-/
