//! The wasm backend port's input and output (SH4 lane 15, #1289), shared by
//! tests/self_host_wasm_backend.rs and the hybrid compiler's `wasm` hook
//! (crates/vesper-hybrid/src/hybrid.rs).
//!
//! - `wasm_input`: the check-data world and the Place IR (#1259's
//!   `ir_words`), then the wasm section: options, the entry, signatures,
//!   effect declarations, call-site type arguments and one expression fact
//!   per payload (closure captures and handler clauses).
//! - `outcome`: what `@vesper/wasm-backend`'s `wasm_backend_compile`
//!   returned. `artifact` turns a module into the `WasmArtifact` Rust's
//!   `compile` returns, wrapping it with wit-component.
#![allow(dead_code)]

use super::check_words::{Enc, pass_word};
use super::ir_words::{self, Payload, Tables};
use vesper::ast::{Effect, Effects, Expr, ExprKind, Function, Literal, Pattern, PatternKind};
use vesper::check::World;
use vesper::ir::Program;
use vesper::wasm_backend::{ENTRY_INTERFACE, EffectLowering, WasmArtifact, WasmOptions, WasmWorld};

// ------------------------------------------------------------------ input

/// Words of the wasm section, interning names and types through `enc`.
struct Sec<'a> {
    enc: &'a mut Enc,
    out: Vec<u64>,
}

impl Sec<'_> {
    fn raw(&mut self, x: u64) {
        self.out.push(x);
    }
    fn name(&mut self, s: &str) {
        let i = self.enc.name(s);
        self.out.push(i);
    }
    fn opt_name(&mut self, s: &Option<String>) {
        let i = self.enc.opt_name(s);
        self.out.push(i);
    }
    fn ty(&mut self, t: &vesper::ast::Type) {
        let i = self.enc.ty(t);
        self.out.push(i);
    }
    fn names<'s>(&mut self, ns: impl ExactSizeIterator<Item = &'s String>) {
        self.raw(ns.len() as u64);
        for n in ns {
            self.name(n);
        }
    }
    fn effect(&mut self, e: &Effect) {
        self.opt_name(&e.label);
        self.name(&e.name);
        self.raw(e.args.len() as u64);
        for t in &e.args {
            self.ty(t);
        }
    }
    fn row(&mut self, e: &Effects) {
        self.raw(e.instances.len() as u64);
        for i in &e.instances {
            self.effect(i);
        }
        self.opt_name(&e.tail);
    }
    /// A signature: generics (name, row), params (name, pass, type),
    /// result, effect row.
    fn signature(&mut self, f: &Function) {
        self.raw(f.generics.len() as u64);
        for g in &f.generics {
            self.name(&g.name);
            self.raw(g.row as u64);
        }
        self.raw(f.params.len() as u64);
        for p in &f.params {
            self.name(&p.name);
            self.raw(pass_word(p.pass));
            self.ty(&p.ty);
        }
        self.ty(&f.result);
        self.row(&f.effects);
    }
    /// Kind (0 int lo hi, 1 float, 2 bool, 3 string, 4 wide int), as in ir_words.
    fn literal(&mut self, l: &Literal) {
        match l {
            Literal::Int(n) => {
                self.raw(0);
                self.raw(*n as u64);
                self.raw((*n >> 64) as u64);
            }
            Literal::Float(x) => {
                self.raw(1);
                self.raw(x.to_bits());
                self.raw(0);
            }
            Literal::Bool(b) => {
                self.raw(2);
                self.raw(*b as u64);
                self.raw(0);
            }
            Literal::String(t) => {
                self.raw(3);
                self.name(t);
                self.raw(0);
            }
            // A literal past `i128::MAX`: two raw words, like an Int.
            Literal::WideInt(n) => {
                self.raw(4);
                self.raw(*n as u64);
                self.raw((*n >> 64) as u64);
            }
        }
    }
    /// A pattern in prefix order, as in ir_words: kind, span, parts.
    fn pattern(&mut self, p: &Pattern) {
        let (a, b) = (p.span.start as u64, p.span.end as u64);
        match &p.kind {
            PatternKind::Wild => self.out.extend([0, a, b]),
            PatternKind::Bind(n) => {
                self.out.extend([1, a, b]);
                self.name(n);
            }
            PatternKind::Literal(l) => {
                self.out.extend([2, a, b]);
                self.literal(l);
            }
            PatternKind::Tuple(ps) => {
                self.out.extend([3, a, b, ps.len() as u64]);
                for c in ps {
                    self.pattern(c);
                }
            }
            PatternKind::Constructor {
                name,
                fields,
                named,
            } => {
                self.out.extend([4, a, b]);
                self.name(name);
                self.raw(*named as u64);
                self.raw(fields.len() as u64);
                for (f, c) in fields {
                    self.name(f);
                    self.pattern(c);
                }
            }
        }
    }
    fn patterns(&mut self, ps: &[Pattern]) {
        self.raw(ps.len() as u64);
        for p in ps {
            self.pattern(p);
        }
    }
    /// The facts the backend reads from a closure or handler expression
    /// (the pool keeps the `Expr` on the Rust side).
    fn expr(&mut self, e: &Expr) {
        match &e.kind {
            ExprKind::Closure { params, body, .. } => {
                let mut free = vesper::check::free_names(body);
                for p in params {
                    free.remove(&p.name);
                }
                self.raw(1);
                self.name(&vesper::ir::nested_name("closure", &body.span));
                self.names(free.iter());
            }
            ExprKind::Handle {
                effect,
                state,
                body,
                ret,
                ops,
            } => {
                self.raw(2);
                self.names(vesper::check::free_names(e).iter());
                self.effect(effect);
                self.raw(state.is_some() as u64);
                self.name(&vesper::ir::nested_name("handler_body", &body.span));
                self.name(&vesper::ir::nested_name("handler_clause", &ret.body.span));
                self.patterns(&ret.pats);
                self.names(vesper::check::clause_free_names(ret).iter());
                self.raw(ops.len() as u64);
                for c in ops {
                    self.name(&c.name);
                    self.name(&vesper::ir::nested_name("handler_clause", &c.body.span));
                    self.patterns(&c.pats);
                    self.names(vesper::check::clause_free_names(c).iter());
                }
            }
            _ => self.raw(0),
        }
    }
}

/// Every name a world's effect rows mention that is a capability effect.
fn capability_names(w: &World) -> Vec<String> {
    let mut out = std::collections::BTreeSet::new();
    let mut row = |e: &Effects| {
        for i in &e.instances {
            if vesper::capability_effects::is_capability_effect(&i.name) {
                out.insert(i.name.clone());
            }
        }
    };
    for f in w.functions.values() {
        row(&f.effects);
    }
    for d in w.effects.values() {
        for o in &d.ops {
            row(&o.effects);
        }
    }
    out.into_iter().collect()
}

/// The input of `wasm_backend_compile`: check-data's names, types and
/// world, the Place IR program section (tests/common/ir_words.rs), then
/// the wasm section:
///
/// ```text
/// options: world, stack switching, max pages, max functions, max instructions
/// entry name, trace active, n capability effect names
/// functions: n, n x (name, signature)
/// effects: n, n x (name, linear, ngen, ngen names, nops, nops x (name, signature))
/// calls: n, n x (start, end, k, k x (name, type))
/// exprs: one per pool row: 0 | 1 closure fn, free | 2 handler (see `Sec::expr`)
/// ```
pub fn wasm_input(w: &World, program: &Program, entry: &str, o: &WasmOptions) -> Option<Vec<u8>> {
    if !ir_words::encodable(w, program) {
        return None;
    }
    let mut t = Tables::of(program);
    let labels: Vec<String> = t
        .pool
        .items
        .iter()
        .map(|p| match p {
            Payload::Seq(x) => format!("{x:?}"),
            Payload::Vector(x) => format!("{x:?}"),
            Payload::Tiled(x) => format!("{x:?}"),
            Payload::Fip(x) => format!("{x:?}"),
            Payload::Expr(_) => String::new(),
        })
        .collect();
    for l in &labels {
        t.enc.name(l);
    }
    let mut section = t.section();
    let world = t.enc.world(w);
    let items = t.pool.items.clone();
    let mut s = Sec {
        enc: &mut t.enc,
        out: vec![],
    };
    s.raw(match o.world {
        WasmWorld::Command => 0,
        WasmWorld::NexusPlugin => 1,
    });
    s.raw((o.effects == EffectLowering::StackSwitching) as u64);
    s.raw(o.max_memory_pages as u64);
    s.raw(o.max_functions as u64);
    s.raw(o.max_instructions as u64);
    s.name(entry);
    s.raw(vesper::trace_effects::active(w) as u64);
    let caps = capability_names(w);
    s.names(caps.iter());
    s.raw(w.functions.len() as u64);
    for (name, f) in &w.functions {
        s.name(name);
        s.signature(f);
    }
    s.raw(w.effects.len() as u64);
    for (name, d) in &w.effects {
        s.name(name);
        s.raw(d.linear as u64);
        s.raw(d.generics.len() as u64);
        for g in &d.generics {
            s.name(&g.name);
        }
        s.raw(d.ops.len() as u64);
        for op in &d.ops {
            s.name(&op.name);
            s.signature(op);
        }
    }
    s.raw(w.calls.len() as u64);
    for ((a, b), vars) in &w.calls {
        s.raw(*a as u64);
        s.raw(*b as u64);
        s.raw(vars.len() as u64);
        for (k, ty) in vars {
            s.name(k);
            s.ty(ty);
        }
    }
    for p in &items {
        match p {
            Payload::Expr(e) => s.expr(e),
            _ => s.raw(0),
        }
    }
    let extra = s.out;
    section.extend(extra);
    Some(t.enc.finish(&world, &section))
}

/// The program `wasm_backend::compile` lowers: `ir::lower_world`, then
/// `ir_opt::optimize` when the options ask for it.
pub fn backend_program(w: &World, o: &WasmOptions) -> Option<Program> {
    let mut p = vesper::ir::lower_world(w);
    if o.optimize && vesper::ir_opt::optimize(w, &mut p).is_err() {
        return None;
    }
    Some(p)
}

/// `Unsupported` constructs in declaration order (src/err.ves `U_*`).
const CONSTRUCTS: [&str; 16] = [
    "FunctionValue",
    "String",
    "FloatRemainder",
    "Index",
    "Projection",
    "GenericOperation",
    "GenericEffectInstance",
    "LabelledEffectRow",
    "InoutEffectArgument",
    "FirstClassContinuation",
    "NonTailResumptiveHandler",
    "ExternalFunction",
    "ExportType",
    "Operation",
    "IntegerWidth",
    "Float32",
];

/// What the Vesper backend returned.
#[derive(Debug)]
pub enum Outcome {
    Module {
        core: Vec<u8>,
        wit: String,
        /// `Builder::function_count` (imports and defined functions).
        functions: usize,
    },
    /// An error class as `error_class` spells it, and its site.
    Error(String, u64),
    /// A construct this port does not lower yet (handlers, host frames).
    NotPorted(u64),
}

fn unpack_bytes(words: &[u64]) -> (Vec<u8>, usize) {
    let len = words[0] as usize;
    let n = len.div_ceil(8);
    let mut bytes = Vec::with_capacity(n * 8);
    for w in &words[1..1 + n] {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    bytes.truncate(len);
    (bytes, 1 + n)
}

pub fn outcome(words: &[u64]) -> Outcome {
    if words[0] == 0 {
        let functions = words[1] as usize;
        let (core, used) = unpack_bytes(&words[2..]);
        let (wit, _) = unpack_bytes(&words[2 + used..]);
        return Outcome::Module {
            core,
            wit: String::from_utf8(wit).unwrap(),
            functions,
        };
    }
    let (kind, construct, site) = (words[1], words[2] as usize, words[3]);
    let class = match kind {
        0 => format!("Unsupported({})", CONSTRUCTS[construct]),
        1 => "Entry".into(),
        2 => "Limit".into(),
        3 => "Invalid".into(),
        4 => "Encode".into(),
        _ => return Outcome::NotPorted(site),
    };
    Outcome::Error(class, site)
}

/// The WASI packages a command world without host imports names
/// (src/wasm_backend/host.rs `IO_WIT`, `CLI_WIT`).
const IO_WIT: &str = "package wasi:io@0.2.0;\n\ninterface error {\n  resource error;\n}\n\ninterface streams {\n  use error.{error};\n  variant stream-error {\n    last-operation-failed(error),\n    closed,\n  }\n  resource output-stream {\n    blocking-write-and-flush: func(contents: list<u8>) -> result<_, stream-error>;\n  }\n}\n";
const CLI_WIT: &str = "package wasi:cli@0.2.0;\n\ninterface run {\n  run: func() -> result;\n}\n\ninterface stdout {\n  use wasi:io/streams@0.2.0.{output-stream};\n  get-stdout: func() -> output-stream;\n}\n";

/// `component::encode` for a port module: the WIT world embedded, then
/// wit-component (the part of the backend that stays in Rust).
pub fn wrap(mut core: Vec<u8>, wit: &str, world: WasmWorld) -> Vec<u8> {
    let mut resolve = wit_parser::Resolve::default();
    resolve.push_source("wasi-io.wit", IO_WIT).unwrap();
    resolve.push_source("wasi-cli.wit", CLI_WIT).unwrap();
    let pkg = resolve.push_source("vesper-program.wit", wit).unwrap();
    let name = match world {
        WasmWorld::Command => "program",
        WasmWorld::NexusPlugin => "plugin",
    };
    let id = resolve.select_world(&[pkg], Some(name)).unwrap();
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

/// `wasm_backend::compile`'s artifact for a module the port returned, as the
/// `wasm` hook hands it back. Part 1 lowers no handler and no host
/// capability, so the module uses no WasmFX and imports nothing.
pub fn artifact(
    w: &World,
    entry: &str,
    o: &WasmOptions,
    core: Vec<u8>,
    wit: String,
    functions: usize,
) -> WasmArtifact {
    let f = &w.functions[entry];
    let component = wrap(core.clone(), &wit, o.world);
    WasmArtifact {
        core,
        component,
        wit,
        world: o.world,
        export: match o.world {
            WasmWorld::Command => format!("{ENTRY_INTERFACE}#main"),
            WasmWorld::NexusPlugin => "run".to_owned(),
        },
        params: f.params.iter().map(|p| p.ty.clone()).collect(),
        result: vesper::numeric::default_type(&f.result),
        stack_switching: false,
        imports: vec![],
        handlers: vec![],
        functions,
    }
}

/// The port's answer for one compilation: `Some` artifact for a module,
/// `None` to leave the program to the Rust backend (an error, so the
/// diagnostics keep Rust's text, or a construct not ported yet).
pub fn compile_with(
    w: &World,
    entry: &str,
    o: &WasmOptions,
    port: impl FnOnce(&[u8]) -> Vec<u64>,
) -> Option<WasmArtifact> {
    if !w.functions.contains_key(entry) {
        return None;
    }
    let program = backend_program(w, o)?;
    let input = wasm_input(w, &program, entry, o)?;
    match outcome(&port(&input)) {
        Outcome::Module {
            core,
            wit,
            functions,
        } => Some(artifact(w, entry, o, core, wit, functions)),
        _ => None,
    }
}
