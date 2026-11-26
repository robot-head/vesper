//! `@vesper/wit-component` wraps core modules that import the Cranelift
//! shim (`vesper:cranelift/vcl@0.1.0`, #2210) byte for byte as
//! wit-component 0.261 does.
//!
//! The port keeps the interface's instance type and each function's
//! lowering in packages/vesper-wit-component/src/vcl.ves, generated here
//! from Rust's encoding of the checked-in WIT
//! (crates/vesper-cranelift-component/wit/vcl.wit). `VESPER_BLESS=1`
//! rewrites it.
#[path = "common/check_harness.rs"]
mod check_harness;

use check_harness::*;
use std::fmt::Write as _;
use std::sync::OnceLock;
use wasm_encoder::{
    CodeSection, ExportKind, ExportSection, Function, FunctionSection, ImportSection,
    Instruction as I, MemorySection, MemoryType, Module, TypeSection, ValType,
};
use wit_parser::abi::WasmType;
use wit_parser::{Resolve, Type, TypeDefKind};

const VCL_WIT: &str = include_str!("../crates/vesper-cranelift-component/wit/vcl.wit");
const VCL: &str = "vesper:cranelift/vcl@0.1.0";
const STDOUT_IMPORTS: &str = "  import wasi:cli/stdout@0.2.0;\n";
const IO_WIT: &str = "package wasi:io@0.2.0;\n\ninterface error {\n  resource error;\n}\n\ninterface streams {\n  use error.{error};\n  variant stream-error {\n    last-operation-failed(error),\n    closed,\n  }\n  resource output-stream {\n    blocking-write-and-flush: func(contents: list<u8>) -> result<_, stream-error>;\n  }\n}\n";
const CLI_WIT: &str = "package wasi:cli@0.2.0;\n\ninterface run {\n  run: func() -> result;\n}\n\ninterface stdout {\n  use wasi:io/streams@0.2.0.{output-stream};\n  get-stdout: func() -> output-stream;\n}\n";

/// One vcl function: its name, core signature and lowering (the port's
/// `L_DIRECT` 1, `L_MEMORY` 2, `L_MEMORY_REALLOC` 4).
struct VclFunc {
    name: String,
    params: Vec<ValType>,
    results: Vec<ValType>,
    lower: u8,
}

fn val(t: WasmType) -> ValType {
    match t {
        WasmType::I32 | WasmType::Pointer | WasmType::Length => ValType::I32,
        WasmType::I64 | WasmType::PointerOrI64 => ValType::I64,
        WasmType::F32 => ValType::F32,
        WasmType::F64 => ValType::F64,
    }
}

fn has_list(resolve: &Resolve, t: &Type) -> bool {
    match t {
        Type::String => true,
        Type::Id(id) => match &resolve.types[*id].kind {
            TypeDefKind::List(_) => true,
            TypeDefKind::Tuple(t) => t.types.iter().any(|x| has_list(resolve, x)),
            TypeDefKind::Option(x) => has_list(resolve, x),
            TypeDefKind::Type(x) => has_list(resolve, x),
            _ => false,
        },
        _ => false,
    }
}

fn vcl_funcs() -> &'static [VclFunc] {
    static F: OnceLock<Vec<VclFunc>> = OnceLock::new();
    F.get_or_init(|| {
        let mut resolve = Resolve::default();
        let pkg = resolve.push_source("vcl.wit", VCL_WIT).unwrap();
        let iface = resolve.packages[pkg].interfaces["vcl"];
        let funcs: Vec<_> = resolve.interfaces[iface]
            .functions
            .values()
            .cloned()
            .collect();
        funcs
            .iter()
            .map(|f| {
                let sig = resolve.wasm_signature(wit_parser::abi::AbiVariant::GuestImport, f);
                let lists_in = f.params.iter().any(|p| has_list(&resolve, &p.ty));
                let lists_out = f.result.as_ref().is_some_and(|r| has_list(&resolve, r));
                let lower = if lists_out {
                    4
                } else if lists_in || sig.retptr || sig.indirect_params {
                    2
                } else {
                    1
                };
                VclFunc {
                    name: f.name.clone(),
                    params: sig.params.into_iter().map(val).collect(),
                    results: sig.results.into_iter().map(val).collect(),
                    lower,
                }
            })
            .collect()
    })
}

/// A command core module importing `imps` (module, field, signature).
fn make_core(imps: &[(&str, &str, Vec<ValType>, Vec<ValType>)]) -> Vec<u8> {
    let mut types = TypeSection::new();
    let mut imports = ImportSection::new();
    for (idx, (m, f, params, results)) in imps.iter().enumerate() {
        types.ty().function(params.clone(), results.clone());
        imports.import(m, f, wasm_encoder::EntityType::Function(idx as u32));
    }
    let base = imps.len() as u32;
    types.ty().function([], [ValType::I64]);
    types.ty().function([], [ValType::I32]);
    types.ty().function([ValType::I32; 4], [ValType::I32]);
    let mut funcs = FunctionSection::new();
    for k in 0..3 {
        funcs.function(base + k);
    }
    let mut mem = MemorySection::new();
    mem.memory(MemoryType {
        minimum: 1,
        maximum: None,
        memory64: false,
        shared: false,
        page_size_log2: None,
    });
    let mut exports = ExportSection::new();
    exports.export("memory", ExportKind::Memory, 0);
    exports.export("vesper:program/entry@0.1.0#main", ExportKind::Func, base);
    exports.export("wasi:cli/run@0.2.0#run", ExportKind::Func, base + 1);
    exports.export("cabi_realloc", ExportKind::Func, base + 2);
    let mut code = CodeSection::new();
    for c in [I::I64Const(0), I::I32Const(0), I::I32Const(0)] {
        let mut f = Function::new([]);
        f.instruction(&c);
        f.instruction(&I::End);
        code.function(&f);
    }
    let mut m = Module::new();
    m.section(&types);
    m.section(&imports);
    m.section(&funcs);
    m.section(&mem);
    m.section(&exports);
    m.section(&code);
    m.finish()
}

fn program_wit(imports: &str) -> String {
    format!(
        "package vesper:program@0.1.0;\n\ninterface entry {{\n  main: func() -> s64;\n}}\n\nworld program {{\n{imports}  export wasi:cli/run@0.2.0;\n  export entry;\n}}\n"
    )
}

fn rust_encode(core_in: &[u8], wit: &str) -> Vec<u8> {
    let mut core = core_in.to_vec();
    let mut resolve = Resolve::default();
    for (name, source) in [
        ("wasi-io.wit", IO_WIT),
        ("wasi-cli.wit", CLI_WIT),
        ("vcl.wit", VCL_WIT),
    ] {
        resolve.push_source(name, source).unwrap();
    }
    let pkg = resolve.push_source("vesper-program.wit", wit).unwrap();
    let id = resolve.select_world(&[pkg], Some("program")).unwrap();
    wit_component::embed_component_metadata(
        &mut core,
        &resolve,
        id,
        wit_component::StringEncoding::UTF8,
        false,
    )
    .unwrap();
    wit_component::ComponentEncoder::default()
        .validate(true)
        .module(&core)
        .unwrap()
        .encode()
        .unwrap()
}

fn leb(b: &[u8], pos: &mut usize) -> u64 {
    let (mut v, mut shift) = (0u64, 0);
    loop {
        let x = b[*pos];
        *pos += 1;
        v |= u64::from(x & 127) << shift;
        if x & 128 == 0 {
            return v;
        }
        shift += 7;
    }
}

/// The instance type Rust writes for the vcl import: the only item of the
/// first type section of a component whose one import is vcl.
fn rust_instance_type() -> Vec<u8> {
    let imps: Vec<_> = vcl_funcs()
        .iter()
        .map(|f| (VCL, f.name.as_str(), f.params.clone(), f.results.clone()))
        .collect();
    let c = rust_encode(
        &make_core(&imps),
        &program_wit(&format!("  import {VCL};\n")),
    );
    let mut pos = 8;
    loop {
        let id = c[pos];
        pos += 1;
        let size = leb(&c, &mut pos) as usize;
        let end = pos + size;
        if id == 7 {
            assert_eq!(leb(&c, &mut pos), 1, "one type in the first type section");
            return c[pos..end].to_vec();
        }
        pos = end;
    }
}

/// packages/vesper-wit-component/src/vcl.ves as it should be.
fn generated_vcl_ves() -> String {
    let funcs = vcl_funcs();
    let hex: String = rust_instance_type()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let mut s = String::new();
    s.push_str(
        "// Generated by tests/wasm_cranelift_wit_component.rs from\n\
         // crates/vesper-cranelift-component/wit/vcl.wit (#2210). Do not edit:\n\
         // rerun that test with VESPER_BLESS=1.\n\
         //\n\
         // The `vesper:cranelift/vcl@0.1.0` interface as wit-component 0.261\n\
         // imports it: its instance type, and each function's name and lowering\n\
         // (`vcl_lowers` holds one digit per function: 1 direct, 2 through\n\
         // memory, 4 through memory with cabi_realloc).\n\n",
    );
    let _ = write!(
        s,
        "pub fn vcl_func_count() -> u64 {{\n    {}\n}}\n\n",
        funcs.len()
    );
    s.push_str("pub fn vcl_func_name(i: u64) -> Str {\n");
    for (i, f) in funcs.iter().enumerate() {
        let _ = write!(
            s,
            "    if i == {i} {{\n        return \"{}\";\n    }}\n",
            f.name
        );
    }
    s.push_str("    \"\"\n}\n\n");
    let lowers: String = funcs.iter().map(|f| char::from(b'0' + f.lower)).collect();
    let _ = write!(s, "pub fn vcl_lowers() -> Str {{\n    \"{lowers}\"\n}}\n\n");
    let _ = write!(
        s,
        "pub fn vcl_instance_hex() -> Str {{\n    \"{hex}\"\n}}\n"
    );
    s
}

#[test]
fn the_checked_in_vcl_table_is_the_generated_one() {
    let path = repo().join("packages/vesper-wit-component/src/vcl.ves");
    let want = generated_vcl_ves();
    if std::env::var_os("VESPER_BLESS").is_some() {
        std::fs::write(&path, &want).unwrap();
        return;
    }
    let have = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        have == want,
        "{} is stale: rerun with VESPER_BLESS=1",
        path.display()
    );
}

fn package() -> &'static Package {
    static P: OnceLock<Package> = OnceLock::new();
    P.get_or_init(|| Package::load("packages/vesper-wit-component"))
}

fn wrap_input(core: &[u8], wit: &str) -> Vec<u8> {
    let mut v = vec![0];
    v.extend_from_slice(&(wit.len() as u32).to_le_bytes());
    v.extend_from_slice(wit.as_bytes());
    v.extend_from_slice(core);
    v
}

fn answer(words: &[u64]) -> Result<Vec<u8>, u64> {
    match words.first() {
        Some(0) => {
            let len = words[1] as usize;
            let mut bytes: Vec<u8> = words[2..].iter().flat_map(|w| w.to_le_bytes()).collect();
            bytes.truncate(len);
            Ok(bytes)
        }
        Some(_) => Err(words.get(1).copied().unwrap_or(0)),
        None => Err(0),
    }
}

#[test]
fn the_port_wraps_cranelift_imports_as_wit_component_does() {
    let all = vcl_funcs();
    let pick = |names: &[&str]| -> Vec<_> {
        all.iter()
            .filter(|f| names.is_empty() || names.contains(&f.name.as_str()))
            .map(|f| (VCL, f.name.as_str(), f.params.clone(), f.results.clone()))
            .collect()
    };
    let stdout = vec![
        (
            "wasi:cli/stdout@0.2.0",
            "get-stdout",
            vec![],
            vec![ValType::I32],
        ),
        (
            "wasi:io/streams@0.2.0",
            "[method]output-stream.blocking-write-and-flush",
            vec![ValType::I32; 4],
            vec![],
        ),
    ];
    // The whole interface (as the wasm backend imports it), alone and with
    // stdout before and after.
    let every = pick(&[]);
    let mut every_stdout = stdout.clone();
    every_stdout.extend(every.iter().cloned());
    let mut stdout_every = every.clone();
    stdout_every.extend(stdout.iter().cloned());
    let vcl = format!("  import {VCL};\n");
    let cases = [
        ("every", every.clone(), vcl.clone()),
        (
            "stdout+every",
            every_stdout,
            format!("{STDOUT_IMPORTS}{vcl}"),
        ),
        (
            "every+stdout",
            stdout_every,
            format!("{vcl}{STDOUT_IMPORTS}"),
        ),
    ];
    let wrap = package().entry("wit_component_wrap", true);
    let mut errs = vec![];
    for (label, imps, imports) in cases {
        let core = make_core(&imps);
        let wit = program_wit(&imports);
        let expected = rust_encode(&core, &wit);
        match answer(&wrap.words(&wrap_input(&core, &wit))) {
            Ok(actual) if actual == expected => {}
            Ok(actual) => {
                let pos = actual.iter().zip(&expected).position(|(a, b)| a != b);
                errs.push(format!(
                    "{label}: differs ({} vs {} bytes, first at {pos:?})",
                    actual.len(),
                    expected.len()
                ));
            }
            Err(reason) => errs.push(format!("{label}: declined ({reason})")),
        }
    }
    // wit-component keeps only the imported functions in the instance
    // type, so the port declines a module that imports part of vcl.
    let few = pick(&[
        "vcl-module-new",
        "vcl-new-error",
        "vcl-module-free",
        "vcl-iconst",
    ]);
    let wit = program_wit(&vcl);
    if let Ok(c) = answer(&wrap.words(&wrap_input(&make_core(&few), &wit))) {
        errs.push(format!(
            "few: wrapped ({} bytes) instead of declining",
            c.len()
        ));
    }
    assert!(errs.is_empty(), "diffs:\n{}", errs.join("\n"));
}
