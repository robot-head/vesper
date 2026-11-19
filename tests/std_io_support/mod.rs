//! Shared harness of tests/std_fs.rs, std_process.rs and std_env.rs
//! (self-hosting SH0 lane 3, #1125): a driver package over
//! `@vesper-std/fs`, `process` and `env`, checked against their three
//! `.vsys` imports, and run
//!
//! * in the JIT with the native optimizer on and off, the imports bound to
//!   the real host shim (`vesper_std_host::symbols`), and
//! * in the evaluator, through [`eval_host`]: a generic bridge that
//!   marshals each foreign call's source values into the `.vsys` C ABI
//!   (driven by the parsed library descriptors) and calls the same host
//!   functions, so both paths touch the real file system.
//!
//! Each run must leave no live host handle behind (the leak check).
#![allow(dead_code)]

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{
        Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use vesper::ffi_lowering::{self, EvalCall, ForeignChecked, Handler, Imports};
use vesper::runtime::Value;
use vesper_ffi::sys::{self, LibraryDescriptor, ParameterType, Pointee, Scalar, ValueType};

/// The packages and their `.vsys` libraries.
pub const LIBRARIES: [&str; 3] = ["fs", "process", "env"];

/// JIT and evaluator runs share the process-wide handle counters, so they
/// run one at a time.
pub static SERIAL: Mutex<()> = Mutex::new(());

/// The std packages: tests run from the repository root (under Bazel too,
/// where the root package's files are the test's data).
pub fn std_dir() -> PathBuf {
    fs::canonicalize("packages/vesper-std").expect("run from the repository root")
}

/// The host's `.vsys` target and calling convention. Any value checks; the
/// JIT needs the real one.
pub fn host_target() -> (&'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", "aapcs64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "darwin64")
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        ("x86_64-pc-windows-msvc", "win64")
    } else {
        ("x86_64-unknown-linux-gnu", "sysv64")
    }
}

/// Whether this host has a `.vsys` target the JIT can call.
pub fn jit_host() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64"),
    ))
}

/// The `.vsys` source of package `lib` for this host.
pub fn vsys(lib: &str) -> String {
    let (t, a) = host_target();
    fs::read_to_string(std_dir().join(lib).join(format!("{lib}.vsys")))
        .unwrap()
        .replace("@TARGET@", t)
        .replace("@ABI@", a)
}

pub fn imports() -> Imports {
    let mut imports = Imports::new();
    for lib in LIBRARIES {
        imports.add(&vsys(lib), Handler::Direct).unwrap();
    }
    imports
}

static NEXT: AtomicU64 = AtomicU64::new(0);

/// A temporary directory, removed on drop.
pub struct Temp(pub PathBuf);
impl Temp {
    pub fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "vesper-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }
    pub fn path(&self) -> &Path {
        &self.0
    }
    /// The directory as a Vesper string literal's contents.
    pub fn lit(&self) -> String {
        lit(&self.0)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// `path` with `/` separators, escaped for a Vesper string literal.
pub fn lit(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "/")
        .replace('"', "\\\"")
}

/// A driver package `@local/std-io-driver` whose `src/main.ves` is
/// `source`, depending on the three std I/O packages by absolute path.
pub fn driver(source: &str) -> Temp {
    let temp = Temp::new("std-io-driver");
    let std = std_dir();
    let mut deps = String::new();
    for name in LIBRARIES {
        deps.push_str(&format!(
            "\"@vesper-std/{name}\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
            lit(&std.join(name))
        ));
    }
    let manifest = format!(
        "[package]\nname = \"@local/std-io-driver\"\nversion = \"0.1.0\"\nedition = \"2026\"\n\
         capability_ceiling = [\"Fail\", \"Alloc\", \"Ffi[StdFs]\", \"Ffi[StdProcess]\", \"Ffi[StdEnv]\"]\n\n\
         [dependencies]\n{deps}"
    );
    fs::write(temp.0.join("Loom.toml"), manifest).unwrap();
    fs::create_dir_all(temp.0.join("src")).unwrap();
    fs::write(temp.0.join("src/main.ves"), source).unwrap();
    temp
}

/// Load and check `dir` against the three imports.
pub fn check(dir: &Path) -> ForeignChecked {
    let assembled = vesper::packages::load(dir).unwrap();
    assert!(
        assembled.ok(),
        "{:?} {:?}",
        assembled.diagnostics,
        assembled.manifest_errors
    );
    ffi_lowering::check_module(&assembled.module, &imports()).unwrap_or_else(|e| {
        let shown: Vec<_> = e
            .diagnostics
            .iter()
            .map(|d| assembled.sources.localize(d))
            .collect();
        panic!("{e}: {shown:?}")
    })
}

/// Check `source` as a driver package.
pub fn check_driver(source: &str) -> ForeignChecked {
    let pkg = driver(source);
    check(pkg.path())
}

/// One way of running an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Jit,
    JitNoOpt,
    Evaluator,
}
pub const MODES: [Mode; 3] = [Mode::Jit, Mode::JitNoOpt, Mode::Evaluator];

/// Run `entry` in `mode`; panics with the error. Checks that every host
/// handle the run created was finalized.
pub fn run(checked: &ForeignChecked, entry: &str, mode: Mode) -> Value {
    let before = vesper_std_host::live_handles();
    let got = match mode {
        Mode::Jit | Mode::JitNoOpt => checked
            .jit_with_options(entry, &vesper_std_host::symbols(), mode == Mode::Jit)
            .unwrap_or_else(|e| panic!("{entry} ({mode:?}): {e}")),
        Mode::Evaluator => {
            let host = EvalHost::new();
            checked
                .evaluate(entry, |call| host.call(call))
                .unwrap_or_else(|e| panic!("{entry} ({mode:?}): {e}"))
        }
    };
    assert_eq!(
        vesper_std_host::live_handles(),
        before,
        "{entry} ({mode:?}): live handles [file, dir, command, child]"
    );
    got
}

/// The u64 an entry returned.
pub fn word(v: &Value) -> u64 {
    match v {
        Value::Int(n) => *n as u64,
        Value::UInt(n, _) => *n,
        Value::SInt(n, _) => *n as u64,
        other => panic!("not a word: {other:?}"),
    }
}

/// Run `entry` in every mode the host supports and require one answer.
pub fn agree(checked: &ForeignChecked, entry: &str, modes: &[Mode]) -> u64 {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let mut answer = None;
    for &mode in modes {
        if mode != Mode::Evaluator && !jit_host() {
            continue;
        }
        let got = word(&run(checked, entry, mode));
        match answer {
            None => answer = Some((mode, got)),
            Some((first, want)) => assert_eq!(got, want, "{entry}: {mode:?} vs {first:?}"),
        }
    }
    answer.expect("no mode ran").1
}

// ---------------------------------------------------------------------------
// The evaluator's host: source values -> the `.vsys` C ABI -> the real host.

/// A foreign function: its descriptor and its host address.
struct Foreign {
    descriptor: sys::FunctionDescriptor,
    address: usize,
}

pub struct EvalHost {
    functions: BTreeMap<String, Foreign>,
}

fn int(v: &Value) -> Result<u64, String> {
    match v {
        Value::Int(n) => Ok(*n as u64),
        Value::UInt(n, _) => Ok(*n),
        Value::SInt(n, _) => Ok(*n as u64),
        Value::Bool(b) => Ok(u64::from(*b)),
        other => Err(format!("not a scalar: {other:?}")),
    }
}

fn scalar_value(s: Scalar, raw: u64) -> Value {
    match s {
        Scalar::U8 => Value::UInt(raw & 0xff, 8),
        Scalar::U16 => Value::UInt(raw & 0xffff, 16),
        Scalar::U32 => Value::UInt(raw & 0xffff_ffff, 32),
        Scalar::U64 | Scalar::Usize => Value::UInt(raw, 64),
        Scalar::I64 => Value::Int(raw as i64),
        Scalar::I32 => Value::Int(i64::from(raw as u32 as i32)),
        Scalar::I16 => Value::Int(i64::from(raw as u16 as i16)),
        Scalar::I8 => Value::Int(i64::from(raw as u8 as i8)),
    }
}

fn data(ty: &str, constructor: &str, fields: Vec<(&str, Value)>) -> Value {
    Value::Data {
        ty: ty.into(),
        constructor: constructor.into(),
        fields: fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect(),
        resource_identity: None,
    }
}

fn field(v: &Value, name: &str) -> Result<u64, String> {
    match v {
        Value::Data { fields, .. } => int(fields.get(name).ok_or(format!("no field {name}"))?),
        other => Err(format!("not a struct: {other:?}")),
    }
}

/// The wire structs of the three libraries: `(name, [(field, offset,
/// scalar)], size)`. Their layouts have no padding (the `.vsys` checks).
/// One field of a wire struct: name, byte offset, scalar.
type WireField = (&'static str, usize, Scalar);

const WIRE: [(&str, &[WireField], usize); 2] = [
    (
        "FsMeta",
        &[
            ("size", 0, Scalar::U64),
            ("mtime_s", 8, Scalar::I64),
            ("mtime_ns", 16, Scalar::U32),
            ("kind", 20, Scalar::U32),
        ],
        24,
    ),
    (
        "ProcStatus",
        &[
            ("code", 0, Scalar::I64),
            ("signal", 8, Scalar::U32),
            ("exited", 12, Scalar::U32),
        ],
        16,
    ),
];

/// One marshalled argument kept alive across the call.
enum Slot {
    None,
    Bytes(Vec<u8>),
    Word(Box<u64>),
    Wire(&'static str, Vec<u64>),
}

impl EvalHost {
    pub fn new() -> Self {
        let addresses: BTreeMap<&str, usize> = vesper_std_host::symbols()
            .into_iter()
            .map(|(n, a)| (n, a as usize))
            .collect();
        let mut functions = BTreeMap::new();
        for lib in LIBRARIES {
            let d: LibraryDescriptor = sys::parse(&vsys(lib), Default::default()).unwrap();
            for f in d.functions() {
                functions.insert(
                    f.symbol().to_owned(),
                    Foreign {
                        descriptor: f.clone(),
                        address: addresses[f.symbol()],
                    },
                );
            }
        }
        Self { functions }
    }

    pub fn call(&self, call: &mut EvalCall<'_, '_>) -> Result<Value, String> {
        let f = self
            .functions
            .get(&call.symbol)
            .ok_or_else(|| format!("unknown symbol {}", call.symbol))?;
        let args = call.arguments().to_vec();
        let params = f.descriptor.parameters();
        if args.len() != params.len() {
            return Err(format!("{}: {} arguments", call.symbol, args.len()));
        }
        let mut slots: Vec<Slot> = vec![];
        let mut words: Vec<u64> = vec![];
        for (p, v) in params.iter().zip(&args) {
            match p.ty() {
                ParameterType::Value(_) => {
                    words.push(int(v)?);
                    slots.push(Slot::None);
                }
                ParameterType::Handle { .. } => {
                    words.push(ffi_lowering::handle_word(v).ok_or("not a handle")?);
                    slots.push(Slot::None);
                }
                ParameterType::Slice { .. } => {
                    let bytes = ffi_lowering::slice_bytes(v).ok_or("not a byte slice")?;
                    slots.push(Slot::Bytes(bytes));
                    words.push(0);
                    words.push(0);
                }
                ParameterType::Pointer {
                    pointee: Pointee::Scalar(_),
                    ..
                } => {
                    slots.push(Slot::Word(Box::new(int(v)?)));
                    words.push(0);
                }
                ParameterType::Pointer {
                    pointee: Pointee::Struct(w),
                    ..
                } => {
                    let (name, fields, size) = WIRE
                        .iter()
                        .find(|(n, _, _)| *n == w.name())
                        .ok_or_else(|| format!("no layout for {}", w.name()))?;
                    let mut buf = vec![0u64; size.div_ceil(8)];
                    let bytes = bytemuck_u64(&mut buf);
                    for (fname, off, s) in fields.iter() {
                        let x = field(v, fname)?;
                        let width = usize::from(s.bits() / 8);
                        bytes[*off..off + width].copy_from_slice(&x.to_le_bytes()[..width]);
                    }
                    slots.push(Slot::Wire(name, buf));
                    words.push(0);
                }
            }
        }
        // Fill in the addresses now that every buffer is in place.
        let mut w = 0;
        for (p, slot) in params.iter().zip(slots.iter_mut()) {
            match (p.ty(), slot) {
                (ParameterType::Slice { .. }, Slot::Bytes(b)) => {
                    words[w] = b.as_mut_ptr() as u64;
                    words[w + 1] = b.len() as u64;
                    w += 2;
                }
                (_, Slot::Word(b)) => {
                    words[w] = &mut **b as *mut u64 as u64;
                    w += 1;
                }
                (_, Slot::Wire(_, b)) => {
                    words[w] = b.as_mut_ptr() as u64;
                    w += 1;
                }
                _ => w += 1,
            }
        }
        // SAFETY: `address` is the host function of this `.vsys` signature;
        // every parameter is one integer-class word in the C ABI, so a call
        // through a word-only signature of the same arity is the same call.
        let raw = unsafe { call_words(f.address, &words)? };
        // Copy writes back into the caller's values.
        for (i, (p, slot)) in params.iter().zip(&slots).enumerate() {
            match (p.ty(), slot) {
                (
                    ParameterType::Slice {
                        access: sys::Access::Inout,
                        ..
                    },
                    Slot::Bytes(b),
                ) => ffi_lowering::write_slice_bytes(&args[i], b)?,
                (
                    ParameterType::Pointer {
                        pointee: Pointee::Scalar(s),
                        access,
                        ..
                    },
                    Slot::Word(b),
                ) if access.writes() => {
                    let mask = if s.bits() == 64 {
                        u64::MAX
                    } else {
                        (1u64 << s.bits()) - 1
                    };
                    call.write_argument(i, scalar_value(*s, **b & mask))?;
                }
                (ParameterType::Pointer { access, .. }, Slot::Wire(name, b)) if access.writes() => {
                    let (_, fields, _) = WIRE.iter().find(|(n, _, _)| n == name).unwrap();
                    let mut b = b.clone();
                    let bytes = bytemuck_u64(&mut b);
                    let mut out = vec![];
                    for (fname, off, s) in fields.iter() {
                        let width = usize::from(s.bits() / 8);
                        let mut le = [0u8; 8];
                        le[..width].copy_from_slice(&bytes[*off..off + width]);
                        out.push((*fname, scalar_value(*s, u64::from_le_bytes(le))));
                    }
                    call.write_argument(i, data(name, name, out))?;
                }
                _ => {}
            }
        }
        Ok(match f.descriptor.result() {
            ValueType::Unit => Value::Tuple(vec![]),
            ValueType::Scalar(s) => scalar_value(*s, raw),
            ValueType::Bool => Value::Bool(raw & 0xff != 0),
            ValueType::Handle { handle, nullable } => {
                let name = handle.name();
                match (*nullable, raw) {
                    (true, 0) => data("Option", "None", vec![]),
                    (true, h) => data(
                        "Option",
                        "Some",
                        vec![("0", ffi_lowering::handle_value(name, h))],
                    ),
                    (false, 0) => data(
                        "Result",
                        "Err",
                        vec![(
                            "0",
                            data(
                                "ForeignInvalid",
                                "ForeignInvalid",
                                vec![("raw", Value::UInt(0, 64))],
                            ),
                        )],
                    ),
                    (false, h) => data(
                        "Result",
                        "Ok",
                        vec![("0", ffi_lowering::handle_value(name, h))],
                    ),
                }
            }
            other => return Err(format!("{}: unsupported result {other:?}", call.symbol)),
        })
    }
}

fn bytemuck_u64(words: &mut [u64]) -> &mut [u8] {
    // SAFETY: u64 has no padding and any byte pattern is valid; the view
    // covers exactly the words' bytes.
    unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), words.len() * 8) }
}

/// Call the C function at `address` with `words` as its integer arguments.
unsafe fn call_words(address: usize, w: &[u64]) -> Result<u64, String> {
    type F0 = extern "C" fn() -> u64;
    type F1 = extern "C" fn(u64) -> u64;
    type F2 = extern "C" fn(u64, u64) -> u64;
    type F3 = extern "C" fn(u64, u64, u64) -> u64;
    type F4 = extern "C" fn(u64, u64, u64, u64) -> u64;
    type F5 = extern "C" fn(u64, u64, u64, u64, u64) -> u64;
    type F6 = extern "C" fn(u64, u64, u64, u64, u64, u64) -> u64;
    // SAFETY: the caller's contract (see the call site).
    unsafe {
        Ok(match w.len() {
            0 => std::mem::transmute::<usize, F0>(address)(),
            1 => std::mem::transmute::<usize, F1>(address)(w[0]),
            2 => std::mem::transmute::<usize, F2>(address)(w[0], w[1]),
            3 => std::mem::transmute::<usize, F3>(address)(w[0], w[1], w[2]),
            4 => std::mem::transmute::<usize, F4>(address)(w[0], w[1], w[2], w[3]),
            5 => std::mem::transmute::<usize, F5>(address)(w[0], w[1], w[2], w[3], w[4]),
            6 => std::mem::transmute::<usize, F6>(address)(w[0], w[1], w[2], w[3], w[4], w[5]),
            n => return Err(format!("{n} argument words")),
        })
    }
}
