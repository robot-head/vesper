# SH7 Cranelift entry-point inventory

Tracking [#2209](https://github.com/robot-head/vesper/issues/2209) and slice 1 [#2212](https://github.com/robot-head/vesper/issues/2212). Inventory base `0662cc90`.

The standalone [Stage 2 entry](../../packages/vesper-stage2/src/lib.ves)
currently lowers to the Wasm backend, wraps a component, and writes the
result. The native object path below belongs to the hosted/hybrid
compiler. Stage 2 will need a native output request and a Vesper-owned
handoff from its lowered IR to the backend.

## Compiler entry paths

| Entry and source | Cranelift boundary |
|---|---|
| Ordinary executable object, `object_with_options` → `object_with_observer` → `object_with_codegen_options` in [native.rs](../../src/native.rs) | Host or explicit-target ISA, `ObjectBuilder::new`, `ObjectModule::new`, `compile_entry`, and `finish().emit()`. Windows routes via `windows::object`. The sibling `object_for_isa` accepts the freestanding caller-supplied ISA. |
| Traced objects, coverage, and observer-only verification in [native.rs](../../src/native.rs) and [native_coverage.rs](../../src/native_coverage.rs) | Same function-building and object APIs, with trace host imports, counters, or emitted-function observers. The observer-only path consumes CLIF without finishing an object. |
| Library object and C exports in [native_export.rs](../../src/native_export.rs) | Declares internal bodies, exported wrappers, glue, and data on the module. Uses `object_isa_optimized` and finishes the object. The export swap can emit glue through a lent module. |
| Session objects and session JIT in [native_session.rs](../../src/native_session.rs) | Builds the session entry and state dispatch, imports runtime symbols including `vesper_fiber_park`, and finishes the object or finalizes JIT definitions. |
| x64 Windows COFF in [native_windows.rs](../../src/native_windows.rs) | `UnwindModule` implements `Module` over `ObjectModule`. It records `create_unwind_info` for compiled functions and handles the raw-byte definition path. It writes `.xdata` and `.pdata`, relocates them against function symbols, and emits the final object. |
| Freestanding images in [native_freestanding.rs](../../src/native_freestanding.rs), [freestanding/runtime.rs](../../src/freestanding/runtime.rs), and [freestanding/checks.rs](../../src/freestanding/checks.rs) | Uses the caller-supplied ISA for RV64IMAC and AArch64; emits the compiler object and runtime helpers. Compiles the emitted CLIF for frame and call-site measurement and uses trap metadata for its image diagnostics. |
| JIT execution, tracing, and test internals in [native.rs](../../src/native.rs) | Uses `JITBuilder::with_isa` and `JITModule::new`, registers runtime symbols, keeps the code near callees where necessary, finalizes definitions, reads finalized function/global addresses, and frees the module memory after use. |
| Batch JIT and Anvil in [native_batch.rs](../../src/native_batch.rs) and [anvil_jit.rs](../../src/anvil_jit.rs) | Shares declarations and runtime symbol resolution across entries. Anvil also uses the compiled functions and data for per-mutant dispatch. |

The main Rust compiler depends directly on Cranelift 0.125.4. The shim
re-exports `cranelift_codegen`, `cranelift_frontend`, `cranelift_jit`,
`cranelift_module`, `cranelift_native`, and `cranelift_object`; replacing calls spelled `vcl_*` alone leaves the direct
Rust path in place. [Cargo manifests](../../Cargo.toml),
[shim re-exports](../../crates/vesper-cranelift-sys/src/lib.rs).

### Direct Rust CLIF and code generation

- `FunctionBuilderContext` and `FunctionBuilder` own SSA construction:
  block creation, insertion, parameters and sealing, local variable
  declaration/definition/use, stack slots, source locations, cold blocks,
  and finalization. `InstBuilder` emits the IR. `Function`, its layout,
  data-flow graph, and instruction/condition/type definitions are also read
  and edited directly by native optimization and verifier passes.
- `Module` supplies target config, pointer type, context/signature creation
  and clearing, symbol and data declarations, importing their references
  into functions, `define_function`, `define_function_bytes`, and
  `define_data`. `DataDescription` supplies initialized or zero-filled
  storage, alignment, and function/data relocations.
- `Context::compile` and the module's compile path return assembled code,
  frame size, call sites, traps, relocations, and unwind information. The
  stack analysis recompiles observed functions with the same ISA settings.
  Preserve source/trap locations and memory flags at the boundary, as
  native checks and runtime diagnostics read them.
- The function-definition boundary in `define_instances_observed_with_trace`
  receives finalized functions. It is downstream of Place IR traversal,
  SSA construction and native-walk body emission. Alternate code can enter
  an existing object module through `define_function_bytes` during the
  hybrid migration. The caller must supply alignment and relocations, and
  the Windows path must receive unwind information before accepting framed
  code. This would still use Cranelift's object container until the writer
  slice replaces it. Decline before defining a function on the module so
  a fallback can still compile that function.
- CLIF diagnostics, optimization, constant-time and stack checks, and
  coverage are consumers of the current IR or compile results. The
  inventory includes them so a backend row cannot silently bypass their
  checks. See [native_clif_opt.rs](../../src/native_clif_opt.rs),
  [native_verifiers.rs](../../src/native_verifiers.rs),
  [native_vectorize.rs](../../src/native_vectorize.rs), and
  [native_cold_outline.rs](../../src/native_cold_outline.rs).

### ISA, ABI, and register allocation

| Caller | Effective settings and dependencies |
|---|---|
| `native::isa_with_flags` | Hosted objects are PIC. Host detection uses `cranelift_native::builder`; explicit triples use `isa::lookup` and support x86-64 and AArch64. Enable `enable_llvm_abi_extensions` for wide integer register pairs. Optimized builds set `opt_level=speed`. DetFloat sets `enable_nan_canonicalization`. |
| `native::set_target_features` / `cranelift_feature` | Explicit x86-64 feature mapping covers SSE3, SSSE3, SSE4.1/4.2, POPCNT, AVX, AVX2, FMA, BMI1/2, LZCNT, CMPXCHG16B, AVX512F/VL/DQ/BITALG/VBMI. AArch64 covers LSE, pointer authentication (`paca` → `has_pauth`) and FP16. Unknown and inapplicable feature names are ignored. |
| `native` JIT constructors | Host ISA, non-PIC, `use_colocated_libcalls=false`, LLVM ABI extensions, and speed/NaN canonicalization on the paths that request them. Batch JIT uses non-PIC, non-colocated libcalls, and conditional NaN canonicalization. |
| `native_session::isa_flags` | PIC for objects and non-PIC for JIT, with non-colocated libcalls for JIT. Sets speed and NaN canonicalization when requested. |
| `FreestandingTarget::isa` → `vesper_silicon::stack::target_isa_with` | `opt_level=speed`, frame pointers kept, stack probes off, unwind metadata on, default non-PIC. RV64 sets `has_zca=true` and `has_zcd=false`, with floating-point operations rejected by freestanding checks. |
| `vcl_module_new` / `vcl_jit_module_new` → `isa_for` | Empty triple detects host features and an explicit triple uses `isa::lookup`. Each semicolon- or newline-separated `key=value` tries the shared settings first, then ISA-specific settings. Invalid values return a constructor error. |
| Register allocation | The hosted compiler sets no explicit `regalloc_algorithm`, `regalloc_checker`, or `regalloc_verbose_logs`. Cranelift 0.125.4 defaults to backtracking allocation, with checker and verbose logs disabled. The shim's generic flag interface can override those defaults. Stage 2 must replace allocation together with spills, ABI register constraints, frame layout, and prologue/epilogue metadata. |

The shim carries the target default convention and explicit Fast, System V,
Windows fastcall, Apple AArch64, Tail, and other convention tags defined in
[`codes.rs`](../../crates/vesper-cranelift-sys/src/codes.rs). Signatures can
include narrow sign/zero extension and multi-value results. The native
export ABI also fixes aggregate layout, ownership, and exported symbols
([manuscript §16.6](../manuscript/content.typ), export ABI
`vesper.native.export-abi/1`). Capture the concrete
convention per signature so internal calls and external symbols agree.

## Shim entry points

The inventory covers the entire exported ABI so wrapper-only, pilot and
diagnostic entries are visible alongside production calls. The
[header](../../crates/vesper-cranelift-sys/include/vesper_cranelift.h),
[`.vsys`](../../packages/vesper-cranelift/cranelift.vsys), and
[`symbols()`](../../crates/vesper-cranelift-sys/src/lib.rs) expose the same
names. The 133 entries below omit the common `vcl_` prefix.

| Family | Entry points |
|---|---|
| Lent-call protocol | `call_op`, `call_arg_count`, `call_arg`, `call_args`, `call_result`, `call_builder`, `call_module`, `call_return_builder`, `call_return_module` |
| Module lifecycle and target | `module_new`, `jit_module_new`, `new_error`, `module_error`, `module_free`, `module_kind`, `module_pointer_type`, `isa_triple_arch`, `has_x86_pshufb_lowering` |
| Signatures | `sig_new`, `sig_param`, `sig_return`, `sig_param_ext`, `sig_return_ext` |
| Symbol and data declarations | `declare_function`, `declare_data`, `declare_anonymous_data`, `module_get_function`, `module_get_data`, `module_function_count`, `module_function_at`, `module_function_name`, `module_function_linkage`, `module_function_signature`, `module_data_count`, `module_data_at`, `module_data_name`, `module_data_linkage` |
| Data contents and relocations | `data_new`, `data_free`, `data_define`, `data_zeroinit`, `data_align`, `data_func_addr`, `data_data_addr`, `define_data` |
| Function builder and diagnostics | `builder_new`, `builder_error`, `builder_free`, `set_func_name`, `set_srcloc`, `value_type`, `inst_result` |
| Blocks and SSA variables | `create_block`, `append_block_param`, `append_block_params_for_function_params`, `block_param`, `current_block`, `switch_to_block`, `seal_block`, `seal_all_blocks`, `set_cold_block`, `declare_var`, `def_var`, `use_var` |
| Imported references and addresses | `import_function`, `import_function_colocated`, `import_signature`, `import_data`, `import_data_colocated`, `func_addr`, `symbol_value`, `global_value`, `get_stack_pointer` |
| Scalar and shared vector operations | `iconst`, `f32const`, `f64const`, `binary`, `binary_imm`, `unary`, `convert`, `ternary`, `overflow`, `icmp`, `icmp_imm`, `fcmp`, `isplit`, `iconcat`, `emit` |
| Memory and slots | `load`, `store`, `stack_slot`, `stack_addr`, `stack_load`, `stack_store` |
| SIMD and atomics | `vconst`, `shuffle`, `extractlane`, `insertlane`, `atomic_rmw`, `atomic_load`, `atomic_store`, `atomic_cas` |
| Control flow | `call`, `call_indirect`, `return_call`, `call_memmove`, `call_memset`, `jump`, `brif`, `switch`, `return`, `trap`, `trap_if` |
| Compilation and definition | `compile`, `define_function`, `define_function_bytes`, `module_keep_clif`, `module_clif`, `module_code_bytes`, `module_last_clif`, `module_last_code`, `module_last_unwind`, `module_last_traps`, `module_last_relocs`, `module_last_call_sites`, `module_last_frame_size` |
| Object serialization and transfer | `module_finish`, `object_len`, `object_read`, `object_publish`, `object_free` |
| JIT finalization and lookup | `module_finalize`, `function_address` |

Instruction families carry operation tables rather than one entry per
opcode. [`codes.rs`](../../crates/vesper-cranelift-sys/src/codes.rs) specifies
the integer/float comparisons, binary, immediate, unary, conversion,
overflow, load/store, ternary, atomic, relocation, memory flag, and trap
codes. [`lib.rs`](../../crates/vesper-cranelift-sys/src/lib.rs) specifies the
batched `emit` record and references to prior results, alongside the
individual builder instructions. [`extras.rs`](../../crates/vesper-cranelift-sys/src/extras.rs)
specifies compiled-byte and relocation records. Preserve multi-result
semantics for overflow and splits, symbol colocation, invalid-handle
behavior, builder ownership, and the distinction between compiling for
inspection and defining a symbol.

The compiler's Vesper lowering packages under `packages/` import wrappers
that reference 104 of these exports, counting transitive wrapper calls.
The 29 below have no reference from those imports on this base. This
source inventory includes opt-in packages and counts wrapper imports
independently of runtime dispatch or dead-code elimination.

| Surface | Additional exports |
|---|---|
| Builder and instructions | `builder_error`, `builder_free`, `emit`, `get_stack_pointer`, `import_function_colocated`, `return_call`, `switch` |
| Signature and declaration read-back | `sig_param_ext`, `sig_return_ext`, `module_function_at`, `module_function_count`, `module_function_linkage`, `module_function_name`, `module_function_signature`, `module_data_at`, `module_data_count`, `module_data_linkage`, `module_data_name` |
| Compilation for inspection | `compile`, `module_code_bytes`, `module_last_call_sites`, `module_last_clif`, `module_last_code`, `module_last_frame_size`, `module_last_relocs`, `module_last_unwind` |
| Owned JIT module | `jit_module_new`, `module_finalize`, `function_address` |

### Shim callers and ownership

- [`codegen_swap`](../../src/codegen_swap.rs) and
  [hybrid swap adapters](../../crates/vesper-hybrid/src/swap) lend live
  Cranelift builders/modules for scalar, control, calls, numeric, sequence,
  aggregate, drop/FFI, and export glue emission. The corresponding Vesper
  packages import the typed wrappers from [`@vesper/cranelift`](../../packages/vesper-cranelift/src/lib.ves).
  The Rust entry points are `lend`, `lend_as`, and `lend_module`
  (`lend_module_as` is the explicit-kind variant). Pause/return handling
  gives the existing builder back to Rust for continued lowering.
- [Native walk body emission](../../src/codegen_swap/walk.rs) also uses
  lent builder/module state. Its handoff is owned by #2094. The checked
  world (#2024), the Cranelift-under-Wasm bridge, the run evaluator (#2053),
  and LSP are separate ownership boundaries.
- `symbols()` enumerates the ABI for host registration, and generated port
  bindings retain references to it for linking. Retention establishes
  availability, not that every entry is called by a given production
  configuration. `take_published`, `live_handles`, the Rust
  `module_clif` accessor, and the `codes` helpers are additional Rust-side
  surfaces used for bridge plumbing or oracle tests.
- [`vesper-simd`](../../crates/vesper-simd/src/lib.rs) has a separate scanner
  JIT/object path, including an SSE4.2 ISA preset and a
  `lend_module_as` scanner port. It is outside the compiler closure on
  this base. Treat it as a downstream consumer of the shim.
- The compiler's native object code also uses helpers or verification in
  [Silicon](../../crates/vesper-silicon/src), [Vault](../../crates/vesper-vault/src),
  and [fiber ABI](../../crates/vesper-fiber-abi/src). These dependencies are
  additional to the shim. Wasmtime's separate Cranelift dependency is a
  runtime dependency and sits outside the backend swap.

## Evidence before switching a row

Compare native bytes together with link-visible symbols and relocations,
and compare execution results, diagnostics, and traps on the same native
corpus inputs. Record differences explicitly. Measure compile time and
generated-code runtime with paired ABBA samples under release
`RUSTFLAGS="-C target-cpu=native"`. Both 95% CI upper bounds must be at or
below 1.50× and the differential must have zero diffs before a flip.
