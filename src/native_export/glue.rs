//! Export ABI v2 runtime entry points and handle drop glue (#898).
//!
//! Every v2 library exports `vesper_<lib>_seq_new`, `vesper_<lib>_seq_free`
//! and `vesper_<lib>_live_allocations`, and one `vesper_<lib>_<Type>_drop`
//! per non-linear handle type. The glue walks the handle's words with the
//! interface's field offsets: a sequence field drops its elements when they
//! own something (handles, boxes, strings; #1087) and is then freed, a
//! `String` field's buffer is freed, a box field has its pointee dropped and
//! is then freed (a boxed trait object through its vtable's drop word), and
//! nested handles, `Option`s and enum variants recurse. Each handle type
//! gets one function, so recursive types recurse through calls.
use super::*;
use cranelift_codegen::ir::{self, AbiParam, InstBuilder, MemFlags, condcodes::IntCC, types};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext};
use cranelift_module::{FuncId, Linkage, Module};

struct Runtime {
    seq_new: FuncId,
    seq_free: FuncId,
    str_release: FuncId,
    box_free: FuncId,
    /// Drop function of every handle type, by name.
    drops: BTreeMap<String, FuncId>,
}

fn word_sig<M: Module>(m: &M, params: usize, ret: bool) -> ir::Signature {
    let mut s = m.make_signature();
    s.params
        .extend((0..params).map(|_| AbiParam::new(types::I64)));
    if ret {
        s.returns.push(AbiParam::new(types::I64));
    }
    s
}

pub(super) fn define<M: Module>(
    m: &mut M,
    world: &World,
    interface: &LibraryInterface,
) -> Result<(), String> {
    let e = |e: cranelift_module::ModuleError| e.to_string();
    let _ = world;
    let symbols = interface
        .runtime
        .as_ref()
        .ok_or("a v2 library lists its runtime symbols")?;
    super::super::seq_native::aot_runtime::define(m)?;
    let box_alloc = crate::fip::aot_runtime::declare_or_define(m)?;
    let _ = box_alloc;
    let box_free = crate::fip::aot_runtime::declare_or_define_free(m)?;
    let seq_new_local = m
        .declare_function("vesper_seq_new", Linkage::Local, &word_sig(m, 2, true))
        .map_err(e)?;
    let seq_free_local = m
        .declare_function("vesper_seq_free", Linkage::Local, &word_sig(m, 1, true))
        .map_err(e)?;
    let str_release = m
        .declare_function("vesper_str_release", Linkage::Local, &word_sig(m, 1, true))
        .map_err(e)?;
    let mut rt = Runtime {
        seq_new: seq_new_local,
        seq_free: seq_free_local,
        str_release,
        box_free,
        drops: BTreeMap::new(),
    };
    // Exported entry points.
    let seq_new = m
        .declare_function(&symbols.seq_new, Linkage::Export, &word_sig(m, 2, true))
        .map_err(e)?;
    body(m, seq_new, 2, |b, m, args| {
        let r = call(b, m, rt.seq_new, &[args[0], args[1]]);
        b.ins().return_(&[r[0]]);
    })?;
    let seq_free = m
        .declare_function(&symbols.seq_free, Linkage::Export, &word_sig(m, 1, true))
        .map_err(e)?;
    body(m, seq_free, 1, |b, m, args| {
        call(b, m, rt.seq_free, &[args[0]]);
        let zero = b.ins().iconst(types::I64, 0);
        b.ins().return_(&[zero]);
    })?;
    let live = m
        .declare_function(
            &symbols.live_allocations,
            Linkage::Export,
            &word_sig(m, 0, true),
        )
        .map_err(e)?;
    let counters = [
        super::super::seq_native::aot_runtime::LIVE_COUNTER,
        crate::fip::aot_runtime::LIVE_COUNTER,
    ]
    .iter()
    .filter_map(|name| match m.get_name(name) {
        Some(cranelift_module::FuncOrDataId::Data(id)) => Some(id),
        _ => None,
    })
    .collect::<Vec<_>>();
    body(m, live, 0, |b, m, _| {
        let mut total = b.ins().iconst(types::I64, 0);
        for id in &counters {
            let data = m.declare_data_in_func(*id, b.func);
            let address = b.ins().symbol_value(types::I64, data);
            let value = b
                .ins()
                .atomic_load(types::I64, MemFlags::trusted(), address);
            total = b.ins().iadd(total, value);
        }
        b.ins().return_(&[total]);
    })?;
    // Drop glue: declare every handle type's function first, since types
    // may refer to each other (and to themselves) through boxes.
    for decl in interface.types.values().filter(|d| d.handle) {
        let (name, linkage) = match &decl.drop_symbol {
            Some(symbol) => (symbol.clone(), Linkage::Export),
            None => (
                format!("vesper_{}_{}_drop_local", interface.library, decl.name),
                Linkage::Local,
            ),
        };
        let id = m
            .declare_function(&name, linkage, &word_sig(m, 1, true))
            .map_err(e)?;
        rt.drops.insert(decl.name.clone(), id);
    }
    for decl in interface.types.values().filter(|d| d.handle) {
        let id = rt.drops[&decl.name];
        let types_map = &interface.types;
        let rt = &rt;
        body(m, id, 1, |b, m, args| {
            drop_decl(b, m, rt, types_map, decl, args[0]);
            let zero = b.ins().iconst(types::I64, 0);
            b.ins().return_(&[zero]);
        })?;
    }
    Ok(())
}

/// Drop the fields of a handle value at `base`.
fn drop_decl<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    rt: &Runtime,
    types: &BTreeMap<String, TypeDecl>,
    decl: &TypeDecl,
    base: ir::Value,
) {
    if !decl.is_enum {
        for f in &decl.variants[0].fields {
            drop_at(b, m, rt, types, &f.ty, base, f.offset);
        }
        return;
    }
    let tag = b.ins().load(types::I64, MemFlags::trusted(), base, 0);
    let done = b.create_block();
    for (index, v) in decl.variants.iter().enumerate() {
        if !v.fields.iter().any(|f| owning(&f.ty, types)) {
            continue;
        }
        let this = b.create_block();
        let next = b.create_block();
        let hit = b.ins().icmp_imm(IntCC::Equal, tag, index as i64);
        b.ins().brif(hit, this, &[], next, &[]);
        b.switch_to_block(this);
        for f in &v.fields {
            drop_at(b, m, rt, types, &f.ty, base, f.offset);
        }
        b.ins().jump(done, &[]);
        b.switch_to_block(next);
    }
    b.ins().jump(done, &[]);
    b.switch_to_block(done);
}

/// Drop a value of `ty` stored at word `offset` of `base`.
fn drop_at<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    rt: &Runtime,
    types: &BTreeMap<String, TypeDecl>,
    ty: &AbiType,
    base: ir::Value,
    offset: u32,
) {
    if !owning(ty, types) {
        return;
    }
    let at = (offset * 8) as i32;
    match ty {
        AbiType::Seq { elem } if owning(elem, types) => {
            // An empty slot or a moved-from field holds handle 0.
            let handle = b.ins().load(types::I64, MemFlags::trusted(), base, at);
            let live = b.create_block();
            let done = b.create_block();
            b.ins().brif(handle, live, &[], done, &[]);
            b.switch_to_block(live);
            let len = b.ins().load(types::I64, MemFlags::trusted(), handle, 0);
            let data = b.ins().load(types::I64, MemFlags::trusted(), handle, 16);
            let stride = i64::from(abi_words(elem, types)) * 8;
            let head = b.create_block();
            b.append_block_param(head, types::I64);
            let body = b.create_block();
            let exit = b.create_block();
            let zero = b.ins().iconst(types::I64, 0);
            b.ins().jump(head, &[zero.into()]);
            b.switch_to_block(head);
            let i = b.block_params(head)[0];
            let more = b.ins().icmp(IntCC::UnsignedLessThan, i, len);
            b.ins().brif(more, body, &[], exit, &[]);
            b.switch_to_block(body);
            let offset = b.ins().imul_imm(i, stride);
            let element = b.ins().iadd(data, offset);
            drop_at(b, m, rt, types, elem, element, 0);
            let next = b.ins().iadd_imm(i, 1);
            b.ins().jump(head, &[next.into()]);
            b.switch_to_block(exit);
            call(b, m, rt.seq_free, &[handle]);
            b.ins().jump(done, &[]);
            b.switch_to_block(done);
        }
        AbiType::Seq { .. } => {
            let handle = b.ins().load(types::I64, MemFlags::trusted(), base, at);
            call(b, m, rt.seq_free, &[handle]);
        }
        AbiType::Box { inner } if matches!(**inner, AbiType::Object { .. }) => {
            // A boxed object (§7.3): the block `{vtable, value}`; the
            // vtable's word 0 drops the value (0 when it owns nothing).
            let block = b.ins().load(types::I64, MemFlags::trusted(), base, at);
            let live = b.create_block();
            let call_drop = b.create_block();
            let free = b.create_block();
            let done = b.create_block();
            b.ins().brif(block, live, &[], done, &[]);
            b.switch_to_block(live);
            let table = b.ins().load(types::I64, MemFlags::trusted(), block, 0);
            let drop = b
                .ins()
                .load(types::I64, MemFlags::trusted().with_readonly(), table, 0);
            b.ins().brif(drop, call_drop, &[], free, &[]);
            b.switch_to_block(call_drop);
            let value = b.ins().iadd_imm(block, crate::dyn_trait::BOXED_HEADER);
            let mut sig = m.make_signature();
            sig.params.push(AbiParam::new(types::I64));
            let sig = b.import_signature(sig);
            b.ins().call_indirect(sig, drop, &[value]);
            b.ins().jump(free, &[]);
            b.switch_to_block(free);
            call(b, m, rt.box_free, &[block]);
            b.ins().jump(done, &[]);
            b.switch_to_block(done);
        }
        AbiType::Box { inner } => {
            let pointer = b.ins().load(types::I64, MemFlags::trusted(), base, at);
            drop_at(b, m, rt, types, inner, pointer, 0);
            call(b, m, rt.box_free, &[pointer]);
        }
        AbiType::Tuple { items } => {
            let mut word = offset;
            for item in items {
                drop_at(b, m, rt, types, item, base, word);
                word += abi_words(item, types);
            }
        }
        AbiType::Named { name, args } if name == "Option" || name == "Result" => {
            let tag = b.ins().load(types::I64, MemFlags::trusted(), base, at);
            let done = b.create_block();
            for (index, arg) in args.iter().enumerate() {
                // Option: tag 1 is Some(args[0]); Result: tag 0 is Ok, 1 Err.
                let tag_value = if name == "Option" { 1 } else { index as i64 };
                if !owning(arg, types) {
                    continue;
                }
                let this = b.create_block();
                let next = b.create_block();
                let hit = b.ins().icmp_imm(IntCC::Equal, tag, tag_value);
                b.ins().brif(hit, this, &[], next, &[]);
                b.switch_to_block(this);
                drop_at(b, m, rt, types, arg, base, offset + 1);
                b.ins().jump(done, &[]);
                b.switch_to_block(next);
            }
            b.ins().jump(done, &[]);
            b.switch_to_block(done);
        }
        AbiType::Named { name, .. } => {
            if let Some(id) = rt.drops.get(name) {
                let address = b.ins().iadd_imm(base, i64::from(at));
                call(b, m, *id, &[address]);
            }
        }
        AbiType::Str { owned: true } => {
            let address = b.ins().iadd_imm(base, i64::from(at));
            call(b, m, rt.str_release, &[address]);
        }
        AbiType::Scalar { .. }
        | AbiType::Slice { .. }
        | AbiType::Str { owned: false }
        | AbiType::Object { .. } => {}
    }
}

fn call<M: Module>(
    b: &mut FunctionBuilder,
    m: &mut M,
    id: FuncId,
    args: &[ir::Value],
) -> Vec<ir::Value> {
    let r = m.declare_func_in_func(id, b.func);
    let inst = b.ins().call(r, args);
    b.inst_results(inst).to_vec()
}

fn body<M: Module>(
    m: &mut M,
    id: FuncId,
    params: usize,
    build: impl FnOnce(&mut FunctionBuilder, &mut M, &[ir::Value]),
) -> Result<(), String> {
    let mut ctx = m.make_context();
    ctx.func.signature = word_sig(m, params, true);
    let mut fbc = FunctionBuilderContext::new();
    {
        let mut b = FunctionBuilder::new(&mut ctx.func, &mut fbc);
        let entry = b.create_block();
        b.append_block_params_for_function_params(entry);
        b.switch_to_block(entry);
        let args = b.block_params(entry).to_vec();
        build(&mut b, m, &args);
        b.seal_all_blocks();
        b.finalize();
    }
    m.define_function(id, &mut ctx).map_err(|e| e.to_string())?;
    m.clear_context(&mut ctx);
    Ok(())
}
