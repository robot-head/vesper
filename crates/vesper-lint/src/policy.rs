//! Bounded Loom.toml lint policy. No plugin execution or format configuration.
use crate::FindingKind;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Allow,
    Warn,
    Deny,
}
impl Severity {
    fn parse(value: &str) -> Result<Self, PolicyError> {
        match value {
            "allow" => Ok(Self::Allow),
            "warn" => Ok(Self::Warn),
            "deny" => Ok(Self::Deny),
            _ => Err(error(
                ErrorKind::Severity,
                "severity must be exactly allow, warn, or deny",
            )),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LintName {
    OverbroadInoutProjection,
    UnusedEffectCapability,
    DemoteAllocToRegion,
    OverlyRestrictiveMode,
    MissedFipOpportunity,
    EarlyCancelCheckpoint,
    UnredactedPiiBoundary,
}
impl LintName {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::OverbroadInoutProjection => "overbroad_inout_projection",
            Self::UnusedEffectCapability => "unused_effect_capability",
            Self::DemoteAllocToRegion => "demote_alloc_to_region",
            Self::OverlyRestrictiveMode => "overly_restrictive_mode",
            Self::MissedFipOpportunity => "missed_fip_opportunity",
            Self::EarlyCancelCheckpoint => "early_cancel_checkpoint",
            Self::UnredactedPiiBoundary => "unredacted_pii_boundary",
        }
    }
    pub fn available(self) -> bool {
        matches!(
            self,
            Self::OverbroadInoutProjection | Self::UnusedEffectCapability
        )
    }
    fn parse(name: &str) -> Result<Self, PolicyError> {
        Ok(match name {
            "overbroad_inout_projection" => Self::OverbroadInoutProjection,
            "unused_effect_capability" => Self::UnusedEffectCapability,
            "demote_alloc_to_region" => Self::DemoteAllocToRegion,
            "overly_restrictive_mode" => Self::OverlyRestrictiveMode,
            "missed_fip_opportunity" => Self::MissedFipOpportunity,
            "early_cancel_checkpoint" => Self::EarlyCancelCheckpoint,
            "unredacted_pii_boundary" => Self::UnredactedPiiBoundary,
            _ => return Err(error(ErrorKind::UnknownRule, "unknown Vesper lint name")),
        })
    }
}
#[derive(Clone, Debug)]
pub struct Rule {
    pub name: String,
    pub severity: Severity,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct RuleStatus {
    pub lint: LintName,
    pub severity: Severity,
    pub configured: bool,
    pub available: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct PolicyLimits {
    pub bytes: usize,
    pub depth: usize,
    pub tokens: usize,
    pub nodes: usize,
    pub work: usize,
}
impl Default for PolicyLimits {
    fn default() -> Self {
        Self {
            bytes: 65536,
            depth: 32,
            tokens: 8192,
            nodes: 4096,
            work: 262144,
        }
    }
}
impl PolicyLimits {
    fn validate(self) -> Result<(), PolicyError> {
        let hard = Self::default();
        if self.bytes == 0
            || self.bytes > hard.bytes
            || self.depth == 0
            || self.depth > hard.depth
            || self.tokens == 0
            || self.tokens > hard.tokens
            || self.nodes == 0
            || self.nodes > hard.nodes
            || self.work == 0
            || self.work > hard.work
        {
            Err(error(ErrorKind::Limit, "invalid policy limits"))
        } else {
            Ok(())
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    Limit,
    Syntax,
    Shape,
    UnknownRule,
    UnknownSection,
    Severity,
    DuplicateRule,
    UnavailableDeny,
    Plugins,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PolicyError {
    pub kind: ErrorKind,
    pub message: &'static str,
}
fn error(kind: ErrorKind, message: &'static str) -> PolicyError {
    PolicyError { kind, message }
}
impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "policy {:?}: {}", self.kind, self.message)
    }
}
impl std::error::Error for PolicyError {}
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct PolicyUsage {
    pub input_bytes: Option<usize>,
    pub decoded_bytes: usize,
    pub tokens: usize,
    pub nodes: usize,
    pub work: usize,
}
#[derive(Clone, Debug)]
pub struct Policy {
    levels: BTreeMap<LintName, Severity>,
    configured: BTreeSet<LintName>,
    usage: PolicyUsage,
}
impl Default for Policy {
    fn default() -> Self {
        Self {
            levels: BTreeMap::from([
                (LintName::OverbroadInoutProjection, Severity::Warn),
                (LintName::UnusedEffectCapability, Severity::Warn),
            ]),
            configured: BTreeSet::new(),
            usage: PolicyUsage::default(),
        }
    }
}
impl Policy {
    /// Parse the complete TOML document only after lexical/shape/work preflight.
    /// Duplicate decoded keys are rejected by the actual TOML parser.
    pub fn parse(source: &str, limits: PolicyLimits) -> Result<Self, PolicyError> {
        limits.validate()?;
        let mut budget = Budget::new(limits);
        let tokens = preflight(source, &mut budget)?;
        let document: toml::Value = toml::from_str(source)
            .map_err(|_| error(ErrorKind::Syntax, "invalid TOML syntax or duplicate key"))?;
        validate_value(&document, &mut budget)?;
        let mut policy = Self::from_validated_value(&document, &mut budget)?;
        policy.usage = PolicyUsage {
            input_bytes: Some(source.len()),
            decoded_bytes: budget.value_bytes,
            tokens,
            nodes: budget.nodes,
            work: budget.work,
        };
        Ok(policy)
    }
    /// Borrowed typed TOML is fully bounded before any policy extraction/copy.
    /// The caller owns allocations made before entering this API.
    pub fn from_value(document: &toml::Value, limits: PolicyLimits) -> Result<Self, PolicyError> {
        limits.validate()?;
        let mut budget = Budget::new(limits);
        validate_value(document, &mut budget)?;
        let mut policy = Self::from_validated_value(document, &mut budget)?;
        policy.usage = PolicyUsage {
            input_bytes: None,
            decoded_bytes: budget.value_bytes,
            tokens: 0,
            nodes: budget.nodes,
            work: budget.work,
        };
        Ok(policy)
    }
    /// Direct typed entry point with duplicate/name/unavailable-deny checks.
    pub fn from_rules(rules: &[Rule], limits: PolicyLimits) -> Result<Self, PolicyError> {
        limits.validate()?;
        if rules.len() > 7 || rules.len() + 1 > limits.nodes {
            return Err(error(ErrorKind::Limit, "policy rule count limit"));
        }
        let mut policy = Self::default();
        let mut budget = Budget::new(limits);
        budget.nodes = rules.len() + 1;
        budget.charge(1)?;
        for rule in rules {
            budget.text(rule.name.len())?;
            budget.charge(1)?;
            policy.insert(&rule.name, rule.severity)?;
        }
        policy.usage = PolicyUsage {
            input_bytes: None,
            decoded_bytes: budget.value_bytes,
            tokens: 0,
            nodes: budget.nodes,
            work: budget.work,
        };
        Ok(policy)
    }
    pub fn severity(&self, name: LintName) -> Option<Severity> {
        self.levels.get(&name).copied()
    }
    pub fn severity_for(&self, finding: &FindingKind) -> Severity {
        let lint = match finding {
            FindingKind::UnusedEffectCapability { .. } => LintName::UnusedEffectCapability,
            FindingKind::OverbroadInoutProjection { .. } => LintName::OverbroadInoutProjection,
        };
        self.levels[&lint]
    }
    pub fn rules(&self) -> Vec<RuleStatus> {
        self.levels
            .iter()
            .map(|(&lint, &severity)| RuleStatus {
                lint,
                severity,
                configured: self.configured.contains(&lint),
                available: lint.available(),
            })
            .collect()
    }
    pub fn unavailable(&self) -> Vec<RuleStatus> {
        self.rules().into_iter().filter(|r| !r.available).collect()
    }
    pub fn usage(&self) -> PolicyUsage {
        self.usage
    }
    fn insert(&mut self, name: &str, severity: Severity) -> Result<(), PolicyError> {
        if name.len() > 128 {
            return Err(error(ErrorKind::Limit, "policy key byte limit"));
        }
        let lint = LintName::parse(name)?;
        if !self.configured.insert(lint) {
            return Err(error(
                ErrorKind::DuplicateRule,
                "duplicate lint policy rule",
            ));
        }
        if !lint.available() && severity == Severity::Deny {
            return Err(error(
                ErrorKind::UnavailableDeny,
                "requested deny lint is not implemented",
            ));
        }
        self.levels.insert(lint, severity);
        Ok(())
    }
    fn from_validated_value(
        document: &toml::Value,
        budget: &mut Budget,
    ) -> Result<Self, PolicyError> {
        let table = document
            .as_table()
            .ok_or(error(ErrorKind::Shape, "Loom document must be a table"))?;
        let mut policy = Self::default();
        let Some(lints) = table.get("lints") else {
            return Ok(policy);
        };
        let lints = lints
            .as_table()
            .ok_or(error(ErrorKind::Shape, "lints must be a table"))?;
        for (section, value) in lints {
            budget.charge(1)?;
            match section.as_str() {
                "vesper" => {
                    let rules = value
                        .as_table()
                        .ok_or(error(ErrorKind::Shape, "lints.vesper must be a table"))?;
                    if rules.len() > 7 {
                        return Err(error(ErrorKind::Limit, "policy rule count limit"));
                    }
                    for (name, value) in rules {
                        budget.charge(1)?;
                        let severity = value
                            .as_str()
                            .ok_or(error(ErrorKind::Severity, "lint severity must be a string"))?;
                        policy.insert(name, Severity::parse(severity)?)?;
                    }
                }
                "plugins" => {
                    let plugins = value
                        .as_table()
                        .ok_or(error(ErrorKind::Shape, "lints.plugins must be a table"))?;
                    if !plugins.is_empty() {
                        return Err(error(
                            ErrorKind::Plugins,
                            "lint plugin packs are unsupported; no pack was loaded",
                        ));
                    }
                }
                _ => {
                    return Err(error(
                        ErrorKind::UnknownSection,
                        "unknown table under lints",
                    ));
                }
            }
        }
        Ok(policy)
    }
}
struct Budget {
    limits: PolicyLimits,
    work: usize,
    nodes: usize,
    value_bytes: usize,
}
impl Budget {
    fn new(limits: PolicyLimits) -> Self {
        Self {
            limits,
            work: 0,
            nodes: 0,
            value_bytes: 0,
        }
    }
    fn charge(&mut self, n: usize) -> Result<(), PolicyError> {
        self.work = self
            .work
            .checked_add(n)
            .ok_or(error(ErrorKind::Limit, "policy work overflow"))?;
        if self.work > self.limits.work {
            return Err(error(ErrorKind::Limit, "policy work limit"));
        }
        Ok(())
    }
    fn text(&mut self, n: usize) -> Result<(), PolicyError> {
        self.value_bytes = self
            .value_bytes
            .checked_add(n)
            .ok_or(error(ErrorKind::Limit, "policy byte overflow"))?;
        if self.value_bytes > self.limits.bytes {
            return Err(error(ErrorKind::Limit, "policy decoded byte limit"));
        }
        self.charge(n)
    }
}
fn validate_value(document: &toml::Value, budget: &mut Budget) -> Result<(), PolicyError> {
    let mut stack = vec![(document, 0usize)];
    let mut enqueued = 1usize;
    while let Some((value, depth)) = stack.pop() {
        budget.charge(1)?;
        budget.nodes += 1;
        if depth > budget.limits.depth || budget.nodes > budget.limits.nodes {
            return Err(error(ErrorKind::Limit, "policy value depth/node limit"));
        }
        match value {
            toml::Value::String(s) => budget.text(s.len())?,
            toml::Value::Integer(_)
            | toml::Value::Float(_)
            | toml::Value::Boolean(_)
            | toml::Value::Datetime(_) => budget.text(8)?,
            toml::Value::Array(array) => {
                enqueued = enqueued
                    .checked_add(array.len())
                    .ok_or(error(ErrorKind::Limit, "policy tree overflow"))?;
                if enqueued > budget.limits.nodes {
                    return Err(error(ErrorKind::Limit, "policy node limit"));
                }
                budget.charge(array.len())?;
                stack.extend(array.iter().map(|v| (v, depth + 1)));
            }
            toml::Value::Table(table) => {
                enqueued = enqueued
                    .checked_add(table.len())
                    .ok_or(error(ErrorKind::Limit, "policy tree overflow"))?;
                if enqueued > budget.limits.nodes {
                    return Err(error(ErrorKind::Limit, "policy node limit"));
                }
                budget.charge(table.len())?;
                for (key, v) in table {
                    if key.len() > 128 {
                        return Err(error(ErrorKind::Limit, "TOML key byte limit"));
                    }
                    budget.text(key.len())?;
                    stack.push((v, depth + 1));
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Atom,
    String,
    Dot,
    Eq,
    LeftArray,
    RightArray,
    LeftTable,
    RightTable,
    Comma,
    Newline,
}
#[derive(Clone, Copy)]
struct Token {
    kind: Kind,
}
fn lexical(source: &str, budget: &mut Budget) -> Result<Vec<Token>, PolicyError> {
    if source.len() > budget.limits.bytes {
        return Err(error(ErrorKind::Limit, "policy input byte limit"));
    }
    budget.charge(source.len())?;
    let bytes = source.as_bytes();
    let mut i = 0;
    let mut out = vec![];
    let mut nodes = 1usize;
    while i < bytes.len() {
        let kind = match bytes[i] {
            b' ' | b'\t' => {
                i += 1;
                continue;
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' && bytes[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            b'\n' | b'\r' => {
                if bytes[i] == b'\r' && bytes.get(i + 1) == Some(&b'\n') {
                    i += 1;
                }
                i += 1;
                Kind::Newline
            }
            b'\'' | b'"' => {
                i = string_end(bytes, i)?;
                Kind::String
            }
            byte => {
                i += 1;
                match byte {
                    b'.' => Kind::Dot,
                    b'=' => Kind::Eq,
                    b'[' => Kind::LeftArray,
                    b']' => Kind::RightArray,
                    b'{' => Kind::LeftTable,
                    b'}' => Kind::RightTable,
                    b',' => Kind::Comma,
                    _ => {
                        while i < bytes.len()
                            && !matches!(
                                bytes[i],
                                b' ' | b'\t'
                                    | b'\r'
                                    | b'\n'
                                    | b'#'
                                    | b'\''
                                    | b'"'
                                    | b'.'
                                    | b'='
                                    | b'['
                                    | b']'
                                    | b'{'
                                    | b'}'
                                    | b','
                            )
                        {
                            i += 1;
                        }
                        Kind::Atom
                    }
                }
            }
        };
        if out.len() >= budget.limits.tokens {
            return Err(error(ErrorKind::Limit, "policy token limit"));
        }
        if matches!(
            kind,
            Kind::Atom | Kind::String | Kind::LeftArray | Kind::LeftTable
        ) {
            nodes = nodes
                .checked_add(1)
                .ok_or(error(ErrorKind::Limit, "policy node estimate overflow"))?;
            if nodes > budget.limits.nodes {
                return Err(error(
                    ErrorKind::Limit,
                    "policy lexical tree estimate limit",
                ));
            }
        }
        out.push(Token { kind });
    }
    Ok(out)
}
fn string_end(bytes: &[u8], start: usize) -> Result<usize, PolicyError> {
    let quote = bytes[start];
    let multiline = bytes
        .get(start..start + 3)
        .is_some_and(|s| s == [quote, quote, quote]);
    let mut i = start + if multiline { 3 } else { 1 };
    while i < bytes.len() {
        if quote == b'"' && bytes[i] == b'\\' {
            i = i
                .checked_add(2)
                .ok_or(error(ErrorKind::Syntax, "invalid string escape"))?;
            continue;
        }
        if bytes[i] == quote {
            if !multiline {
                return Ok(i + 1);
            }
            let mut end = i;
            while end < bytes.len() && bytes[end] == quote {
                end += 1;
            }
            let run = end - i;
            if run >= 3 {
                if run > 5 {
                    return Err(error(ErrorKind::Syntax, "invalid multiline quote run"));
                }
                return Ok(end);
            }
            i = end;
        } else {
            i += 1;
        }
    }
    Err(error(ErrorKind::Syntax, "unterminated TOML string"))
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    Array,
    Table,
}
#[derive(Clone, Copy)]
struct Frame {
    kind: Container,
    depth: usize,
    key: usize,
    expect_key: bool,
    value: bool,
    value_depth: usize,
}
fn check_depth(depth: usize, budget: &Budget) -> Result<(), PolicyError> {
    if depth > budget.limits.depth {
        Err(error(ErrorKind::Limit, "policy preparse tree depth limit"))
    } else {
        Ok(())
    }
}
fn header(
    tokens: &[Token],
    mut i: usize,
    budget: &mut Budget,
) -> Result<(usize, usize), PolicyError> {
    let array = tokens.get(i + 1).is_some_and(|t| t.kind == Kind::LeftArray);
    i += if array { 2 } else { 1 };
    let mut parts = 0usize;
    let mut expect = true;
    while let Some(token) = tokens.get(i) {
        budget.charge(1)?;
        match token.kind {
            Kind::Atom | Kind::String if expect => {
                parts += 1;
                expect = false;
                check_depth(parts * 2, budget)?;
            }
            Kind::Dot if !expect => expect = true,
            Kind::RightArray if !expect => {
                if array {
                    if tokens.get(i + 1).is_none_or(|t| t.kind != Kind::RightArray) {
                        return Err(error(ErrorKind::Syntax, "malformed array-of-table header"));
                    }
                    budget.charge(1)?;
                    i += 1;
                }
                return Ok((i + 1, parts * 2));
            }
            _ => return Err(error(ErrorKind::Syntax, "malformed TOML table header")),
        }
        i += 1;
    }
    Err(error(ErrorKind::Syntax, "unterminated TOML table header"))
}
fn preflight(source: &str, budget: &mut Budget) -> Result<usize, PolicyError> {
    let tokens = lexical(source, budget)?;
    let mut i = 0;
    let mut root = Frame {
        kind: Container::Table,
        depth: 0,
        key: 0,
        expect_key: true,
        value: false,
        value_depth: 0,
    };
    let mut stack: Vec<Frame> = vec![];
    let mut line_start = true;
    while i < tokens.len() {
        budget.charge(1)?;
        let kind = tokens[i].kind;
        if kind == Kind::Newline {
            if stack.is_empty() {
                root.key = 0;
                root.expect_key = true;
                root.value = false;
                line_start = true;
            }
            i += 1;
            continue;
        }
        if line_start && stack.is_empty() && kind == Kind::LeftArray && !root.value && root.key == 0
        {
            let (next, depth) = header(&tokens, i, budget)?;
            i = next;
            root.depth = depth;
            line_start = false;
            continue;
        }
        line_start = false;
        let nested = !stack.is_empty();
        let frame = stack.last_mut().unwrap_or(&mut root);
        if !frame.value {
            match kind {
                Kind::Atom | Kind::String if frame.expect_key => {
                    frame.key += 1;
                    frame.expect_key = false;
                    check_depth(frame.depth + frame.key * 2, budget)?;
                }
                Kind::Dot if !frame.expect_key => frame.expect_key = true,
                Kind::Eq if !frame.expect_key => {
                    frame.value = true;
                    frame.value_depth = frame.depth + frame.key * 2;
                }
                Kind::RightTable if frame.kind == Container::Table && frame.key == 0 && nested => {
                    stack.pop();
                }
                _ => return Err(error(ErrorKind::Syntax, "malformed TOML key")),
            }
            i += 1;
            continue;
        }
        match kind {
            Kind::LeftArray | Kind::LeftTable => {
                let base = if frame.kind == Container::Array {
                    frame.depth
                } else {
                    frame.value_depth
                };
                let depth = base + 1;
                check_depth(depth, budget)?;
                stack.push(Frame {
                    kind: if kind == Kind::LeftArray {
                        Container::Array
                    } else {
                        Container::Table
                    },
                    depth,
                    key: 0,
                    expect_key: true,
                    value: kind == Kind::LeftArray,
                    value_depth: depth,
                });
            }
            Kind::RightArray | Kind::RightTable => {
                let expected = if kind == Kind::RightArray {
                    Container::Array
                } else {
                    Container::Table
                };
                if stack.last().is_none_or(|f| f.kind != expected) {
                    return Err(error(ErrorKind::Syntax, "mismatched TOML container"));
                }
                stack.pop();
            }
            Kind::Comma => {
                if frame.kind == Container::Table && nested {
                    frame.key = 0;
                    frame.expect_key = true;
                    frame.value = false;
                } else if !nested {
                    return Err(error(ErrorKind::Syntax, "comma outside TOML container"));
                }
            }
            _ => (),
        }
        i += 1;
    }
    if !stack.is_empty() {
        return Err(error(ErrorKind::Syntax, "unclosed TOML container"));
    }
    Ok(tokens.len())
}
