# Exact resource measurements and refusal bounds (#322)

Owned source commits `a276295`, `5d0835c`, `5c55cfe`, and `defc692` add only
`lean/ResourceModel42/MetricsProofs.lean`. The module imports Syntax `d51a21b`
over unchanged Full at baseline `30de7fe`. It does not import or modify Checker,
Evaluator, their proof modules, compiler/runtime code, or CI.

The independent specifications `valueMetrics`, `atomMetrics`, and `exprMetrics`
count the actual logical expanded representation. They include all stored
branches, closed value islands, pair values assembled from two closed atoms,
and enclosing expression nodes. They do not inspect the measurement result.
`ValueExceeds`, `ExprExceeds`, and `InitialExceeds` describe inequalities on
these actual structures, not a predicate asserting that an algorithm returned
an error.

The proofs establish:

- Successful value/expression measurement returns exactly the independent
  metrics and satisfies every corresponding configured bound. Every refusal
  identifies an actual exceeded bound.
- Planned substitution has the same metrics as actual sequential runtime
  substitution, including index adjustment and lifting under one/two binders.
- Substitution preflight describes the actual candidate. It can also refuse
  an oversized incoming value even when the body will not use that value;
  the refusal theorem explicitly preserves that distinction.
- Unpack preflights both the actual intermediate right-at-zero substitution
  and the final left-at-zero substitution. Neither is replaced by a final-only
  metric assertion.
- Context preflight describes the whole actual plugged expression.
- The production input-admission Array.forIn loop is proved equal to the
  structurally analyzed loop. Success returns the exact expression metrics
  after every input-closing pure let, and bounds input count and the sum of
  expanded input-value nodes. Refusals identify actual count, aggregate, or
  whole closed-expression excess.
- Conversely, the public `*_complete` theorems prove exact success whenever
  these independent actual bounds hold. This excludes an implementation that
  arbitrarily refuses otherwise admissible values/expressions.

No positivity premise is imposed on limits: zero retains its ordinary numeric
meaning. The recursive measurement fuel in the general helper theorems is
separate from evaluator execution fuel. The public configured-depth wrappers
prove excess against the configured depth itself. Scalar range/type correctness,
source admission, resource/event counters, and reachable-state operational
composition remain the corresponding checker/evaluator proof layers' concerns.

## Complete public theorem catalog

All 68 manual theorems are public, under
`Vesper.ResourceModel42.MetricsProofs`:

1. `value_depth_positive`
2. `atom_depth_positive`
3. `expr_depth_positive`
4. `checkValueStats_ok`
5. `checkValueStats_error`
6. `checkExprStats_ok`
7. `checkExprStats_error`
8. `value_pair_left_over`
9. `value_pair_right_over`
10. `measureValue_spec`
11. `measureValue_ok`
12. `measureValue_error`
13. `metricsPlan_lift`
14. `applyAtomPlan_val`
15. `applyExprPlan_atom`
16. `applyExprPlan_pair`
17. `applyExprPlan_letE`
18. `applyExprPlan_unpack`
19. `applyExprPlan_ifz`
20. `applyExprPlan_mint`
21. `applyExprPlan_consume`
22. `plannedVariable_metrics`
23. `pair_values_metrics`
24. `asValue_metrics`
25. `value_over_atom`
26. `measureAtom_spec`
27. `binary_left_over`
28. `binary_right_over`
29. `pair_left_over`
30. `pair_right_over`
31. `branch_condition_over`
32. `branch_zero_over`
33. `branch_nonzero_over`
34. `checkStats_spec`
35. `stats_bind`
36. `consume_over`
37. `measureExpr_spec`
38. `measureExpr_ok`
39. `measureExpr_error`
40. `preflightSubst_ok`
41. `preflightSubst_error`
42. `preflightUnpack_ok`
43. `preflightUnpack_error`
44. `configured_over_at`
45. `measureExpr_at`
46. `measurePlug_spec`
47. `preflightPlug_ok`
48. `preflightPlug_error`
49. `input_forIn`
50. `input_preflight_eq`
51. `closeList_cons`
52. `initialNodes_cons`
53. `wrapper_over`
54. `closeList_over`
55. `input_value_over`
56. `actual_over_inputs`
57. `inputLoop_spec`
58. `preflightInputs_spec`
59. `preflightInputs_ok`
60. `preflightInputs_error`
61. `valueWithin_not_exceeds`
62. `within_not_exceeds`
63. `measureValue_complete`
64. `measureExpr_complete`
65. `preflightSubst_complete`
66. `preflightUnpack_complete`
67. `preflightPlug_complete`
68. `preflightInputs_complete`

There are no private helper theorems, sorry/admit/project axioms, unsafe or
partial definitions, or native-decision shortcuts in this module.

## Actual execution and remaining integration

With the coordinator's explicit one-compiler lease, the source writer ran the
actual pinned `/home/agent/.local/share/vesper-lean/bin/lean` with
`LEAN_NUM_THREADS=1`, against the immutable Full/Syntax dependency checkpoint:

```sh
LEAN_PATH=/tmp/resource-model42-syntax-api-d51a21b:/tmp/resource-model42-metrics-final \
LEAN_NUM_THREADS=1 /home/agent/.local/share/vesper-lean/bin/lean \
  -o /tmp/resource-model42-metrics-final/ResourceModel42/MetricsProofs.olean \
  lean/ResourceModel42/MetricsProofs.lean
```

The final compile exited 0 with an empty diagnostic log at
`/tmp/resource-model42-metrics-final/compile.log`. Initial proof-elaboration
failures and unused-simp notes were resolved as actual proofs, without changing
the source algorithms or weakening the independent metrics. `git diff --check`
passed. No Cargo command was run.

The complete immutable read-only Full/Syntax/Metrics source-and-olean chain is
`/tmp/resource-model42-metrics-api-defc692`. No compiled artifact is tracked or
stored in the checkout. The compiler lease was released after the coherent
checkpoint.

This report claims Lean compilation of these proofs, not a separately executed
transitive axiom-policy audit or independent Lean4Lean replay. Acceptance must
include all 68 roots in the assembled complete catalog, partitioning requests
when the full project's union exceeds 128 roots. Evaluator composition must use
these actual-bound witnesses for reachable refusal classification. These are
finite-model representation proofs, not a Rust/compiler refinement or an
executable resource differential campaign.
