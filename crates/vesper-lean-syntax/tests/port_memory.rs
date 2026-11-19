//! The port's runtime frees everything each call allocates, on success and
//! on every error path. Its own test binary, since the allocation counter is
//! shared by every thread.
#![cfg(vesper_port)]
use vesper_lean_syntax::*;

fn x() -> Term {
    Term::Ref(Name::single("x").unwrap())
}

fn one_tuple() -> Term {
    Term::Tuple(vec![x()])
}

#[test]
fn port_calls_do_not_leak() {
    let before = port_live_allocations();
    let binder = Binder::explicit(Ident::new("x").unwrap(), x());
    let terms = [
        x(),
        Term::Nat(NatLiteral::from(12345u64)),
        Term::Sort(Sort::Type(3)),
        Term::String("a\nb\u{1}".into()),
        Term::app(x(), [x()]),
        // Errors: in a node, in an application's function, and in both its
        // function and an argument.
        one_tuple(),
        Term::app(one_tuple(), [x()]),
        Term::app(one_tuple(), [Term::ProductType(vec![])]),
        Term::Match {
            scrutinee: Box::new(x()),
            arms: vec![MatchArm {
                pattern: Pattern::Int(i128::MIN),
                body: x(),
            }],
        },
        Term::Lambda {
            binders: vec![binder.clone()],
            body: Box::new(x()),
        },
    ];
    for _ in 0..3 {
        for t in &terms {
            let _ = t.render();
            let _ = t.references();
            let _ = Term::free_references(std::slice::from_ref(&binder), t);
        }
        let module = Module {
            imports: vec![ModuleName::new("Init").unwrap()],
            declarations: vec![
                Declaration::Eval(x()),
                Declaration::MutualDefinitions(vec![]),
            ],
        };
        assert!(module.render().is_err());
        assert!(Module::default().render().is_ok());
    }
    assert_eq!(port_live_allocations(), before);
}
