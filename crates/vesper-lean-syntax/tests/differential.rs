//! The Vesper port (`vesper_lean_syntax`, this package) against the frozen
//! Rust seed (`lean_seed`) on generated syntax trees: printed text, the
//! first error (malformed nodes are generated on purpose, several per tree,
//! so the error order is checked too), reference sets and the name and
//! literal checks. The same generator builds the tree for both crates from
//! one seed.

/// A small xorshift generator, so failures reproduce.
pub struct Rng(pub u64);
impl Rng {
    pub fn step(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.step() % n
    }
    pub fn chance(&mut self, n: u64) -> bool {
        self.below(n) == 0
    }
}

pub const TEXTS: [&str; 10] = [
    "x",
    "y",
    "Nat",
    "a.b",
    "名前",
    "end\naxiom injected : False\n\" 🦀",
    "tab\there\\'q'",
    "\u{1}\u{7f}\u{85}\u{2028}\u{2029}",
    "_root_x",
    "if",
];

macro_rules! generator {
    ($module:ident, $krate:ident) => {
        pub mod $module {
            use super::{Rng, TEXTS};
            use ::$krate::*;

            fn text(rng: &mut Rng) -> String {
                TEXTS[rng.below(TEXTS.len() as u64) as usize].to_string()
            }
            fn ident(rng: &mut Rng) -> Ident {
                loop {
                    if let Ok(i) = Ident::new(text(rng)) {
                        return i;
                    }
                }
            }
            fn name(rng: &mut Rng) -> Name {
                let n = Name::qualified((0..1 + rng.below(2)).map(|_| ident(rng))).unwrap();
                if rng.chance(5) { n.rooted() } else { n }
            }
            fn many<T>(rng: &mut Rng, max: u64, mut f: impl FnMut(&mut Rng) -> T) -> Vec<T> {
                (0..rng.below(max + 1)).map(|_| f(rng)).collect()
            }
            fn binder(rng: &mut Rng, depth: u32) -> Binder {
                Binder {
                    name: if rng.chance(4) {
                        None
                    } else {
                        Some(ident(rng))
                    },
                    ty: Box::new(term(rng, depth)),
                    kind: [
                        BinderKind::Explicit,
                        BinderKind::Implicit,
                        BinderKind::Instance,
                    ][rng.below(3) as usize],
                }
            }
            fn binders(rng: &mut Rng, depth: u32, at_least_one: bool) -> Vec<Binder> {
                let mut v = many(rng, 2, |r| binder(r, depth));
                if at_least_one && v.is_empty() && !rng.chance(8) {
                    v.push(binder(rng, depth));
                }
                v
            }
            fn pattern(rng: &mut Rng, depth: u32) -> Pattern {
                let leaf = depth == 0;
                match rng.below(if leaf { 7 } else { 10 }) {
                    0 => Pattern::Wildcard,
                    1 => Pattern::Bind(ident(rng)),
                    2 => Pattern::Nat(NatLiteral::from(rng.step() >> rng.below(64))),
                    3 => Pattern::Bool(rng.chance(2)),
                    4 => Pattern::String(text(rng)),
                    5 => Pattern::Char(text(rng).chars().next().unwrap()),
                    6 => Pattern::Int(
                        [i128::MIN, i128::MAX, 0, -1, -10, 12345678901234567890123]
                            [rng.below(6) as usize]
                            ^ (rng.below(3) as i128),
                    ),
                    7 => Pattern::Constructor {
                        name: name(rng),
                        arguments: many(rng, 2, |r| pattern(r, depth - 1)),
                    },
                    8 => Pattern::Tuple(many(rng, 3, |r| pattern(r, depth - 1))),
                    _ => Pattern::Record {
                        fields: many(rng, 2, |r| (ident(r), pattern(r, depth - 1))),
                    },
                }
            }
            fn binding(rng: &mut Rng, depth: u32) -> LetBinding {
                LetBinding {
                    pattern: pattern(rng, 1),
                    ty: if rng.chance(2) {
                        Some(Box::new(term(rng, depth)))
                    } else {
                        None
                    },
                    value: term(rng, depth),
                }
            }
            fn fields(rng: &mut Rng, depth: u32) -> Vec<(Ident, Term)> {
                many(rng, 2, |r| (ident(r), term(r, depth)))
            }
            fn statement(rng: &mut Rng, depth: u32) -> DoStatement {
                let d = depth.saturating_sub(1);
                match rng.below(if depth == 0 { 6 } else { 8 }) {
                    0 => DoStatement::Let {
                        binding: binding(rng, d),
                        mutable: rng.chance(2),
                    },
                    1 => DoStatement::Bind {
                        pattern: pattern(rng, 1),
                        action: term(rng, d),
                    },
                    2 => DoStatement::Assign {
                        local: ident(rng),
                        value: term(rng, d),
                    },
                    3 => DoStatement::Expr(term(rng, d)),
                    4 => DoStatement::BindTyped {
                        pattern: pattern(rng, 1),
                        ty: term(rng, d),
                        action: term(rng, d),
                    },
                    5 => DoStatement::LetInstance {
                        name: if rng.chance(2) {
                            Some(ident(rng))
                        } else {
                            None
                        },
                        ty: term(rng, d),
                        value: term(rng, d),
                    },
                    6 => DoStatement::If {
                        condition: term(rng, d),
                        yes: many(rng, 2, |r| statement(r, d)),
                        no: if rng.chance(2) {
                            Some(many(rng, 2, |r| statement(r, d)))
                        } else {
                            None
                        },
                    },
                    _ => DoStatement::Match {
                        scrutinee: term(rng, d),
                        arms: many(rng, 2, |r| (pattern(r, 1), many(r, 2, |r| statement(r, d)))),
                    },
                }
            }
            pub fn term(rng: &mut Rng, depth: u32) -> Term {
                if depth == 0 {
                    return match rng.below(9) {
                        0 => Term::Infer,
                        1 | 2 => Term::Ref(name(rng)),
                        3 => Term::Sort(if rng.chance(2) {
                            Sort::Prop
                        } else {
                            Sort::Type(rng.below(3) as u32 * 1_000_000)
                        }),
                        4 => Term::Nat(NatLiteral::from(rng.step())),
                        5 => Term::Bool(rng.chance(2)),
                        6 => Term::String(text(rng)),
                        7 => Term::Char(text(rng).chars().last().unwrap()),
                        _ => [Term::Rfl, Term::ByDecide][rng.below(2) as usize].clone(),
                    };
                }
                let d = depth - 1;
                let bx = |rng: &mut Rng| Box::new(term(rng, d));
                match rng.below(24) {
                    0 | 1 => Term::App {
                        function: bx(rng),
                        arguments: many(rng, 3, |r| {
                            if r.chance(3) {
                                Argument::Named {
                                    name: ident(r),
                                    value: term(r, d),
                                }
                            } else {
                                Argument::Positional(term(r, d))
                            }
                        }),
                    },
                    2 => Term::Lambda {
                        binders: binders(rng, d, true),
                        body: bx(rng),
                    },
                    3 => Term::Forall {
                        binders: binders(rng, d, true),
                        body: bx(rng),
                    },
                    4 => Term::Arrow(bx(rng), bx(rng)),
                    5 => Term::ProductType(many(rng, 3, |r| term(r, d))),
                    6 => Term::Tuple(many(rng, 3, |r| term(r, d))),
                    7 => Term::List(many(rng, 3, |r| term(r, d))),
                    8 => Term::Let {
                        binding: Box::new(binding(rng, d)),
                        body: bx(rng),
                    },
                    9 => Term::If {
                        condition: bx(rng),
                        yes: bx(rng),
                        no: bx(rng),
                    },
                    10 => Term::Match {
                        scrutinee: bx(rng),
                        arms: many(rng, 2, |r| MatchArm {
                            pattern: pattern(r, 1),
                            body: term(r, d),
                        }),
                    },
                    11 => Term::Project {
                        base: bx(rng),
                        projection: if rng.chance(2) {
                            Projection::Field(ident(rng))
                        } else {
                            Projection::Tuple(
                                std::num::NonZeroU32::new(1 + rng.below(12) as u32).unwrap(),
                            )
                        },
                    },
                    12 => Term::Ascribe {
                        value: bx(rng),
                        ty: bx(rng),
                    },
                    13 => Term::Unary {
                        op: [UnaryOp::Neg, UnaryOp::Not, UnaryOp::BoolNot][rng.below(3) as usize],
                        value: bx(rng),
                    },
                    14 | 15 => Term::Binary {
                        op: [
                            BinaryOp::Add,
                            BinaryOp::Sub,
                            BinaryOp::Mul,
                            BinaryOp::Div,
                            BinaryOp::Mod,
                            BinaryOp::Eq,
                            BinaryOp::Ne,
                            BinaryOp::BoolEq,
                            BinaryOp::BoolNe,
                            BinaryOp::Lt,
                            BinaryOp::Le,
                            BinaryOp::Gt,
                            BinaryOp::Ge,
                            BinaryOp::And,
                            BinaryOp::Or,
                            BinaryOp::BoolAnd,
                            BinaryOp::BoolOr,
                            BinaryOp::Iff,
                        ][rng.below(18) as usize],
                        left: bx(rng),
                        right: bx(rng),
                    },
                    16 => Term::Record {
                        fields: fields(rng, d),
                    },
                    17 => Term::Update {
                        base: bx(rng),
                        fields: fields(rng, d),
                    },
                    18 | 19 => Term::Do {
                        statements: many(rng, 3, |r| statement(r, d)),
                        result: bx(rng),
                    },
                    20 => Term::Eliminate { value: bx(rng) },
                    _ => term(rng, 0),
                }
            }
            fn definition(rng: &mut Rng, depth: u32) -> Definition {
                Definition {
                    name: name(rng),
                    binders: binders(rng, depth, false),
                    result: term(rng, depth),
                    body: term(rng, depth),
                    kind: [
                        DefinitionKind::Def,
                        DefinitionKind::NoncomputableDef,
                        DefinitionKind::PartialDef,
                        DefinitionKind::Abbrev,
                        DefinitionKind::SimpDef,
                    ][rng.below(5) as usize],
                }
            }
            fn field_list(rng: &mut Rng, depth: u32) -> Vec<Field> {
                many(rng, 2, |r| Field {
                    name: ident(r),
                    ty: term(r, depth),
                    default: if r.chance(2) {
                        Some(term(r, depth))
                    } else {
                        None
                    },
                })
            }
            fn type_declaration(rng: &mut Rng, depth: u32) -> TypeDeclaration {
                match rng.below(3) {
                    0 => TypeDeclaration::Class {
                        name: name(rng),
                        parameters: binders(rng, depth, false),
                        fields: field_list(rng, depth),
                    },
                    1 => TypeDeclaration::Inductive {
                        name: name(rng),
                        parameters: binders(rng, depth, false),
                        sort: term(rng, 0),
                        constructors: many(rng, 2, |r| Constructor {
                            name: ident(r),
                            binders: binders(r, depth, false),
                            result: if r.chance(2) {
                                Some(term(r, depth))
                            } else {
                                None
                            },
                        }),
                        deriving: many(rng, 2, name),
                    },
                    _ => TypeDeclaration::Structure {
                        name: name(rng),
                        parameters: binders(rng, depth, false),
                        fields: field_list(rng, depth),
                        deriving: many(rng, 2, name),
                    },
                }
            }
            fn declaration(rng: &mut Rng, depth: u32) -> Declaration {
                match rng.below(if depth == 0 { 10 } else { 11 }) {
                    0 => Declaration::Instance(Instance {
                        name: if rng.chance(2) { Some(name(rng)) } else { None },
                        binders: binders(rng, depth, false),
                        ty: term(rng, depth),
                        body: term(rng, depth),
                    }),
                    1 => Declaration::SetOption {
                        option: [
                            LeanOption::LinterUnusedVariables,
                            LeanOption::MaxRecDepth,
                            LeanOption::MaxHeartbeats,
                        ][rng.below(3) as usize],
                        value: if rng.chance(2) {
                            OptionValue::Bool(rng.chance(2))
                        } else {
                            OptionValue::Nat(rng.step())
                        },
                    },
                    2 | 3 => Declaration::Definition(definition(rng, depth)),
                    4 => Declaration::Type(type_declaration(rng, depth)),
                    5 => Declaration::MutualDefinitions(many(rng, 2, |r| definition(r, depth))),
                    6 => Declaration::MutualTypes(many(rng, 2, |r| type_declaration(r, depth))),
                    7 => Declaration::Theorem {
                        name: name(rng),
                        binders: binders(rng, depth, false),
                        proposition: term(rng, depth),
                        proof: term(rng, depth),
                    },
                    8 => Declaration::Example {
                        binders: binders(rng, depth, false),
                        proposition: term(rng, depth),
                        proof: term(rng, depth),
                    },
                    9 => {
                        if rng.chance(2) {
                            Declaration::Eval(term(rng, depth))
                        } else {
                            Declaration::Check(term(rng, depth))
                        }
                    }
                    _ => Declaration::Namespace {
                        name: name(rng),
                        declarations: many(rng, 2, |r| declaration(r, depth - 1)),
                    },
                }
            }
            pub fn module(rng: &mut Rng, depth: u32) -> Module {
                Module {
                    imports: many(rng, 2, |r| {
                        ModuleName::new(["Init", "Std.Data", "A_1.b2"][r.below(3) as usize])
                            .unwrap()
                    }),
                    declarations: many(rng, 3, |r| declaration(r, depth)),
                }
            }
            pub fn free(rng: &mut Rng, depth: u32) -> Vec<String> {
                let params = binders(rng, depth, false);
                let body = term(rng, depth);
                Term::free_references(&params, &body)
                    .iter()
                    .map(|n| format!("{n:?}"))
                    .collect()
            }
            pub fn references(rng: &mut Rng, depth: u32) -> Vec<String> {
                term(rng, depth)
                    .references()
                    .iter()
                    .map(|n| format!("{n:?}"))
                    .collect()
            }
            pub fn checks(value: &str) -> String {
                format!(
                    "{:?} {:?} {:?}",
                    Ident::new(value).map(|i| i.as_str().to_string()),
                    NatLiteral::decimal(value).map(|n| n.as_str().to_string()),
                    ModuleName::new(value).is_ok()
                )
            }
        }
    };
}
generator!(rust, lean_seed);
generator!(port, vesper_lean_syntax);

#[test]
fn builds_are_the_expected_ones() {
    assert_eq!(vesper_lean_syntax::IMPLEMENTATION, "vesper");
    assert_eq!(lean_seed::IMPLEMENTATION, "rust");
}

#[test]
fn terms_print_the_same_text_and_errors() {
    let (mut ok, mut err) = (0, 0);
    for seed in 1..4000u64 {
        let depth = (seed % 5) as u32;
        let r = rust::term(&mut Rng(seed), depth).render();
        let p = port::term(&mut Rng(seed), depth).render();
        assert_eq!(format!("{r:?}"), format!("{p:?}"), "seed {seed}");
        if r.is_ok() { ok += 1 } else { err += 1 }
    }
    assert!(ok > 1000 && err > 300, "{ok} printed, {err} refused");
}

#[test]
fn modules_print_the_same_text_and_errors() {
    let (mut ok, mut err) = (0, 0);
    for seed in 1..3000u64 {
        let depth = (seed % 4) as u32;
        let r = rust::module(&mut Rng(seed), depth).render();
        let p = port::module(&mut Rng(seed), depth).render();
        assert_eq!(format!("{r:?}"), format!("{p:?}"), "seed {seed}");
        if r.is_ok() { ok += 1 } else { err += 1 }
    }
    assert!(ok > 500 && err > 300, "{ok} printed, {err} refused");
}

#[test]
fn reference_sets_are_the_same() {
    for seed in 1..3000u64 {
        let depth = (seed % 5) as u32;
        assert_eq!(
            rust::free(&mut Rng(seed), depth),
            port::free(&mut Rng(seed), depth),
            "free {seed}"
        );
        assert_eq!(
            rust::references(&mut Rng(seed), depth),
            port::references(&mut Rng(seed), depth),
            "references {seed}"
        );
    }
}

#[test]
fn name_and_literal_checks_agree() {
    let mut values: Vec<String> = [
        "", "_root_", "_root", "a", "0", "00", "01", "10", "123", "12a", "A.b", "A..b", ".A", "A.",
        "a_1.B2", "1a", "a-b", "«x", "x»", "\u{202a}", "\u{202e}", "\u{202f}", "\u{2065}",
        "\u{2066}", "\u{2069}", "\u{206a}", "\u{feff}", "\u{9f}", "\u{a0}", "\t", "é", "名",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    values.extend(super_texts());
    for v in &values {
        assert_eq!(rust::checks(v), port::checks(v), "{v:?}");
    }
}

fn super_texts() -> Vec<String> {
    TEXTS.iter().map(|s| s.to_string()).collect()
}
