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
use std::borrow::Cow;
use std::collections::BTreeSet;
use vesper::ast::{Effect, Effects, Expr, ExprKind, Function, Literal, Pass, Pattern, PatternKind};
use vesper::check::World;
use vesper::ir::{Op, Program};
use vesper::seq::SeqOp;
use vesper::wasm_backend::{
    ENTRY_INTERFACE, EffectLowering, HandlerLowering, HandlerMode, WasmArtifact, WasmOptions,
    WasmWorld,
};

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
// Place IR represents both element views as TryGet. Mark the mutable receiver
// in the Wasm input so the port can reject it when the function is reachable.
// If a source view cannot be matched to an instruction, use the Rust fallback.
fn wasm_element_views<'a>(w: &World, program: &'a Program) -> Option<Cow<'a, Program>> {
    let mut unmatched: BTreeSet<_> = w
        .element_views
        .iter()
        .filter_map(|(span, pass)| (*pass == Pass::Inout).then_some(*span))
        .collect();
    if unmatched.is_empty() {
        return Some(Cow::Borrowed(program));
    }
    let mut marked = program.clone();
    for i in marked
        .functions
        .values_mut()
        .flat_map(|f| &mut f.blocks)
        .flat_map(|b| &mut b.instructions)
    {
        let span = (i.span.start, i.span.end);
        if w.element_views.get(&span) == Some(&Pass::Inout)
            && let Op::Seq {
                op: SeqOp::TryGet,
                args,
            } = &mut i.op
            && let Some(receiver) = args.first_mut()
        {
            receiver.pass = Pass::Inout;
            unmatched.remove(&span);
        }
    }
    unmatched.is_empty().then_some(Cow::Owned(marked))
}

pub fn wasm_input(w: &World, program: &Program, entry: &str, o: &WasmOptions) -> Option<Vec<u8>> {
    if !ir_words::encodable(w, program) {
        return None;
    }
    let program = wasm_element_views(w, program)?;
    let mut t = Tables::of(&program);
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
        /// The plan's handlers, in plan order.
        handlers: Vec<PortHandler>,
    },
    /// An error class as `error_class` spells it, and its site.
    Error(String, u64),
    /// A construct this port does not lower yet (handlers, host frames).
    NotPorted(u64),
}

/// One handler the port lowered: its site instance, its effect and each
/// clause's operation index and mode (0 Abort, 1 Tail).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortHandler {
    pub site: String,
    pub effect: String,
    pub clauses: Vec<(usize, u64)>,
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
        let (wit, more) = unpack_bytes(&words[2 + used..]);
        let mut at = 2 + used + more;
        let mut handlers = vec![];
        for _ in 0..words[at] {
            at += 1;
            let (site, n) = unpack_bytes(&words[at..]);
            at += n;
            let (effect, n) = unpack_bytes(&words[at..]);
            at += n;
            let clauses = (0..words[at] as usize)
                .map(|k| (words[at + 1 + 2 * k] as usize, words[at + 2 + 2 * k]))
                .collect::<Vec<_>>();
            at += 2 * clauses.len();
            handlers.push(PortHandler {
                site: String::from_utf8(site).unwrap(),
                effect: String::from_utf8(effect).unwrap(),
                clauses,
            });
        }
        return Outcome::Module {
            core,
            wit: String::from_utf8(wit).unwrap(),
            functions,
            handlers,
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
const CLI_WIT: &str = "package wasi:cli@0.2.0;\n\ninterface run {\n  run: func() -> result;\n}\n\ninterface stdout {\n  use wasi:io/streams@0.2.0.{output-stream};\n  get-stdout: func() -> output-stream;\n}\n\ninterface environment {\n  get-environment: func() -> list<tuple<string, string>>;\n  get-arguments: func() -> list<string>;\n  initial-cwd: func() -> option<string>;\n}\n";
/// The WASI 0.2 interfaces the standard I/O libraries lower to
/// (packages/vesper-wasm-backend/src/host.ves): every type as WASI defines
/// it, and only the functions the shims import.
const CLOCKS_WIT: &str = r#"package wasi:clocks@0.2.0;

interface wall-clock {
  record datetime {
    seconds: u64,
    nanoseconds: u32,
  }
}
"#;
/// The Cranelift component's interface (crates/vesper-cranelift-component,
/// #2210), which a program calling `Ffi[Cranelift]` imports.
const CRANELIFT_WIT: &str = include_str!("../../crates/vesper-cranelift-component/wit/vcl.wit");

const FILESYSTEM_WIT: &str = r#"package wasi:filesystem@0.2.0;

interface types {
  use wasi:clocks/wall-clock@0.2.0.{datetime};
  type filesize = u64;
  enum descriptor-type {
    unknown,
    block-device,
    character-device,
    directory,
    fifo,
    symbolic-link,
    regular-file,
    socket,
  }
  flags descriptor-flags {
    read,
    write,
    file-integrity-sync,
    data-integrity-sync,
    requested-write-sync,
    mutate-directory,
  }
  flags path-flags {
    symlink-follow,
  }
  flags open-flags {
    create,
    directory,
    exclusive,
    truncate,
  }
  type link-count = u64;
  record descriptor-stat {
    %type: descriptor-type,
    link-count: link-count,
    size: filesize,
    data-access-timestamp: option<datetime>,
    data-modification-timestamp: option<datetime>,
    status-change-timestamp: option<datetime>,
  }
  record directory-entry {
    %type: descriptor-type,
    name: string,
  }
  enum error-code {
    access,
    would-block,
    already,
    bad-descriptor,
    busy,
    deadlock,
    quota,
    exist,
    file-too-large,
    illegal-byte-sequence,
    in-progress,
    interrupted,
    invalid,
    io,
    is-directory,
    loop,
    too-many-links,
    message-size,
    name-too-long,
    no-device,
    no-entry,
    no-lock,
    insufficient-memory,
    insufficient-space,
    not-directory,
    not-empty,
    not-recoverable,
    unsupported,
    no-tty,
    no-such-device,
    overflow,
    not-permitted,
    pipe,
    read-only,
    invalid-seek,
    text-file-busy,
    cross-device,
  }
  resource descriptor {
    read: func(length: filesize, offset: filesize) -> result<tuple<list<u8>, bool>, error-code>;
    write: func(buffer: list<u8>, offset: filesize) -> result<filesize, error-code>;
    read-directory: func() -> result<directory-entry-stream, error-code>;
    sync: func() -> result<_, error-code>;
    create-directory-at: func(path: string) -> result<_, error-code>;
    stat: func() -> result<descriptor-stat, error-code>;
    stat-at: func(path-flags: path-flags, path: string) -> result<descriptor-stat, error-code>;
    open-at: func(path-flags: path-flags, path: string, open-flags: open-flags, %flags: descriptor-flags) -> result<descriptor, error-code>;
    remove-directory-at: func(path: string) -> result<_, error-code>;
    rename-at: func(old-path: string, new-descriptor: borrow<descriptor>, new-path: string) -> result<_, error-code>;
    unlink-file-at: func(path: string) -> result<_, error-code>;
  }
  resource directory-entry-stream {
    read-directory-entry: func() -> result<option<directory-entry>, error-code>;
  }
}

interface preopens {
  use types.{descriptor};
  get-directories: func() -> list<tuple<descriptor, string>>;
}
"#;

/// `component::encode` for a port module: the WIT world embedded, then
/// wit-component (the part of the backend that stays in Rust).
pub fn wrap(mut core: Vec<u8>, wit: &str, world: WasmWorld) -> Vec<u8> {
    let mut resolve = wit_parser::Resolve::default();
    resolve.push_source("wasi-io.wit", IO_WIT).unwrap();
    resolve.push_source("wasi-cli.wit", CLI_WIT).unwrap();
    resolve.push_source("wasi-clocks.wit", CLOCKS_WIT).unwrap();
    resolve
        .push_source("wasi-filesystem.wit", FILESYSTEM_WIT)
        .unwrap();
    // `Ffi[Cranelift]` imports (#2210).
    resolve
        .push_source("vesper-cranelift.wit", CRANELIFT_WIT)
        .unwrap();
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

/// The artifact's `handlers` for the port's report: operation names from
/// the world, as `wasm_backend::compile` writes them.
pub fn handler_lowerings(w: &World, handlers: &[PortHandler]) -> Vec<HandlerLowering> {
    handlers
        .iter()
        .map(|h| HandlerLowering {
            effect: h.effect.clone(),
            function: h.site.clone(),
            clauses: h
                .clauses
                .iter()
                .map(|&(op, mode)| {
                    (
                        w.effects[&h.effect].ops[op].name.clone(),
                        if mode == 0 {
                            HandlerMode::Abort
                        } else {
                            HandlerMode::Tail
                        },
                    )
                })
                .collect(),
        })
        .collect()
}

/// `wasm_backend::compile`'s artifact for a module the port returned, as the
/// `wasm` hook hands it back. The port lowers no host capability and no
/// WasmFX, so the module imports nothing.
pub fn artifact(
    w: &World,
    entry: &str,
    o: &WasmOptions,
    core: Vec<u8>,
    wit: String,
    functions: usize,
    handlers: &[PortHandler],
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
        handlers: handler_lowerings(w, handlers),
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
            handlers,
        } => Some(artifact(w, entry, o, core, wit, functions, &handlers)),
        _ => None,
    }
}
