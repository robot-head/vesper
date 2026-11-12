# Field-mode correction

The user selected preservation of total linear exhaustion over unrestricted field-mode relaxation.

Add the following premise to `[K-Adt]`:

```text
∀i, k_i ⊑ m_i   (field modality well-formedness)
```

Field modality well-formedness (required by [K-Adt]): For every declared field f_i : τ_i @@ m_i, its component kind under the declaration's generic bounds must satisfy k_i ⊑ m_i. An override must never relax a component's ownership obligations. Therefore @@ copy accepts only components known to be copy, and @@ affine accepts only components known to be at most affine. Unannotated fields default to linear and satisfy this premise automatically. A copy wrapper containing a linear Ticket @@ copy is rejected; it cannot hide or discard the Ticket's obligation. This restriction preserves Theorem 2 (Total Linear Exhaustion).

The original unrestricted rule's counterexample is checked in `lean/SpecificationCounterexample.lean`; the compiler rejects its surface form in `examples/rejected/specification_counterexample.ves` with E0105. The normative specification now includes the restriction above.
