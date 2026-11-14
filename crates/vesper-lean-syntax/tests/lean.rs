use std::{
    fs,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
};
use vesper_lean_syntax::*;

struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn id(value: &str) -> Ident {
    Ident::new(value).unwrap()
}
fn name(value: &str) -> Name {
    Name::qualified(value.split('.').map(id)).unwrap()
}
fn r(value: &str) -> Term {
    Term::Ref(name(value))
}
fn n(value: u64) -> Term {
    Term::Nat(value.into())
}
fn app(function: &str, args: Vec<Term>) -> Term {
    Term::app(r(function), args)
}
fn b(value: &str, ty: Term) -> Binder {
    Binder::explicit(id(value), ty)
}
fn eq(left: Term, right: Term) -> Term {
    Term::binary(BinaryOp::Eq, left, right)
}
fn check_eq(left: Term, right: Term) -> Declaration {
    Declaration::Example {
        binders: vec![],
        proposition: eq(left, right),
        proof: Term::ByDecide,
    }
}
fn def(value: &str, binders: Vec<Binder>, result: Term, body: Term) -> Declaration {
    Declaration::Definition(Definition {
        name: name(value),
        binders,
        result,
        body,
        kind: DefinitionKind::Def,
    })
}
fn module(declarations: Vec<Declaration>) -> Module {
    Module {
        imports: vec![ModuleName::new("Std").unwrap()],
        declarations,
    }
}

fn lean(module: Module) -> Option<String> {
    let source = module.render().unwrap();
    let executable = std::env::var("VESPER_LEAN").unwrap_or_else(|_| "lean".into());
    let version = match Command::new(&executable).arg("--version").output() {
        Ok(version) => version,
        Err(error)
            if std::env::var_os("VESPER_LEAN").is_none()
                && error.kind() == std::io::ErrorKind::NotFound =>
        {
            eprintln!("Lean test skipped: {error}; set VESPER_LEAN to pinned Lean 4.24.0");
            return None;
        }
        Err(error) => panic!("configured Lean failed: {error}"),
    };
    assert!(version.status.success());
    assert!(
        String::from_utf8_lossy(&version.stdout).contains("version 4.24.0"),
        "use the pinned toolchain"
    );
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let directory = std::env::temp_dir().join(format!(
        "vesper-lean-syntax-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    let builder = &mut fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(&directory).unwrap();
    let scratch = Scratch(directory);
    let path = scratch.0.join("Model.lean");
    fs::write(&path, &source).unwrap();
    let output = Command::new(executable).arg(&path).output().unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\n{source}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Some(String::from_utf8(output.stdout).unwrap())
}

#[test]
fn identifiers_are_injective_and_cannot_break_quotations() {
    for invalid in [
        "",
        "_root_",
        "x»; axiom hacked : False",
        "«x",
        "x\ny",
        "x\0",
        "x\u{202e}y",
        "x\u{2066}",
    ] {
        assert!(Ident::new(invalid).is_err(), "{invalid:?}");
    }
    for invalid in [
        "",
        "../Lean",
        "A..B",
        "A/B",
        "A\\B",
        "A\nimport Evil",
        "_root_.Lean",
        "1Foo",
    ] {
        assert!(ModuleName::new(invalid).is_err(), "{invalid:?}");
    }
    let dotted = Name::single("a.b").unwrap();
    assert_ne!(
        Term::Ref(dotted.clone()).render().unwrap(),
        r("a.b").render().unwrap()
    );
    let mut declarations = vec![
        def("a.b", vec![], r("Nat"), n(13)),
        Declaration::Definition(Definition {
            name: dotted.clone(),
            binders: vec![],
            result: r("Nat"),
            body: n(29),
            kind: DefinitionKind::Def,
        }),
        check_eq(r("a.b"), n(13)),
        check_eq(Term::Ref(dotted), n(29)),
    ];
    for (index, ident) in [
        "def",
        "λ",
        "hello world",
        "$block_0",
        "-- comment",
        "x/- #eval 99 -/",
        "_",
    ]
    .into_iter()
    .enumerate()
    {
        let name = Name::single(ident).unwrap();
        declarations.push(Declaration::Definition(Definition {
            name: name.clone(),
            binders: vec![],
            result: r("Nat"),
            body: n(index as u64),
            kind: DefinitionKind::Def,
        }));
        declarations.push(check_eq(Term::Ref(name), n(index as u64)));
    }
    lean(module(vec![Declaration::Namespace {
        name: Name::single("namespace").unwrap(),
        declarations,
    }]));
}

#[test]
fn decimal_literals_preserve_arbitrary_precision_without_backend_semantics() {
    for invalid in [
        "",
        "-1",
        "+1",
        "01",
        "1; #eval 7",
        "0xff",
        "1_000",
        "1.2",
        "1e2",
        "１２",
    ] {
        assert!(NatLiteral::decimal(invalid).is_err(), "{invalid:?}");
    }
    let digits = "340282366920938463463374607431768211456";
    let literal = Term::Nat(NatLiteral::decimal(digits).unwrap());
    let output = lean(module(vec![Declaration::Eval(Term::Ascribe {
        value: Box::new(literal),
        ty: Box::new(r("Nat")),
    })]));
    if let Some(output) = output {
        assert_eq!(output.trim(), digits);
    }
}

#[test]
fn strings_and_chars_roundtrip_control_unicode_quotes_and_injection_text() {
    let text = (0..=31)
        .chain(127..=159)
        .map(|c| char::from_u32(c).unwrap())
        .collect::<String>()
        + "\"'\\\nλ😀\u{2028}\u{2029}\"; #eval 987654; --";
    let literal = Term::String(text.clone());
    let expected = text.chars().rev().fold(r("List.nil"), |tail, c| {
        app("List.cons", vec![n(c as u64), tail])
    });
    let actual = app(
        "List.map",
        vec![r("Char.toNat"), app("String.toList", vec![literal.clone()])],
    );
    let mut declarations = vec![check_eq(actual, expected)];
    for c in [
        '\0', '\u{8}', '\u{c}', '\n', '\r', '\t', '\'', '\\', 'λ', '😀',
    ] {
        declarations.push(check_eq(
            app("Char.toNat", vec![Term::Char(c)]),
            n(c as u64),
        ));
    }
    let rendered = literal.render().unwrap();
    assert!(rendered.contains("\\x08") && rendered.contains("\\x0c"));
    lean(module(declarations));
}

#[test]
fn expression_precedence_binders_lets_conditionals_and_projections() {
    let calc = Term::binary(BinaryOp::Mul, Term::binary(BinaryOp::Add, n(1), n(2)), n(3));
    let lambda = Term::Lambda {
        binders: vec![b("x", r("Nat"))],
        body: Box::new(Term::Let {
            binding: Box::new(LetBinding {
                pattern: Pattern::Bind(id("y")),
                ty: Some(Box::new(r("Nat"))),
                value: Term::binary(BinaryOp::Add, r("x"), n(2)),
            }),
            body: Box::new(Term::If {
                condition: Box::new(Term::binary(BinaryOp::Gt, r("y"), n(5))),
                yes: Box::new(Term::binary(BinaryOp::Mul, r("y"), n(3))),
                no: Box::new(r("y")),
            }),
        }),
    };
    let tuple = Term::Ascribe {
        value: Box::new(Term::Tuple(vec![n(42), Term::String("value".into())])),
        ty: Box::new(Term::ProductType(vec![r("Nat"), r("String")])),
    };
    let forall = Term::Forall {
        binders: vec![b("n", r("Nat"))],
        body: Box::new(eq(r("n"), r("n"))),
    };
    lean(module(vec![
        check_eq(calc, n(9)),
        check_eq(Term::app(lambda, [n(7)]), n(27)),
        check_eq(
            Term::Project {
                base: Box::new(tuple),
                projection: Projection::Tuple(1.try_into().unwrap()),
            },
            n(42),
        ),
        check_eq(
            Term::Unary {
                op: UnaryOp::BoolNot,
                value: Box::new(Term::Bool(false)),
            },
            Term::Bool(true),
        ),
        check_eq(
            Term::Ascribe {
                value: Box::new(Term::Unary {
                    op: UnaryOp::Neg,
                    value: Box::new(n(3)),
                }),
                ty: Box::new(r("Int")),
            },
            app("Int.negSucc", vec![n(2)]),
        ),
        Declaration::Theorem {
            name: name("reflexivity"),
            binders: vec![],
            proposition: forall,
            proof: Term::Lambda {
                binders: vec![b("n", r("Nat"))],
                body: Box::new(Term::Rfl),
            },
        },
        Declaration::Check(Term::Sort(Sort::Type(1))),
        Declaration::Check(Term::Arrow(Box::new(r("Nat")), Box::new(r("Nat")))),
    ]));
}

#[test]
fn adts_records_named_arguments_and_match_patterns() {
    let option = TypeDeclaration::Inductive {
        name: name("Choice"),
        parameters: vec![b("A", Term::Sort(Sort::Type(0)))],
        sort: Term::Sort(Sort::Type(0)),
        constructors: vec![
            Constructor {
                name: id("none"),
                binders: vec![],
                result: None,
            },
            Constructor {
                name: id("some"),
                binders: vec![b("value", r("A"))],
                result: None,
            },
        ],
        deriving: vec![name("Repr")],
    };
    let record = TypeDeclaration::Structure {
        name: name("Counter"),
        parameters: vec![],
        fields: vec![
            Field {
                name: id("value"),
                ty: r("Nat"),
                default: None,
            },
            Field {
                name: id("enabled"),
                ty: r("Bool"),
                default: Some(Term::Bool(true)),
            },
        ],
        deriving: vec![name("Repr")],
    };
    let mut implicit = b("A", Term::Sort(Sort::Type(0)));
    implicit.kind = BinderKind::Implicit;
    let get_or = def(
        "getOr",
        vec![
            implicit,
            b("choice", app("Choice", vec![r("A")])),
            b("fallback", r("A")),
        ],
        r("A"),
        Term::Match {
            scrutinee: Box::new(r("choice")),
            arms: vec![
                MatchArm {
                    pattern: Pattern::Constructor {
                        name: name("Choice.none"),
                        arguments: vec![],
                    },
                    body: r("fallback"),
                },
                MatchArm {
                    pattern: Pattern::Constructor {
                        name: name("Choice.some"),
                        arguments: vec![Pattern::Bind(id("value"))],
                    },
                    body: r("value"),
                },
            ],
        },
    );
    let call = Term::App {
        function: Box::new(r("getOr")),
        arguments: vec![
            Argument::Named {
                name: id("A"),
                value: r("Nat"),
            },
            Argument::Positional(app("Choice.some", vec![n(42)])),
            Argument::Positional(n(0)),
        ],
    };
    let point = def(
        "point",
        vec![],
        r("Counter"),
        Term::Record {
            fields: vec![(id("value"), n(41)), (id("enabled"), Term::Bool(false))],
        },
    );
    let updated = Term::Project {
        base: Box::new(Term::Update {
            base: Box::new(r("point")),
            fields: vec![(id("value"), n(42))],
        }),
        projection: Projection::Field(id("value")),
    };
    lean(module(vec![
        Declaration::Type(option),
        Declaration::Type(record),
        get_or,
        point,
        check_eq(call, n(42)),
        check_eq(updated, n(42)),
    ]));
}

#[test]
fn mutual_partial_cfg_executes_without_a_termination_claim() {
    let recursive = |function: &str, body| Definition {
        name: name(function),
        binders: vec![b("n", r("Nat"))],
        result: r("Nat"),
        body,
        kind: DefinitionKind::PartialDef,
    };
    let group = Declaration::MutualDefinitions(vec![
        recursive(
            "block0",
            Term::If {
                condition: Box::new(eq(r("n"), n(0))),
                yes: Box::new(n(7)),
                no: Box::new(app(
                    "block1",
                    vec![Term::binary(BinaryOp::Sub, r("n"), n(1))],
                )),
            },
        ),
        recursive("block1", app("block0", vec![r("n")])),
    ]);
    let output = lean(module(vec![
        group,
        Declaration::Eval(app("block0", vec![n(100)])),
    ]));
    if let Some(output) = output {
        assert_eq!(output.trim(), "7");
    }
}

#[test]
fn mutually_recursive_inductives_compile_and_execute() {
    let tree = TypeDeclaration::Inductive {
        name: name("Tree"),
        parameters: vec![],
        sort: Term::Sort(Sort::Type(0)),
        constructors: vec![Constructor {
            name: id("node"),
            binders: vec![b("children", r("Forest"))],
            result: None,
        }],
        deriving: vec![name("Repr")],
    };
    let forest = TypeDeclaration::Inductive {
        name: name("Forest"),
        parameters: vec![],
        sort: Term::Sort(Sort::Type(0)),
        constructors: vec![
            Constructor {
                name: id("nil"),
                binders: vec![],
                result: None,
            },
            Constructor {
                name: id("cons"),
                binders: vec![b("head", r("Tree")), b("tail", r("Forest"))],
                result: None,
            },
        ],
        deriving: vec![name("Repr")],
    };
    let output = lean(module(vec![
        Declaration::MutualTypes(vec![tree, forest]),
        Declaration::Eval(app("Tree.node", vec![r("Forest.nil")])),
    ]));
    if let Some(output) = output {
        assert!(output.contains("Tree.node"));
    }
}

#[test]
fn do_bind_mutation_and_tuple_let_execute_as_id() {
    let body = Term::Do {
        statements: vec![
            DoStatement::Let {
                binding: LetBinding {
                    pattern: Pattern::Bind(id("total")),
                    ty: Some(Box::new(r("Nat"))),
                    value: n(0),
                },
                mutable: true,
            },
            DoStatement::Assign {
                local: id("total"),
                value: Term::binary(BinaryOp::Add, r("total"), n(2)),
            },
            DoStatement::Bind {
                pattern: Pattern::Bind(id("x")),
                action: app("pure", vec![n(40)]),
            },
            DoStatement::Expr(app("pure", vec![Term::Tuple(vec![])])),
        ],
        result: Box::new(app(
            "pure",
            vec![Term::binary(BinaryOp::Add, r("total"), r("x"))],
        )),
    };
    let tuple_let = Term::Let {
        binding: Box::new(LetBinding {
            pattern: Pattern::Tuple(vec![Pattern::Bind(id("left")), Pattern::Bind(id("right"))]),
            ty: None,
            value: Term::Tuple(vec![n(20), n(22)]),
        }),
        body: Box::new(Term::binary(BinaryOp::Add, r("left"), r("right"))),
    };
    let output = lean(module(vec![
        Declaration::Eval(Term::Ascribe {
            value: Box::new(app("Id.run", vec![body])),
            ty: Box::new(r("Nat")),
        }),
        check_eq(tuple_let, n(42)),
    ]));
    if let Some(output) = output {
        assert_eq!(output.trim(), "42");
    }
}

#[test]
fn malformed_structural_nodes_fail_before_source_emission() {
    for term in [
        Term::Tuple(vec![n(1)]),
        Term::ProductType(vec![r("Nat")]),
        Term::Lambda {
            binders: vec![],
            body: Box::new(n(0)),
        },
        Term::Match {
            scrutinee: Box::new(n(0)),
            arms: vec![],
        },
        Term::Update {
            base: Box::new(r("point")),
            fields: vec![],
        },
    ] {
        assert!(term.render().is_err());
    }
    assert!(
        module(vec![Declaration::MutualDefinitions(vec![])])
            .render()
            .is_err()
    );
    assert!(
        module(vec![Declaration::Namespace {
            name: name("N").rooted(),
            declarations: vec![]
        }])
        .render()
        .is_err()
    );
    assert!(
        Term::Do {
            statements: vec![DoStatement::Let {
                binding: LetBinding {
                    pattern: Pattern::Wildcard,
                    ty: None,
                    value: n(0)
                },
                mutable: true
            }],
            result: Box::new(n(0))
        }
        .render()
        .is_err()
    );
}

fn lambda(names: &[&str], body: Term) -> Term {
    Term::Lambda {
        binders: names.iter().map(|name| b(name, Term::Infer)).collect(),
        body: Box::new(body),
    }
}
fn arrows(arguments: Vec<Term>, result: Term) -> Term {
    arguments.into_iter().rev().fold(result, |result, arg| {
        Term::Arrow(Box::new(arg), Box::new(result))
    })
}
fn implicit(value: &str, ty: Term) -> Binder {
    let mut binder = b(value, ty);
    binder.kind = BinderKind::Implicit;
    binder
}
fn constraint(value: Term) -> Binder {
    Binder {
        name: None,
        ty: Box::new(value),
        kind: BinderKind::Instance,
    }
}

#[test]
fn cps_monad_effect_class_and_lifting_instances_execute() {
    let universe = || Term::Sort(Sort::Type(0));
    let monad_ty = || arrows(vec![universe()], universe());
    let cps = |s: Term, o: Term, m: Term, a: Term| app("HandlerCPS", vec![s, o, m, a]);
    let monad_constraint = || constraint(app("Monad", vec![r("m")]));
    let parameters = || {
        vec![
            implicit("S", universe()),
            implicit("O", universe()),
            implicit("m", monad_ty()),
            implicit("E", universe()),
        ]
    };
    let mut declarations = vec![
        Declaration::SetOption {
            option: LeanOption::LinterUnusedVariables,
            value: OptionValue::Bool(false),
        },
        Declaration::Definition(Definition {
            name: name("HandlerCPS"),
            binders: vec![
                b("S", universe()),
                b("O", universe()),
                b("m", monad_ty()),
                b("A", universe()),
            ],
            result: universe(),
            body: arrows(
                vec![arrows(vec![r("A"), r("S")], app("m", vec![r("O")])), r("S")],
                app("m", vec![r("O")]),
            ),
            kind: DefinitionKind::Abbrev,
        }),
        Declaration::Instance(Instance {
            name: Some(name("instHandlerMonad")),
            binders: vec![monad_constraint()],
            ty: app(
                "Monad",
                vec![app("HandlerCPS", vec![r("S"), r("O"), r("m")])],
            ),
            body: Term::Record {
                fields: vec![
                    (
                        id("pure"),
                        lambda(&["a", "k", "s"], app("k", vec![r("a"), r("s")])),
                    ),
                    (
                        id("bind"),
                        lambda(
                            &["action", "next", "k", "s"],
                            app(
                                "action",
                                vec![
                                    lambda(
                                        &["a", "s'"],
                                        app("next", vec![r("a"), r("k"), r("s'")]),
                                    ),
                                    r("s"),
                                ],
                            ),
                        ),
                    ),
                ],
            },
        }),
        Declaration::Definition(Definition {
            name: name("liftAction"),
            binders: vec![monad_constraint(), b("action", app("m", vec![r("A")]))],
            result: cps(r("S"), r("O"), r("m"), r("A")),
            kind: DefinitionKind::Def,
            body: lambda(
                &["k", "s"],
                Term::Do {
                    statements: vec![DoStatement::Bind {
                        pattern: Pattern::Bind(id("value")),
                        action: r("action"),
                    }],
                    result: Box::new(app("k", vec![r("value"), r("s")])),
                },
            ),
        }),
        Declaration::Type(TypeDeclaration::Class {
            name: name("Effect_Fail"),
            parameters: vec![b("E", universe()), b("m", monad_ty())],
            fields: vec![Field {
                name: id("raise"),
                ty: arrows(vec![r("E")], app("m", vec![r("Empty")])),
                default: None,
            }],
        }),
    ];
    let mut binders = parameters();
    binders.push(monad_constraint());
    binders.push(b("evidence", app("Effect_Fail", vec![r("E"), r("m")])));
    let raise = Term::App {
        function: Box::new(r("Effect_Fail.raise")),
        arguments: vec![
            Argument::Named {
                name: id("self"),
                value: r("evidence"),
            },
            Argument::Positional(r("error")),
        ],
    };
    declarations.push(Declaration::Definition(Definition {
        name: name("lift_Fail"),
        binders,
        result: app(
            "Effect_Fail",
            vec![r("E"), app("HandlerCPS", vec![r("S"), r("O"), r("m")])],
        ),
        kind: DefinitionKind::Def,
        body: Term::Record {
            fields: vec![(
                id("raise"),
                lambda(
                    &["error", "k", "s"],
                    Term::Do {
                        statements: vec![DoStatement::Bind {
                            pattern: Pattern::Bind(id("result")),
                            action: raise,
                        }],
                        result: Box::new(app("k", vec![r("result"), r("s")])),
                    },
                ),
            )],
        },
    }));
    let mut binders = parameters();
    binders.push(monad_constraint());
    binders.push(Binder {
        name: Some(id("evidence")),
        ty: Box::new(app("Effect_Fail", vec![r("E"), r("m")])),
        kind: BinderKind::Instance,
    });
    declarations.push(Declaration::Instance(Instance {
        name: Some(name("instLiftFail")),
        binders,
        ty: app(
            "Effect_Fail",
            vec![r("E"), app("HandlerCPS", vec![r("S"), r("O"), r("m")])],
        ),
        body: app("lift_Fail", vec![r("evidence")]),
    }));
    declarations.push(Declaration::Instance(Instance {
        name: None,
        binders: vec![],
        ty: app("Effect_Fail", vec![r("Nat"), app("Except", vec![r("Nat")])]),
        body: Term::Record {
            fields: vec![(
                id("raise"),
                lambda(&["error"], app("Except.error", vec![r("error")])),
            )],
        },
    }));
    declarations.push(def(
        "failing",
        vec![],
        cps(r("Nat"), r("Nat"), app("Except", vec![r("Nat")]), r("Nat")),
        Term::Do {
            statements: vec![DoStatement::Bind {
                pattern: Pattern::Bind(id("impossible")),
                action: app("Effect_Fail.raise", vec![n(35)]),
            }],
            result: Box::new(Term::Eliminate {
                value: Box::new(r("impossible")),
            }),
        },
    ));
    let continuation = || {
        lambda(
            &["value", "state"],
            app(
                "Except.ok",
                vec![Term::binary(BinaryOp::Add, r("value"), r("state"))],
            ),
        )
    };
    declarations.push(Declaration::Eval(app(
        "failing",
        vec![continuation(), n(2)],
    )));
    let success = Term::Ascribe {
        value: Box::new(app("liftAction", vec![app("Except.ok", vec![n(40)])])),
        ty: Box::new(cps(
            r("Nat"),
            r("Nat"),
            app("Except", vec![r("Nat")]),
            r("Nat"),
        )),
    };
    declarations.push(Declaration::Eval(Term::app(
        success,
        [continuation(), n(2)],
    )));
    let output = lean(module(declarations));
    if let Some(output) = output {
        assert_eq!(output.trim(), "Except.error 35\nExcept.ok 42");
    }
}

#[test]
fn statement_branches_preserve_outer_mutation_and_typed_bindings() {
    let body = Term::Do {
        statements: vec![
            DoStatement::Let {
                binding: LetBinding {
                    pattern: Pattern::Bind(id("total")),
                    ty: Some(Box::new(r("Nat"))),
                    value: n(0),
                },
                mutable: true,
            },
            DoStatement::If {
                condition: Term::Bool(true),
                yes: vec![
                    DoStatement::Assign {
                        local: id("total"),
                        value: n(1),
                    },
                    DoStatement::Assign {
                        local: id("total"),
                        value: Term::binary(BinaryOp::Add, r("total"), n(1)),
                    },
                ],
                no: Some(vec![DoStatement::Assign {
                    local: id("total"),
                    value: n(100),
                }]),
            },
            DoStatement::If {
                condition: Term::Bool(false),
                yes: vec![],
                no: None,
            },
            DoStatement::Match {
                scrutinee: app("Option.some", vec![n(3)]),
                arms: vec![
                    (
                        Pattern::Constructor {
                            name: name("Option.some"),
                            arguments: vec![Pattern::Bind(id("value"))],
                        },
                        vec![DoStatement::Assign {
                            local: id("total"),
                            value: Term::binary(BinaryOp::Add, r("total"), r("value")),
                        }],
                    ),
                    (
                        Pattern::Constructor {
                            name: name("Option.none"),
                            arguments: vec![],
                        },
                        vec![],
                    ),
                ],
            },
            DoStatement::BindTyped {
                pattern: Pattern::Bind(id("more")),
                ty: r("Nat"),
                action: app("pure", vec![n(37)]),
            },
            DoStatement::Assign {
                local: id("total"),
                value: Term::binary(BinaryOp::Add, r("total"), r("more")),
            },
        ],
        result: Box::new(app("pure", vec![r("total")])),
    };
    let tuple = Term::Do {
        statements: vec![DoStatement::BindTyped {
            pattern: Pattern::Tuple(vec![Pattern::Bind(id("a")), Pattern::Bind(id("b"))]),
            ty: Term::ProductType(vec![r("Nat"), r("Nat")]),
            action: app("pure", vec![Term::Tuple(vec![n(20), n(22)])]),
        }],
        result: Box::new(app(
            "pure",
            vec![Term::binary(BinaryOp::Add, r("a"), r("b"))],
        )),
    };
    let output = lean(module(vec![
        Declaration::Eval(Term::Ascribe {
            value: Box::new(app("Id.run", vec![body])),
            ty: Box::new(r("Nat")),
        }),
        Declaration::Eval(Term::Ascribe {
            value: Box::new(app("Id.run", vec![tuple])),
            ty: Box::new(r("Nat")),
        }),
    ]));
    if let Some(output) = output {
        assert_eq!(output.trim(), "42\n42");
    }
}

#[test]
fn signed_patterns_records_empty_dictionaries_and_fixed_options_compile() {
    let point = TypeDeclaration::Structure {
        name: name("Point"),
        parameters: vec![],
        fields: vec![Field {
            name: id("value"),
            ty: r("Nat"),
            default: None,
        }],
        deriving: vec![],
    };
    let recordlet = Term::Let {
        binding: Box::new(LetBinding {
            pattern: Pattern::Record {
                fields: vec![(id("value"), Pattern::Bind(id("v")))],
            },
            ty: Some(Box::new(r("Point"))),
            value: Term::Record {
                fields: vec![(id("value"), n(42))],
            },
        }),
        body: Box::new(r("v")),
    };
    let negative = def(
        "negative",
        vec![b("x", r("Int64"))],
        r("Nat"),
        Term::Match {
            scrutinee: Box::new(r("x")),
            arms: vec![
                MatchArm {
                    pattern: Pattern::Int(-1),
                    body: n(7),
                },
                MatchArm {
                    pattern: Pattern::Wildcard,
                    body: n(0),
                },
            ],
        },
    );
    let empty = TypeDeclaration::Class {
        name: name("NoOps"),
        parameters: vec![],
        fields: vec![],
    };
    let mut declarations = vec![
        Declaration::SetOption {
            option: LeanOption::LinterUnusedVariables,
            value: OptionValue::Bool(false),
        },
        Declaration::SetOption {
            option: LeanOption::MaxRecDepth,
            value: OptionValue::Nat(2048),
        },
        Declaration::SetOption {
            option: LeanOption::MaxHeartbeats,
            value: OptionValue::Nat(200000),
        },
        Declaration::Type(point),
        check_eq(recordlet, n(42)),
        negative,
        check_eq(
            app(
                "negative",
                vec![Term::Unary {
                    op: UnaryOp::Neg,
                    value: Box::new(n(1)),
                }],
            ),
            n(7),
        ),
        Declaration::Type(empty),
    ];
    declarations.push(def(
        "dictionary",
        vec![],
        r("NoOps"),
        Term::Record { fields: vec![] },
    ));
    declarations.push(Declaration::Eval(Term::Ascribe {
        value: Box::new(app(
            "Id.run",
            vec![Term::Do {
                statements: vec![
                    DoStatement::LetInstance {
                        name: None,
                        ty: r("NoOps"),
                        value: r("dictionary"),
                    },
                    DoStatement::LetInstance {
                        name: Some(id("named")),
                        ty: r("NoOps"),
                        value: r("dictionary"),
                    },
                ],
                result: Box::new(app("pure", vec![n(42)])),
            }],
        )),
        ty: Box::new(r("Nat")),
    }));
    let output = lean(module(declarations));
    if let Some(output) = output {
        assert_eq!(output.trim(), "42");
    }
    assert!(
        module(vec![Declaration::SetOption {
            option: LeanOption::LinterUnusedVariables,
            value: OptionValue::Nat(0)
        }])
        .render()
        .is_err()
    );
}

#[test]
fn monadic_bind_control_threads_outer_mutable_locals() {
    let branch = |value: Term, result: Term| Term::Do {
        statements: vec![DoStatement::Assign {
            local: id("outer"),
            value,
        }],
        result: Box::new(app("pure", vec![result])),
    };
    let conditional = Term::If {
        condition: Box::new(Term::Bool(true)),
        yes: Box::new(branch(n(40), n(2))),
        no: Box::new(branch(n(100), n(3))),
    };
    let matched = Term::Match {
        scrutinee: Box::new(app("Option.some", vec![n(40)])),
        arms: vec![
            MatchArm {
                pattern: Pattern::Constructor {
                    name: name("Option.some"),
                    arguments: vec![Pattern::Bind(id("value"))],
                },
                body: branch(r("value"), n(2)),
            },
            MatchArm {
                pattern: Pattern::Constructor {
                    name: name("Option.none"),
                    arguments: vec![],
                },
                body: branch(n(100), n(3)),
            },
        ],
    };
    let nested = Term::Do {
        statements: vec![DoStatement::Assign {
            local: id("outer"),
            value: n(1),
        }],
        result: Box::new(conditional.clone()),
    };
    let declare = || DoStatement::Let {
        binding: LetBinding {
            pattern: Pattern::Bind(id("outer")),
            ty: Some(Box::new(r("Nat"))),
            value: n(0),
        },
        mutable: true,
    };
    let mut declarations = vec![];
    for action in [conditional, matched, nested] {
        for typed in [false, true] {
            let bind = if typed {
                DoStatement::BindTyped {
                    pattern: Pattern::Bind(id("value")),
                    ty: r("Nat"),
                    action: action.clone(),
                }
            } else {
                DoStatement::Bind {
                    pattern: Pattern::Bind(id("value")),
                    action: action.clone(),
                }
            };
            let body = Term::Do {
                statements: vec![declare(), bind],
                result: Box::new(app(
                    "pure",
                    vec![Term::binary(BinaryOp::Add, r("outer"), r("value"))],
                )),
            };
            declarations.push(Declaration::Eval(Term::Ascribe {
                value: Box::new(app("Id.run", vec![body])),
                ty: Box::new(r("Nat")),
            }));
        }
    }
    let tail = Term::If {
        condition: Box::new(Term::Bool(true)),
        yes: Box::new(branch(n(40), Term::binary(BinaryOp::Add, r("outer"), n(2)))),
        no: Box::new(app("pure", vec![n(0)])),
    };
    let body = Term::Do {
        statements: vec![declare()],
        result: Box::new(tail),
    };
    declarations.push(Declaration::Eval(Term::Ascribe {
        value: Box::new(app("Id.run", vec![body])),
        ty: Box::new(r("Nat")),
    }));
    let statement = Term::If {
        condition: Box::new(Term::Bool(true)),
        yes: Box::new(branch(n(40), Term::Tuple(vec![]))),
        no: Box::new(app("pure", vec![Term::Tuple(vec![])])),
    };
    let body = Term::Do {
        statements: vec![declare(), DoStatement::Expr(statement)],
        result: Box::new(app(
            "pure",
            vec![Term::binary(BinaryOp::Add, r("outer"), n(2))],
        )),
    };
    declarations.push(Declaration::Eval(Term::Ascribe {
        value: Box::new(app("Id.run", vec![body])),
        ty: Box::new(r("Nat")),
    }));
    let output = lean(module(declarations));
    if let Some(output) = output {
        assert_eq!(output.lines().collect::<Vec<_>>(), vec!["42"; 8]);
    }
}

#[test]
fn implicit_instance_binders_and_rooted_names_compile() {
    let mut monad = b(
        "m",
        Term::Arrow(
            Box::new(Term::Sort(Sort::Type(0))),
            Box::new(Term::Sort(Sort::Type(0))),
        ),
    );
    monad.kind = BinderKind::Implicit;
    let instance = Binder {
        name: None,
        ty: Box::new(app("Monad", vec![r("m")])),
        kind: BinderKind::Instance,
    };
    let lift = def(
        "lift",
        vec![monad, instance, b("action", Term::app(r("m"), [r("Nat")]))],
        Term::app(r("m"), [r("Nat")]),
        Term::Do {
            statements: vec![DoStatement::Bind {
                pattern: Pattern::Bind(id("value")),
                action: r("action"),
            }],
            result: Box::new(app("pure", vec![r("value")])),
        },
    );
    let call = Term::App {
        function: Box::new(r("lift")),
        arguments: vec![
            Argument::Named {
                name: id("m"),
                value: r("Id"),
            },
            Argument::Positional(n(42)),
        ],
    };
    lean(module(vec![Declaration::Namespace {
        name: name("Generated"),
        declarations: vec![
            lift,
            check_eq(app("Id.run", vec![call]), n(42)),
            def(
                "Nat",
                vec![],
                Term::Sort(Sort::Type(0)),
                Term::Ref(name("Nat").rooted()),
            ),
            def("usesRoot", vec![], Term::Ref(name("Nat").rooted()), n(7)),
        ],
    }]));
}

#[test]
fn finite_operator_vocabulary_has_lean_checked_meanings() {
    let propositions = vec![
        Term::binary(BinaryOp::Ne, n(1), n(2)),
        Term::binary(BinaryOp::Lt, n(1), n(2)),
        Term::binary(BinaryOp::Le, n(1), n(1)),
        Term::binary(BinaryOp::Gt, n(2), n(1)),
        Term::binary(BinaryOp::Ge, n(1), n(1)),
        Term::binary(BinaryOp::And, eq(n(1), n(1)), eq(n(2), n(2))),
        Term::binary(BinaryOp::Or, eq(n(1), n(2)), eq(n(2), n(2))),
        Term::binary(BinaryOp::Iff, eq(n(1), n(1)), eq(n(2), n(2))),
        Term::Unary {
            op: UnaryOp::Not,
            value: Box::new(eq(n(1), n(2))),
        },
    ];
    let mut declarations = propositions
        .into_iter()
        .map(|proposition| Declaration::Example {
            binders: vec![],
            proposition,
            proof: Term::ByDecide,
        })
        .collect::<Vec<_>>();
    for (op, expected) in [(BinaryOp::Sub, 4), (BinaryOp::Div, 3), (BinaryOp::Mod, 1)] {
        let (left, right) = if op == BinaryOp::Sub { (7, 3) } else { (10, 3) };
        declarations.push(check_eq(Term::binary(op, n(left), n(right)), n(expected)));
    }
    for (op, left, right, expected) in [
        (BinaryOp::BoolEq, true, true, true),
        (BinaryOp::BoolNe, true, false, true),
        (BinaryOp::BoolAnd, true, false, false),
        (BinaryOp::BoolOr, false, true, true),
    ] {
        declarations.push(check_eq(
            Term::binary(op, Term::Bool(left), Term::Bool(right)),
            Term::Bool(expected),
        ));
    }
    declarations.push(Declaration::Check(Term::Sort(Sort::Prop)));
    lean(module(declarations));
}

#[test]
fn scalar_literal_patterns_and_abbreviations_compile() {
    let classify = def(
        "classify",
        vec![b("n", r("Nat"))],
        r("Nat"),
        Term::Match {
            scrutinee: Box::new(r("n")),
            arms: vec![
                MatchArm {
                    pattern: Pattern::Nat(0u64.into()),
                    body: n(7),
                },
                MatchArm {
                    pattern: Pattern::Wildcard,
                    body: n(9),
                },
            ],
        },
    );
    let bool_case = Term::Match {
        scrutinee: Box::new(Term::Bool(false)),
        arms: vec![
            MatchArm {
                pattern: Pattern::Bool(true),
                body: n(1),
            },
            MatchArm {
                pattern: Pattern::Bool(false),
                body: n(2),
            },
        ],
    };
    let string_case = Term::Match {
        scrutinee: Box::new(Term::String("x".into())),
        arms: vec![
            MatchArm {
                pattern: Pattern::String("x".into()),
                body: n(3),
            },
            MatchArm {
                pattern: Pattern::Wildcard,
                body: n(4),
            },
        ],
    };
    let char_case = Term::Match {
        scrutinee: Box::new(Term::Char('\'')),
        arms: vec![
            MatchArm {
                pattern: Pattern::Char('\''),
                body: n(5),
            },
            MatchArm {
                pattern: Pattern::Wildcard,
                body: n(6),
            },
        ],
    };
    lean(module(vec![
        classify,
        check_eq(app("classify", vec![n(0)]), n(7)),
        check_eq(app("classify", vec![n(9)]), n(9)),
        check_eq(bool_case, n(2)),
        check_eq(string_case, n(3)),
        check_eq(char_case, n(5)),
        Declaration::Definition(Definition {
            name: name("Count"),
            binders: vec![],
            result: Term::Sort(Sort::Type(0)),
            body: r("Nat"),
            kind: DefinitionKind::Abbrev,
        }),
    ]));
}

#[test]
fn references_and_simp_definitions() {
    let body = Term::Do {
        statements: vec![DoStatement::Bind {
            pattern: Pattern::Bind(id("x")),
            action: app("step", vec![n(1)]),
        }],
        result: Box::new(Term::Match {
            scrutinee: Box::new(r("x")),
            arms: vec![MatchArm {
                pattern: Pattern::Wildcard,
                body: app("Pure.pure", vec![app("finish", vec![r("x")])]),
            }],
        }),
    };
    let refs = body.references();
    for expected in ["step", "finish", "x", "Pure.pure"] {
        assert!(refs.contains(&name(expected)), "{expected}");
    }
    assert!(!refs.contains(&name("Nat")));
    lean(module(vec![
        Declaration::Definition(Definition {
            name: name("twice"),
            binders: vec![b("k", r("Nat"))],
            result: r("Nat"),
            body: Term::binary(BinaryOp::Add, r("k"), r("k")),
            kind: DefinitionKind::SimpDef,
        }),
        Declaration::Theorem {
            name: name("twice_two"),
            binders: vec![],
            proposition: eq(app("twice", vec![n(2)]), n(4)),
            proof: Term::ByDecide,
        },
    ]));
}
