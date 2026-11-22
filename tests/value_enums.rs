//! Value enums `enum Name: T { A = e, B, .. }` and importable constants
//! (manuscript §2.2 and §11.3, claim #1234): members are module constants
//! of the representation type, literal constants are match patterns, and
//! `pub const` / `pub enum` items are imported by `use`, re-exported by
//! `pub use` and named by qualified paths, across modules and packages.
use std::{
    path::{Path, PathBuf},
    process::Command,
};
use vesper::{check, packages, parser, runtime::Runtime};

fn cli() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vesper"))
}

fn run(source: &str) -> String {
    let p = parser::parse(source);
    assert!(p.diagnostics.is_empty(), "{source}\n{:?}", p.diagnostics);
    let c = check::check(&p.module);
    assert!(c.diagnostics.is_empty(), "{source}\n{:?}", c.diagnostics);
    Runtime::new(&c.world)
        .run("main", vec![])
        .unwrap()
        .to_string()
}

fn package(tag: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vesper-value-enums-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
    dir
}

fn manifest(name: &str, deps: &str) -> String {
    format!(
        "[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2026\"\ncapability_ceiling = []\n{deps}"
    )
}

fn assembled_run(dir: &Path) -> String {
    let assembled = packages::load(dir).unwrap();
    assert!(assembled.ok(), "{:?}", assembled.diagnostics);
    let checked = check::check(&assembled.module);
    assert!(checked.diagnostics.is_empty(), "{:?}", checked.diagnostics);
    Runtime::new(&checked.world)
        .run("main", vec![])
        .unwrap()
        .to_string()
}

fn package_codes(dir: &Path) -> Vec<String> {
    let assembled = packages::load(dir).unwrap();
    let mut codes: Vec<String> = assembled
        .diagnostics
        .iter()
        .map(|d| d.diagnostic.code.clone())
        .collect();
    if codes.is_empty() {
        codes.extend(
            check::check(&assembled.module)
                .diagnostics
                .iter()
                .map(|d| d.code.clone()),
        );
    }
    codes
}

const CLASSES: &str = "enum Class: u8 { Ld, Ldx, St = 2, Alu = 4, Jmp, Mask = 7 }
enum Kind: u64 { Void, Int, Ptr = Kind::Int + 5, Array, Both = Kind::Ptr | Kind::Array, Shifted = 1 << 40 }
enum Delta: i32 { Down = -1, Same, Up }
";

#[test]
fn members_number_from_zero_and_fold_earlier_members() {
    let source = format!(
        "{CLASSES}fn main() -> (u8, u8, u64, u64, u64, i32, i32) {{
    (Class::Ldx, Class::Jmp, Kind::Array, Kind::Both, Kind::Shifted, Delta::Same, Delta::Up)
}}"
    );
    assert_eq!(run(&source), "(1, 5, 7, 7, 1099511627776, 0, 1)");
    // Folded members are literals, so the parser leaves no arithmetic.
    let p = parser::parse(&source);
    let array = p
        .module
        .items
        .iter()
        .find_map(|i| match i {
            vesper::ast::Item::Function(f) if f.name == "const$Kind::Array" => Some(f),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        parser::const_literal(array),
        Some(vesper::ast::Literal::Int(7))
    );
}

#[test]
fn members_and_literal_constants_are_match_patterns() {
    let source = format!(
        "{CLASSES}const LIMIT: u8 = 3;
const NEG: i32 = -1;
fn name(c: u8) -> u64 {{
    match c & Class::Mask {{
        Class::Ld => 10,
        Class::Ldx | Class::St => 11,
        LIMIT => 13,
        Class::Alu => 14,
        _ => 0,
    }}
}}
fn sign(d: i32) -> u64 {{
    match (d, 0) {{ (NEG, _) => 1, (Delta::Up, _) => 2, _ => 3 }}
}}
fn main() -> (u64, u64, u64, u64, u64, u64, u64) {{
    (name(8), name(2), name(3), name(12), name(6), sign(-1), sign(1))
}}"
    );
    assert_eq!(run(&source), "(10, 11, 13, 14, 0, 1, 2)");
}

#[test]
fn value_enum_syntax_errors() {
    for source in [
        "enum E[T]: u8 { A }",
        "enum E: u8 { A(u8) }",
        "enum E: u8 { A = }",
    ] {
        assert!(
            !parser::parse(source).diagnostics.is_empty(),
            "{source} should not parse"
        );
    }
    // An empty value enum declares nothing.
    assert!(
        parser::parse("enum E: u8 {} fn main() -> u8 { 0 }")
            .diagnostics
            .is_empty()
    );
}

#[test]
fn a_value_enum_may_not_share_a_type_name() {
    let p = parser::parse("enum Size { W, H } enum Size: u8 { W = 0 } fn main() -> u8 { 0 }");
    assert!(p.diagnostics.is_empty());
    let c = check::check(&p.module);
    assert!(
        c.diagnostics.iter().any(|d| d.code == "E1542"),
        "{:?}",
        c.diagnostics
    );
}

#[test]
fn members_are_checked_against_the_representation() {
    let p = parser::parse("enum E: u8 { A = 255, B } fn main() -> u8 { E::B }");
    assert!(p.diagnostics.is_empty());
    assert!(!check::check(&p.module).diagnostics.is_empty());
}

const COMMON: &str = "pub enum Class: u8 { Ld, Ldx, Alu = 4, Mask = 7 }
enum Hidden: u8 { A = 1 }
pub const LIMIT: u64 = 40;
pub const TABLE: [u8; 3] = [1, 2, 3];
const PRIVATE: u64 = 9;
";

#[test]
fn constants_and_value_enums_are_imported_across_modules() {
    let dir = package(
        "modules",
        &[
            ("Loom.toml", &manifest("@demo/consts", "")),
            ("src/common.ves", COMMON),
            (
                "src/insn.ves",
                "use self::common::{Class, LIMIT, TABLE};
pub use self::common::Class as Op;
pub use self::common::LIMIT as MAX;
pub fn class_name(c: u8) -> u64 {
    match c & Class::Mask {
        Class::Ld => 1,
        Class::Alu => LIMIT,
        _ => widen_u64(TABLE[2]),
    }
}
pub fn direct() -> u8 { self::common::Class::Ldx }
",
            ),
            (
                "src/lib.ves",
                "pub use self::common::TABLE as T;
use self::insn::{class_name, direct, Op, MAX};
use self::common::Class as C;
fn main() -> (u64, u64, u64, u64, u8) {
    match direct() {
        C::Ldx => (class_name(12), class_name(8), class_name(3), MAX ^ widen_u64(T[0]), Op::Alu),
        _ => (0, 0, 0, 0, 0),
    }
}
",
            ),
        ],
    );
    assert_eq!(assembled_run(&dir), "(40, 1, 3, 41, 4)");
    // The same program through native code.
    let jit = cli().arg("jit").arg(&dir).output().unwrap();
    assert!(
        jit.status.success(),
        "{}",
        String::from_utf8_lossy(&jit.stderr)
    );
}

#[test]
fn private_constants_and_value_enums_stay_private() {
    for (lib, code) in [
        (
            "use self::common::Hidden;\nfn main() -> u8 { Hidden::A }\n",
            "E0112",
        ),
        (
            "use self::common::PRIVATE;\nfn main() -> u64 { PRIVATE }\n",
            "E0112",
        ),
        (
            "use self::common::Missing;\nfn main() -> u64 { 0 }\n",
            "E0111",
        ),
        ("fn main() -> u8 { self::common::Class::Nope }\n", "E0111"),
    ] {
        let dir = package(
            "private",
            &[
                ("Loom.toml", &manifest("@demo/consts", "")),
                ("src/common.ves", COMMON),
                ("src/lib.ves", lib),
            ],
        );
        let codes = package_codes(&dir);
        assert!(codes.iter().any(|c| c == code), "{lib}: {codes:?}");
    }
}

#[test]
fn constants_and_value_enums_are_imported_from_dependencies() {
    let dep = package(
        "dep",
        &[
            ("Loom.toml", &manifest("@demo/consts", "")),
            ("src/lib.ves", "pub fn unused() -> u64 { 0 }\n"),
            ("src/common.ves", COMMON),
        ],
    );
    let app = package(
        "app",
        &[
            (
                "Loom.toml",
                &manifest(
                    "@demo/app",
                    &format!(
                        "\n[dependencies]\n\"@demo/consts\" = {{ version = \"^0.1\", path = \"{}\" }}\n",
                        dep.display()
                    ),
                ),
            ),
            (
                "src/lib.ves",
                "use @demo/consts::common::{Class, LIMIT};
fn main() -> u64 {
    match @demo/consts::common::Class::Alu {
        Class::Alu => LIMIT,
        @demo/consts::common::Class::Ld => 1,
        _ => 2,
    }
}
",
            ),
        ],
    );
    assert_eq!(assembled_run(&app), "40");
}

#[test]
fn untyped_members_take_the_type_of_their_use() {
    let source = "enum Helper: _ { Trace = 6, Map = 1, Next, Big = 1 << 40 }
fn id32(x: i32) -> i32 { x }
fn wide(x: u64) -> u64 { x }
fn named(x: u8) -> u64 {
    match x {
        Helper::Map => 1,
        Helper::Next => 2,
        _ => 0,
    }
}
fn main() -> (i32, u64, u64, u64, u64) {
    (id32(Helper::Trace), wide(Helper::Next), named(2), wide(Helper::Big) & 1099511627777, wide(Helper::Map))
}
";
    assert_eq!(run(source), "(6, 2, 2, 1099511627776, 1)");
}

#[test]
fn untyped_members_must_be_literals() {
    let p = parser::parse("fn f() -> u64 { 3 } enum E: _ { A = f() } fn main() -> u64 { E::A }");
    assert!(p.diagnostics.is_empty(), "{:?}", p.diagnostics);
    let codes: Vec<String> = check::check(&p.module)
        .diagnostics
        .iter()
        .map(|d| d.code.clone())
        .collect();
    assert!(codes.contains(&"E1541".to_string()), "{codes:?}");
}

#[test]
fn untyped_members_are_shared_across_modules() {
    let dir = package(
        "untyped",
        &[
            ("Loom.toml", &manifest("@demo/untyped", "")),
            (
                "src/ids.ves",
                "pub enum Helper: _ { Lookup = 1, Update, Trace = 6 }\n",
            ),
            (
                "src/helpers.ves",
                "use self::ids::Helper;
pub fn small(id: i32) -> i32 { if id == Helper::Trace { 1 } else { 0 } }
",
            ),
            (
                "src/lib.ves",
                "use self::ids::Helper;
use self::helpers::small;
fn kind(id: u64) -> u64 {
    match id {
        Helper::Update => 20,
        _ => 0,
    }
}
fn main() -> (i32, u64, u64) {
    (small(Helper::Trace), kind(Helper::Update), self::ids::Helper::Lookup)
}
",
            ),
        ],
    );
    assert_eq!(assembled_run(&dir), "(1, 20, 1)");
    let jit = cli().arg("jit").arg(&dir).output().unwrap();
    assert!(
        jit.status.success(),
        "{}",
        String::from_utf8_lossy(&jit.stderr)
    );
}

#[test]
fn single_item_parsers_keep_every_member() {
    let source = "enum E: u8 { A, B, C = 7 }\nfn f() -> u8 { E::B }\n";
    let syntax = parser::parse_recovery(source, Default::default()).unwrap();
    let module = parser::parse_complete_items(&syntax, &[0], Default::default()).unwrap();
    let names: Vec<String> = module
        .items
        .iter()
        .filter_map(|i| match i {
            vesper::ast::Item::Function(f) => Some(f.name.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(names, ["const$E::A", "const$E::B", "const$E::C"]);
    let with_macro = format!("macro two()->expr{{quote{{2}}}} {source}");
    let support = vesper_syntax::macro_source::parse(&with_macro, Default::default()).unwrap();
    let items = &support.support_items().items;
    assert_eq!(items.len(), 4, "{items:?}");
}

#[test]
fn untyped_members_alias_other_untyped_members() {
    assert_eq!(
        run("enum A: _ { X = 1 } enum B: _ { Y = A::X } fn main() -> u64 { B::Y }"),
        "1"
    );
}

#[test]
fn untyped_members_get_the_declaration_checks() {
    for (source, code) in [
        (
            "enum E: _ { A = 1, A = 2 } fn main() -> u64 { E::A }",
            "E0100",
        ),
        (
            "struct E { x: u64 } enum E: _ { A = 1 } fn main() -> u64 { 0 }",
            "E1542",
        ),
    ] {
        let p = parser::parse(source);
        let codes: Vec<String> = p
            .diagnostics
            .iter()
            .map(|d| d.code.clone())
            .chain(
                check::check(&p.module)
                    .diagnostics
                    .iter()
                    .map(|d| d.code.clone()),
            )
            .collect();
        assert!(codes.iter().any(|c| c == code), "{source}: {codes:?}");
    }
}

#[test]
fn one_enum_name_cannot_import_two_enums() {
    for lib in [
        "use self::a::E;\nuse self::b::E;\nfn main() -> u64 { E::X }\n",
        "use self::a::E;\nenum E: u64 { Z }\nfn main() -> u64 { E::Z }\n",
    ] {
        let dir = package(
            "clash",
            &[
                ("Loom.toml", &manifest("@demo/clash", "")),
                ("src/a.ves", "pub enum E: u64 { X = 1 }\n"),
                ("src/b.ves", "pub enum E: u64 { X = 2, Y }\n"),
                ("src/lib.ves", lib),
            ],
        );
        let codes = package_codes(&dir);
        assert!(codes.iter().any(|c| c == "E0113"), "{lib}: {codes:?}");
    }
}
