//! `f32`/`f64` by value through `.vsys` imports (§16.3, graphics gap 1):
//! direct calls mixing integer and float registers, a float callback type
//! called through a C pointer, and C calling a Vesper float function it
//! was handed. Floats cross in floating-point registers, bit for bit.
use vesper::ffi_lowering::{self, Handler, Imports};

/// `a * n + b`: a double, an int and a float, so the integer and float
/// argument registers are both in use.
extern "C" fn mix(a: f64, n: i32, b: f32) -> f64 {
    a * f64::from(n) + f64::from(b)
}
extern "C" fn half(x: f32) -> f32 {
    x / 2.0
}
/// Eight doubles and two more: the last two go on the stack (SysV and
/// AAPCS64 have eight float argument registers).
#[allow(clippy::too_many_arguments)]
extern "C" fn sum10(
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
    g: f64,
    h: f64,
    i: f64,
    j: f64,
) -> f64 {
    a + 2.0 * b + 3.0 * c + 4.0 * d + 5.0 * e + 6.0 * f + 7.0 * g + 8.0 * h + 9.0 * i + 10.0 * j
}
/// The address of `half`.
extern "C" fn halver() -> usize {
    half as *const () as usize
}
/// A midpoint-rule integral of `f` over `[a, b]`.
extern "C" fn integrate(f: extern "C" fn(f64) -> f64, a: f64, b: f64, steps: u32) -> f64 {
    let width = (b - a) / f64::from(steps);
    (0..steps)
        .map(|i| f(a + (f64::from(i) + 0.5) * width) * width)
        .sum()
}

fn target() -> (&'static str, &'static str) {
    if cfg!(all(target_os = "linux", target_arch = "aarch64")) {
        ("aarch64-unknown-linux-gnu", "aapcs64")
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        ("aarch64-apple-darwin", "darwin64")
    } else if cfg!(windows) {
        ("x86_64-pc-windows-msvc", "win64")
    } else {
        ("x86_64-unknown-linux-gnu", "sysv64")
    }
}

fn host_jit() -> bool {
    cfg!(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "aarch64"),
    ))
}

fn library() -> String {
    let (t, a) = target();
    format!(
        "sys library M (target = \"{t}\", abi = \"{a}\", link = \"dynamic\", name = \"m\", \
         artifact = \"blake3:{}\") {{
    callback Half(x: f32) -> ForeignVal[f32];
    callback Curve(x: f64) -> ForeignVal[f64];
    fn mix(a: f64, n: i32, b: f32) -> ForeignVal[f64];
    fn sum10(a: f64, b: f64, c: f64, d: f64, e: f64, f: f64, g: f64, h: f64, i: f64, j: f64) -> ForeignVal[f64];
    fn halver() -> ForeignVal[NonNull[Half]];
    fn integrate(nocapture f: Curve, a: f64, b: f64, steps: u32) -> ForeignVal[f64];
}}\n",
        "0".repeat(64)
    )
}

const SOURCE: &str = "\
fn square(x: f64) -> f64 { x * x }
pub fn mixed() -> u64 eff [Ffi[M]] { to_bits_f64(M::mix(1.5, 4, 0.25)) }
pub fn spilled() -> u64 eff [Ffi[M]] {
    to_bits_f64(M::sum10(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.5))
}
pub fn pointer() -> u32 eff [Ffi[M]] { to_bits_f32(M::Half(M::halver(), 3.0)) }
pub fn callback() -> u64 eff [Ffi[M]] { to_bits_f64(M::integrate(square, 0.0, 3.0, 4)) }
";

fn symbols() -> [(&'static str, *const u8); 4] {
    [
        ("mix", mix as *const u8),
        ("sum10", sum10 as *const u8),
        ("halver", halver as *const u8),
        ("integrate", integrate as *const u8),
    ]
}

#[test]
fn floats_cross_by_value() {
    if !host_jit() {
        return;
    }
    let mut imports = Imports::new();
    imports.add(&library(), Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap_or_else(|e| panic!("{e:#?}"));
    let midpoint = integrate(
        {
            extern "C" fn square(x: f64) -> f64 {
                x * x
            }
            square
        },
        0.0,
        3.0,
        4,
    );
    let expected = [
        ("mixed", mix(1.5, 4, 0.25).to_bits().to_string()),
        (
            "spilled",
            sum10(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 0.5)
                .to_bits()
                .to_string(),
        ),
        ("pointer", half(3.0).to_bits().to_string()),
        ("callback", midpoint.to_bits().to_string()),
    ];
    for optimize in [true, false] {
        for (entry, want) in &expected {
            let v = checked
                .jit_with_options(entry, &symbols(), optimize)
                .unwrap_or_else(|e| panic!("{entry}: {e:#?}"));
            assert_eq!(&v.to_string(), want, "{entry}, optimize {optimize}");
        }
    }
}

/// The evaluator's host calls C with integer words only, so it refuses a
/// float call with a reason rather than passing the bits in the wrong
/// registers.
#[test]
fn the_evaluator_refuses_float_calls() {
    let mut imports = Imports::new();
    imports.add(&library(), Handler::Direct).unwrap();
    let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap();
    let host = ffi_lowering::EvalHost::new(&imports, &symbols()).unwrap();
    let e = checked
        .evaluate("mixed", |call| host.call(call))
        .unwrap_err();
    assert!(format!("{e:?}").contains("float"), "{e:?}");
}

/// An object declares the import with float parameters, and objects of
/// every target build.
#[test]
fn objects_build_for_every_target() {
    for (t, a) in [
        ("x86_64-unknown-linux-gnu", "sysv64"),
        ("aarch64-unknown-linux-gnu", "aapcs64"),
        ("aarch64-apple-darwin", "darwin64"),
        ("x86_64-pc-windows-msvc", "win64"),
    ] {
        let vsys = library().replacen(target().0, t, 1).replacen(
            &format!("abi = \"{}\"", target().1),
            &format!("abi = \"{a}\""),
            1,
        );
        let mut imports = Imports::new();
        imports.add(&vsys, Handler::Direct).unwrap();
        let checked = ffi_lowering::check_source(SOURCE, &imports).unwrap();
        for entry in ["mixed", "spilled", "pointer", "callback"] {
            let object = checked
                .object(entry, Some(t))
                .unwrap_or_else(|e| panic!("{t} {entry}: {e:#?}"));
            assert!(!object.bytes.is_empty());
        }
    }
}
