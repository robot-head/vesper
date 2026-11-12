# Actual source function parameters in the typed Forge host

Claim #160 owns typed_quote.rs, the private resolve.rs parameter helper, the new
typed_function_parameters tests and this report. The frozen contract is
docs/rfcs/params41-contracts.md, including the coordinator's actual SelfKw/self
clarification3957f45. No workflows, Bazel/build files or test wrappers were
changed; those remain reserved to external #158.

resolve_expr_function_body requires the current real host-owned ExprSource,
exact original bytes matching sealed MacroSource, and its source-bound
FunctionId. It reads the private actual parameter-name token inventory and
validates header order, source/parameter bounds, matching names and Lower/SelfKw
lexer tokens. Generic/projected headers, duplicate names and unsupported direct
parameter calls refuse. Public copied SourceFunction records cannot alter this
association. Parameter counts are bounded before traversing support headers.

Actual header identifier tokens are appended after the parsed body, so existing
body shape indices remain unchanged. A private resolver helper seeds fresh
source-ranked binding scopes and threads the resulting Ctx into whole-body
resolution. This makes original source/antiquotation references see parameters
without capturing generated references. Definition-site global claims still
bypass caller bindings. Parameter binders do not enter callable-alias tracking,
so indirect calls remain unsupported.

ResolvedBody retains its actual function identity and sealed ResolvedParameter
records containing ordinal, original name span, fragment token and real BinderId.
There is no constructor or Deserialize implementation for those records. The
legacy resolve_expr_body API returns no function association and no parameters.
Public Term and ResolvedFragment wire structures remain unchanged.

The existing shared TypedBudget pays exact-source/name comparison work before
header inspection, token/provenance/record storage before allocation, parameter
candidate scans, initial scope copies and resolver depth. Scope reservations
include the parameter count rather than reusing the former max_depth+2 envelope.
No caps are raised or work allowances reset. These are logical bounded-operation
guards, without latency or resident-memory claims.

Local Linux Rust1.99.1 validation:

- All49 pure Forge tests in7suites passed, zero ignored, using
  cargo test --locked -p vesper-forge --no-default-features --all-targets.
- The8 new tests cover actual header/suffix provenance and in/inout/sink/self;
  both capture directions; a parameter shadowing a definition-site global;
  initializer shadowing; exact source, foreign host and FunctionId refusal;
  legacy/zero-parameter association; scope refusal before binder minting; and
  direct generic/projected/duplicate/indirect-call refusals.
- After the final bounded-header traversal ordering adjustment and final actual
  syntax dependency, all8 new tests passed again. Strict pure Forge all-target
  Clippy, rustfmt and whitespace checks passed.

Owned commits are4e0c0d2 andceec1bf plus this report. Actual syntax dependencies
are798d3f5 andc5f6196; their local cherry-pick copies aref344521 and20d8ed1 and
must not be duplicated when integrating the owned host commits.

This host layer establishes syntax binding/provenance, not checked-program
authority. Parameter header alpha-renaming, exact full checked-header comparison,
ordinary mode/linear checking, actual authority derivation and IR validation
belong to the compiler integration lane. No user program, guest component or
Lean process executes here, and no current native CI result is claimed.
