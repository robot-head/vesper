/* vesper_cranelift.h: the C ABI of crates/vesper-cranelift-sys, a shim over
 * Cranelift 0.125 (self-hosting wave SH0, lane 4, #1126).
 *
 * Link the crate's staticlib (libvesper_cranelift_sys.a /
 * vesper_cranelift_sys.lib). Handles are opaque; each has one finalizer and
 * the consuming calls (vcl_define_function, vcl_define_data,
 * vcl_module_finish, vcl_object_publish) take ownership. Values, blocks,
 * variables, stack slots and references are Cranelift entity numbers;
 * VCL_INVALID marks a failed call. Type, opcode and condition codes are
 * listed in src/codes.rs (tests/header.rs checks this file against it).
 */
#ifndef VESPER_CRANELIFT_H
#define VESPER_CRANELIFT_H

#include <stdbool.h>
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct vcl_module vcl_module;
typedef struct vcl_builder vcl_builder;
typedef struct vcl_data vcl_data;
typedef struct vcl_object vcl_object;

#define VCL_INVALID 0xffffffffu
#define VCL_BATCH_RESULT (1ull << 63)

/* Types: lane | log2(lanes) << 8. */
#define VCL_I8 1u
#define VCL_I16 2u
#define VCL_I32 3u
#define VCL_I64 4u
#define VCL_I128 5u
#define VCL_F32 6u
#define VCL_F64 7u

vcl_module *vcl_module_new(const uint8_t *triple, size_t triple_len, const uint8_t *flags, size_t flags_len, const uint8_t *name, size_t name_len);
uint64_t vcl_new_error(uint8_t *out, size_t out_len);
void vcl_module_free(vcl_module *m);
uint64_t vcl_module_error(const vcl_module *m, uint8_t *out, size_t out_len);
uint32_t vcl_module_pointer_type(const vcl_module *m);
void vcl_module_keep_clif(vcl_module *m, bool keep);
uint64_t vcl_module_code_bytes(const vcl_module *m);
uint64_t vcl_module_clif(const vcl_module *m, uint64_t offset, uint8_t *out, size_t out_len);
uint32_t vcl_sig_new(vcl_module *m, uint32_t call_conv);
uint32_t vcl_sig_param(vcl_module *m, uint32_t sig, uint32_t ty_code);
uint32_t vcl_sig_return(vcl_module *m, uint32_t sig, uint32_t ty_code);
uint32_t vcl_declare_function(vcl_module *m, const uint8_t *name, size_t name_len, uint32_t linkage, uint32_t sig);
uint32_t vcl_declare_data(vcl_module *m, const uint8_t *name, size_t name_len, uint32_t linkage, bool writable, bool tls);
uint32_t vcl_define_function(vcl_module *m, uint32_t func, vcl_builder *b);
uint32_t vcl_define_data(vcl_module *m, uint32_t data, vcl_data *d);
vcl_object *vcl_module_finish(vcl_module *m);
uint64_t vcl_object_len(const vcl_object *o);
uint64_t vcl_object_read(const vcl_object *o, uint64_t offset, uint8_t *out, size_t out_len);
uint64_t vcl_object_publish(vcl_object *o);
void vcl_object_free(vcl_object *o);
vcl_data *vcl_data_new(void);
void vcl_data_free(vcl_data *d);
void vcl_data_define(vcl_data *d, const uint8_t *bytes, size_t len);
void vcl_data_zeroinit(vcl_data *d, uint64_t size);
void vcl_data_align(vcl_data *d, uint64_t align);
void vcl_data_func_addr(vcl_data *d, const vcl_module *m, uint32_t offset, uint32_t func);
void vcl_data_data_addr(vcl_data *d, const vcl_module *m, uint32_t offset, uint32_t data, int64_t addend);
vcl_builder *vcl_builder_new(const vcl_module *m, uint32_t sig);
void vcl_builder_free(vcl_builder *b);
uint64_t vcl_builder_error(const vcl_builder *b, uint8_t *out, size_t out_len);
uint32_t vcl_import_function(vcl_builder *b, vcl_module *m, uint32_t func);
uint32_t vcl_import_signature(vcl_builder *b, const vcl_module *m, uint32_t sig);
uint32_t vcl_import_data(vcl_builder *b, const vcl_module *m, uint32_t data);
uint32_t vcl_create_block(vcl_builder *b);
uint32_t vcl_append_block_param(vcl_builder *b, uint32_t blk, uint32_t ty_code);
uint32_t vcl_append_block_params_for_function_params(vcl_builder *b, uint32_t blk);
uint32_t vcl_block_param(vcl_builder *b, uint32_t blk, uint32_t i);
void vcl_switch_to_block(vcl_builder *b, uint32_t blk);
void vcl_seal_block(vcl_builder *b, uint32_t blk);
void vcl_seal_all_blocks(vcl_builder *b);
uint32_t vcl_declare_var(vcl_builder *b, uint32_t ty_code);
void vcl_def_var(vcl_builder *b, uint32_t var, uint32_t v);
uint32_t vcl_use_var(vcl_builder *b, uint32_t var);
uint32_t vcl_stack_slot(vcl_builder *b, uint32_t size, uint32_t align_shift);
uint32_t vcl_inst_result(vcl_builder *b, uint32_t i);
uint32_t vcl_value_type(vcl_builder *b, uint32_t v);
uint32_t vcl_iconst(vcl_builder *b, uint32_t ty_code, int64_t imm);
uint32_t vcl_f32const(vcl_builder *b, uint32_t bits);
uint32_t vcl_f64const(vcl_builder *b, uint64_t bits);
uint32_t vcl_binary(vcl_builder *b, uint32_t op, uint32_t x, uint32_t y);
uint32_t vcl_binary_imm(vcl_builder *b, uint32_t op, uint32_t x, int64_t imm);
uint32_t vcl_unary(vcl_builder *b, uint32_t op, uint32_t x);
uint32_t vcl_convert(vcl_builder *b, uint32_t op, uint32_t ty_code, uint32_t x);
uint32_t vcl_ternary(vcl_builder *b, uint32_t op, uint32_t x, uint32_t y, uint32_t z);
uint32_t vcl_overflow(vcl_builder *b, uint32_t op, uint32_t x, uint32_t y);
uint32_t vcl_icmp(vcl_builder *b, uint32_t cc, uint32_t x, uint32_t y);
uint32_t vcl_icmp_imm(vcl_builder *b, uint32_t cc, uint32_t x, int64_t imm);
uint32_t vcl_fcmp(vcl_builder *b, uint32_t cc, uint32_t x, uint32_t y);
uint32_t vcl_load(vcl_builder *b, uint32_t op, uint32_t ty_code, uint32_t fl, uint32_t addr, int32_t offset);
void vcl_store(vcl_builder *b, uint32_t op, uint32_t fl, uint32_t x, uint32_t addr, int32_t offset);
uint32_t vcl_stack_addr(vcl_builder *b, uint32_t ty_code, uint32_t slot, int32_t offset);
uint32_t vcl_stack_load(vcl_builder *b, uint32_t ty_code, uint32_t slot, int32_t offset);
void vcl_stack_store(vcl_builder *b, uint32_t x, uint32_t slot, int32_t offset);
uint32_t vcl_extractlane(vcl_builder *b, uint32_t x, uint32_t lane);
uint32_t vcl_insertlane(vcl_builder *b, uint32_t x, uint32_t y, uint32_t lane);
uint32_t vcl_vconst(vcl_builder *b, uint32_t ty_code, const uint8_t *bytes, size_t len);
uint32_t vcl_shuffle(vcl_builder *b, uint32_t x, uint32_t y, const uint8_t *mask, size_t len);
uint32_t vcl_atomic_rmw(vcl_builder *b, uint32_t op, uint32_t ty_code, uint32_t fl, uint32_t addr, uint32_t x);
uint32_t vcl_atomic_load(vcl_builder *b, uint32_t ty_code, uint32_t fl, uint32_t addr);
void vcl_atomic_store(vcl_builder *b, uint32_t fl, uint32_t x, uint32_t addr);
uint32_t vcl_func_addr(vcl_builder *b, uint32_t ty_code, uint32_t fref);
uint32_t vcl_symbol_value(vcl_builder *b, uint32_t ty_code, uint32_t gv);
uint32_t vcl_global_value(vcl_builder *b, uint32_t ty_code, uint32_t gv);
uint32_t vcl_get_stack_pointer(vcl_builder *b, uint32_t ty_code);
uint32_t vcl_call(vcl_builder *b, uint32_t fref, const uint32_t *args, size_t args_len);
uint32_t vcl_call_indirect(vcl_builder *b, uint32_t sigref, uint32_t callee, const uint32_t *args, size_t args_len);
void vcl_return_call(vcl_builder *b, uint32_t fref, const uint32_t *args, size_t args_len);
void vcl_jump(vcl_builder *b, uint32_t blk, const uint32_t *args, size_t args_len);
void vcl_brif(vcl_builder *b, uint32_t c, uint32_t then_blk, const uint32_t *then_args, size_t then_len, uint32_t else_blk, const uint32_t *else_args, size_t else_len);
void vcl_return(vcl_builder *b, const uint32_t *vals, size_t len);
void vcl_trap(vcl_builder *b, uint32_t code);
void vcl_trap_if(vcl_builder *b, uint32_t c, bool if_nonzero, uint32_t code);
uint32_t vcl_emit(vcl_builder *b, const uint64_t *code, size_t code_len, uint32_t *out, size_t out_len);

/* JIT modules, compiling without defining, symbol names and Windows unwind
 * information (SH4 lane 1, #1275). */
vcl_module *vcl_jit_module_new(const uint8_t *flags, size_t flags_len, const uint8_t *symbols, size_t symbols_len, const uint64_t *addrs, size_t addrs_len);
uint32_t vcl_module_finalize(vcl_module *m);
uint64_t vcl_function_address(vcl_module *m, uint32_t func);
uint32_t vcl_module_function_count(const vcl_module *m);
uint64_t vcl_module_function_name(const vcl_module *m, uint32_t func, uint8_t *out, size_t out_len);
/* The function declared as `name`, or UINT32_MAX. */
uint32_t vcl_module_get_function(vcl_module *m, const uint8_t *name, size_t name_len);
/* The data object declared as `name`, or UINT32_MAX. */
uint32_t vcl_module_get_data(vcl_module *m, const uint8_t *name, size_t name_len);
/* 0 object, 1 JIT, 2 object for a target with no operating system. */
uint32_t vcl_module_kind(const vcl_module *m);
uint64_t vcl_compile(vcl_module *m, vcl_builder *b);
uint64_t vcl_module_last_code(const vcl_module *m, uint64_t offset, uint8_t *out, size_t out_len);
uint64_t vcl_module_last_unwind(const vcl_module *m, uint8_t *out, size_t out_len);
/* Trap records of the last compiled or defined function: offset and code
 * words per trap. Returns the word count. */
uint64_t vcl_module_last_traps(const vcl_module *m, uint64_t *out, size_t out_len);
/* vcl_import_data with an explicit colocated flag. */
uint32_t vcl_import_data_colocated(vcl_builder *b, const vcl_module *m, uint32_t data, bool colocated);

/* Builder extras. */
void vcl_set_srcloc(vcl_builder *b, uint32_t loc);
void vcl_set_cold_block(vcl_builder *b, uint32_t blk);
uint32_t vcl_current_block(vcl_builder *b);
void vcl_set_func_name(vcl_builder *b, uint32_t ns, uint32_t index);
uint32_t vcl_import_function_colocated(vcl_builder *b, vcl_module *m, uint32_t func, bool colocated);
void vcl_switch(vcl_builder *b, uint32_t x, uint32_t otherwise, const uint64_t *indices, size_t indices_len, const uint32_t *blocks, size_t blocks_len);

/* Lent handles: a hook's view of one call a Rust host lends its builder and
 * module to (src/lend.rs). `call` is the word the hook was given. */
uint32_t vcl_call_op(uint64_t call);
uint64_t vcl_call_arg_count(uint64_t call);
uint64_t vcl_call_arg(uint64_t call, uint64_t i);
uint64_t vcl_call_args(uint64_t call, uint64_t *out, size_t out_len);
void vcl_call_result(uint64_t call, uint64_t w);
vcl_builder *vcl_call_builder(uint64_t call);
vcl_module *vcl_call_module(uint64_t call);
void vcl_call_return_builder(uint64_t call, vcl_builder *b);
void vcl_call_return_module(uint64_t call, vcl_module *m);

#ifdef __cplusplus
}
#endif

#endif /* VESPER_CRANELIFT_H */
