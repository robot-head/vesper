use vesper_lint::policy::*;
fn parse(source: &str) -> Result<Policy, PolicyError> {
    Policy::parse(source, PolicyLimits::default())
}
fn rule(name: &str, severity: Severity) -> Rule {
    Rule {
        name: name.into(),
        severity,
    }
}
#[test]
fn defaults_and_actual_documented_toml_rules() {
    let p = parse("").unwrap();
    assert_eq!(
        p.severity(LintName::UnusedEffectCapability),
        Some(Severity::Warn)
    );
    assert_eq!(p.rules().len(), 2);
    let p = parse(include_str!("../examples/Loom.lint.toml")).unwrap();
    assert_eq!(
        p.severity(LintName::UnusedEffectCapability),
        Some(Severity::Deny)
    );
    assert_eq!(p.unavailable().len(), 3);
    assert!(p.unavailable().iter().all(|r| !r.available && r.configured));
    let p=parse("lints.vesper.unused_effect_capability = 'allow'\n[package]\nname='unrelated'\n[dependencies]\na={version='1',features=['x','y']}\n[[targets]]\nname='one'\n[[targets]]\nname='two'").unwrap();
    assert_eq!(
        p.severity(LintName::UnusedEffectCapability),
        Some(Severity::Allow)
    );
}
#[test]
fn strict_unknown_types_duplicates_and_plugins() {
    for source in [
        "[lints.vesper]\nunknown='warn'",
        "[lints.unknown]\na=1",
        "lints=1",
        "[lints]\nvesper=[]",
        "[lints.vesper]\nunused_effect_capability=true",
        "[lints.vesper]\nunused_effect_capability='WARN'",
        "[lints.vesper]\nunused_effect_capability='warn'\nunused_effect_capability='deny'",
        "[lints.vesper]\n'overbroad_inout_projection'='allow'\n\"overbroad_inout_projectio\\u006e\"='warn'",
        "[lints.plugins]\n'@vesper-game/ignis-lints'={version='1',fuel_limit=5000000}",
        "[lints]\nplugins=1",
    ] {
        assert!(parse(source).is_err(), "{source}");
    }
    assert_eq!(
        parse("[lints.vesper]\ndemote_alloc_to_region='deny'")
            .unwrap_err()
            .kind,
        ErrorKind::UnavailableDeny
    );
    parse("[lints.plugins]").unwrap();
    let p=parse("[lints.vesper]\nmissed_fip_opportunity='allow'\nearly_cancel_checkpoint='warn'\nunredacted_pii_boundary='allow'").unwrap();
    assert_eq!(p.unavailable().len(), 3);
}
#[test]
fn strings_comments_quoted_keys_and_multiline_endings_are_not_structure() {
    let source = r#####"# [{ . ignored comment
notes = """fake [ [[{{ a.b.c.d ] } \" quote
# still string
"""
literal = '''[ { " . # literal
 ] . }'''
escaped = "quotes \" [ { . # stay in string"
["lints"."vesper"]
"unused_effect_capability" = "warn"
"overbroad_inout_projection" = 'allow'
"#####;
    let p = parse(source).unwrap();
    assert_eq!(
        p.severity(LintName::OverbroadInoutProjection),
        Some(Severity::Allow)
    );
    for ending in ["\"\"\"", "\"\"\"\"", "\"\"\"\"\""] {
        parse(&format!(
            "text=\"\"\"words{ending}\n[lints.vesper]\nunused_effect_capability='warn'\n"
        ))
        .unwrap();
    }
    for ending in ["'''", "''''", "'''''"] {
        parse(&format!(
            "text='''words{ending}\n[lints.vesper]\nunused_effect_capability='warn'\n"
        ))
        .unwrap();
    }
}
#[test]
fn preparse_bounds_cover_nested_containers_and_decoded_dotted_paths() {
    for source in [
        format!("x={}1{}", "[".repeat(80), "]".repeat(80)),
        format!("{}x=1", "a.".repeat(80)),
        format!("[{}x]\ny=1", "a.".repeat(80)),
        format!("[[{}x]]\ny=1", "a.".repeat(80)),
        format!("x={{{}y=1}}", "a.".repeat(80)),
    ] {
        assert_eq!(
            parse(&source).unwrap_err().kind,
            ErrorKind::Limit,
            "{source}"
        );
    }
    // Each quoted string is a single key, even if it contains many periods.
    parse(
        "['a.b.c.d']\n'key.with.dots'={value=1}\n[lints.vesper]\nunused_effect_capability='warn'",
    )
    .unwrap();
    let source = "x=[\n[\n[1]\n]\n]\n";
    parse(source).unwrap();
    let mut source = String::new();
    for n in 1..20 {
        source.push_str(&format!("[[{}]]\n", vec!["x"; n].join(".")));
    }
    assert_eq!(parse(&source).unwrap_err().kind, ErrorKind::Limit);
    assert_eq!(
        parse(&" ".repeat(65537)).unwrap_err().kind,
        ErrorKind::Limit
    );
    assert!(
        Policy::parse(
            "x=1",
            PolicyLimits {
                work: 1,
                ..PolicyLimits::default()
            }
        )
        .is_err()
    );
    assert!(
        Policy::parse(
            "x=1",
            PolicyLimits {
                tokens: 1,
                ..PolicyLimits::default()
            }
        )
        .is_err()
    );
    assert!(
        Policy::parse(
            "x=1",
            PolicyLimits {
                nodes: 1,
                ..PolicyLimits::default()
            }
        )
        .is_err()
    );
}
#[test]
fn direct_typed_inputs_validate_entire_tree_before_policy_extraction() {
    let limits = PolicyLimits::default();
    let mut deep = toml::Value::Integer(0);
    for _ in 0..80 {
        deep = toml::Value::Array(vec![deep]);
    }
    assert_eq!(
        Policy::from_value(&deep, limits).unwrap_err().kind,
        ErrorKind::Limit
    );
    let mut document = toml::map::Map::new();
    document.insert("unrelated".into(), deep);
    assert_eq!(
        Policy::from_value(&toml::Value::Table(document), limits)
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    let wide = toml::Value::Array(vec![toml::Value::Integer(0); 5000]);
    assert_eq!(
        Policy::from_value(&wide, limits).unwrap_err().kind,
        ErrorKind::Limit
    );
    assert_eq!(wide.as_array().unwrap().len(), 5000);
    assert_eq!(
        Policy::from_value(&toml::Value::String("x".repeat(65537)), limits)
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    assert_eq!(
        Policy::from_rules(
            &[
                rule("unused_effect_capability", Severity::Warn),
                rule("unused_effect_capability", Severity::Deny)
            ],
            limits
        )
        .unwrap_err()
        .kind,
        ErrorKind::DuplicateRule
    );
    assert_eq!(
        Policy::from_rules(&[rule("not_a_lint", Severity::Warn)], limits)
            .unwrap_err()
            .kind,
        ErrorKind::UnknownRule
    );
    assert_eq!(
        Policy::from_rules(&[rule("demote_alloc_to_region", Severity::Deny)], limits)
            .unwrap_err()
            .kind,
        ErrorKind::UnavailableDeny
    );
    assert_eq!(
        Policy::from_rules(&[rule(&"x".repeat(129), Severity::Allow)], limits)
            .unwrap_err()
            .kind,
        ErrorKind::Limit
    );
    assert!(
        Policy::from_rules(
            &[],
            PolicyLimits {
                work: usize::MAX,
                ..limits
            }
        )
        .is_err()
    );
    assert_eq!(
        Policy::from_rules(
            &[rule("unused_effect_capability", Severity::Warn)],
            PolicyLimits { nodes: 1, ..limits }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
}

#[test]
fn preparse_and_typed_limits_reject_all_deep_unrelated_paths() {
    let source = "[lints.vesper]\nunused_effect_capability='warn'\n[unrelated]\na.b.c={d.e.f={g.h.i={j.k.l={m.n.o={value=0}}}}}";
    assert_eq!(parse(source).unwrap_err().kind, ErrorKind::Limit);
    let mut document = toml::map::Map::new();
    let mut lints = toml::map::Map::new();
    let mut vesper = toml::map::Map::new();
    vesper.insert(
        "unused_effect_capability".into(),
        toml::Value::String("warn".into()),
    );
    lints.insert("vesper".into(), toml::Value::Table(vesper));
    document.insert("lints".into(), toml::Value::Table(lints));
    document.insert("ignored".into(), toml::Value::String("x".repeat(4000)));
    let value = toml::Value::Table(document);
    assert_eq!(
        Policy::from_value(
            &value,
            PolicyLimits {
                bytes: 100,
                ..PolicyLimits::default()
            }
        )
        .unwrap_err()
        .kind,
        ErrorKind::Limit
    );
    assert!(
        Policy::from_value(
            &value,
            PolicyLimits {
                work: 1,
                ..PolicyLimits::default()
            }
        )
        .is_err()
    );
    let good = Policy::parse(
        "[package]\nname='coexist'\n[lints.vesper]\nunused_effect_capability='allow'",
        PolicyLimits::default(),
    )
    .unwrap();
    assert_eq!(
        good.usage().input_bytes,
        Some("[package]\nname='coexist'\n[lints.vesper]\nunused_effect_capability='allow'".len())
    );
    assert!(good.usage().decoded_bytes > 0);
}
