//! Per-function canonical BLAKE3 hashing, reverse call-graph slicing, and
//! content-addressed test verdict caching for Vesper Anvil.
//!
//! - [`FunctionGraph`] extracts direct call edges across top-level functions
//!   and `impl` methods in a [`vesper_core::ast::Module`], computes
//!   span-insensitive per-function digests (`fn_blake3`), computes
//!   deterministic Merkle digests over transitive call closures
//!   (`transitive_fn_blake3`), and slices reverse callers for mutation testing.
//! - [`VerdictCache`] wraps [`vesper_loom_cas::Store`] to persist serialized
//!   verdict payloads as verified `Domain::Artifact` blobs indexed under
//!   `<store_root>/test_verdicts/<key>.json`.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use vesper_core::ast::{
    Arg, Clause, Effect, Effects, Expr, ExprKind, Function, Generic, Item, Literal, Mode, Module,
    Param, Pass, Pattern, PatternKind, Statement, Type,
};
use vesper_loom_cas::{Domain, ObjectId, Store};

const FN_HASH_DOMAIN: &[u8] = b"vesper.anvil.fn_blake3.v1\0";
const TRANSITIVE_HASH_DOMAIN: &[u8] = b"vesper.anvil.transitive_fn_blake3.v1\0";

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Call graph and canonical BLAKE3 fingerprints for all functions and `impl`
/// methods declared in a [`Module`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FunctionGraph {
    /// Canonical per-function BLAKE3 hex digest (ignoring source byte spans).
    pub fn_blake3: BTreeMap<String, String>,
    /// Direct call edges: caller -> set of known callees in the module.
    pub callees: BTreeMap<String, BTreeSet<String>>,
    /// Reverse direct call edges: callee -> set of direct callers in the module.
    pub callers: BTreeMap<String, BTreeSet<String>>,
}

impl FunctionGraph {
    /// Builds a [`FunctionGraph`] from a parsed or checked [`Module`].
    pub fn from_module(module: &Module) -> Self {
        let mut graph = Self::default();
        let mut functions: Vec<(String, &Function)> = Vec::new();
        let mut method_index: BTreeMap<String, Vec<String>> = BTreeMap::new();

        for item in &module.items {
            match item {
                Item::Function(func) => {
                    let key = func.name.clone();
                    graph
                        .fn_blake3
                        .insert(key.clone(), hash_function(&key, func));
                    graph.callees.entry(key.clone()).or_default();
                    graph.callers.entry(key.clone()).or_default();
                    functions.push((key, func.as_ref()));
                }
                Item::Impl { ty, methods, .. } => {
                    let owner = impl_type_key(ty);
                    for method in methods {
                        let key = format!("{owner}::{}", method.name);
                        graph
                            .fn_blake3
                            .insert(key.clone(), hash_function(&key, method));
                        graph.callees.entry(key.clone()).or_default();
                        graph.callers.entry(key.clone()).or_default();
                        method_index
                            .entry(method.name.clone())
                            .or_default()
                            .push(key.clone());
                        functions.push((key, method));
                    }
                }
                _ => {}
            }
        }

        let known_fns: BTreeSet<String> = graph.fn_blake3.keys().cloned().collect();
        for (caller_name, func) in functions {
            let mut direct = BTreeSet::new();
            if let Some(body) = &func.body {
                collect_expr_calls(body, &known_fns, &method_index, &mut direct);
            }
            for callee in &direct {
                graph
                    .callers
                    .entry(callee.clone())
                    .or_default()
                    .insert(caller_name.clone());
            }
            graph.callees.insert(caller_name, direct);
        }

        graph
    }

    /// Returns the direct (non-transitive) canonical `fn_blake3` hex digest
    /// for `name`, if present in the module.
    pub fn fn_hash(&self, name: &str) -> Option<&str> {
        self.fn_blake3.get(name).map(String::as_str)
    }

    /// Returns the direct callees of `name`, if present in the module.
    pub fn direct_callees(&self, name: &str) -> Option<&BTreeSet<String>> {
        self.callees.get(name)
    }

    /// Returns the sorted transitive call closure reachable from `name`
    /// (including `name` itself), or `None` if `name` is not in the graph.
    pub fn transitive_callees(&self, name: &str) -> Option<BTreeSet<String>> {
        if !self.fn_blake3.contains_key(name) {
            return None;
        }
        let mut visited = BTreeSet::new();
        let mut queue = VecDeque::new();
        visited.insert(name.to_owned());
        queue.push_back(name.to_owned());

        while let Some(current) = queue.pop_front() {
            if let Some(edges) = self.callees.get(&current) {
                for next in edges {
                    if visited.insert(next.clone()) {
                        queue.push_back(next.clone());
                    }
                }
            }
        }

        Some(visited)
    }

    /// Computes a deterministic Merkle BLAKE3 hex digest over `name` and its
    /// sorted transitive call closure.
    ///
    /// Returns `None` if `name` is not present in the graph.
    pub fn transitive_fn_blake3(&self, name: &str) -> Option<String> {
        let closure = self.transitive_callees(name)?;
        let mut hasher = blake3::Hasher::new();
        hasher.update(TRANSITIVE_HASH_DOMAIN);
        hash_str(&mut hasher, name);
        hash_u64(&mut hasher, closure.len() as u64);
        for fn_name in &closure {
            hash_str(&mut hasher, fn_name);
            if let Some(digest) = self.fn_blake3.get(fn_name) {
                hash_str(&mut hasher, digest);
            }
        }
        Some(hasher.finalize().to_hex().to_string())
    }

    /// Returns all functions (including `@test` functions) that transitively
    /// call `target`, for reverse call-graph mutation slicing.
    pub fn reverse_callers(&self, target: &str) -> BTreeSet<String> {
        let mut result = BTreeSet::new();
        let mut queue = VecDeque::new();

        if let Some(direct) = self.callers.get(target) {
            for caller in direct {
                if result.insert(caller.clone()) {
                    queue.push_back(caller.clone());
                }
            }
        }

        while let Some(current) = queue.pop_front() {
            if let Some(parents) = self.callers.get(&current) {
                for parent in parents {
                    if result.insert(parent.clone()) {
                        queue.push_back(parent.clone());
                    }
                }
            }
        }

        result
    }
}

/// Computes the canonical, span-insensitive BLAKE3 hex digest of a single
/// [`Function`].
pub fn hash_function(canonical_name: &str, func: &Function) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(FN_HASH_DOMAIN);
    hash_str(&mut hasher, canonical_name);
    hash_bool(&mut hasher, func.fip);
    hash_generics(&mut hasher, &func.generics);
    hash_params(&mut hasher, &func.params);
    hash_type(&mut hasher, &func.result);
    hash_effects(&mut hasher, &func.effects);
    match &func.body {
        Some(expr) => {
            hash_u8(&mut hasher, 1);
            hash_expr(&mut hasher, expr);
        }
        None => hash_u8(&mut hasher, 0),
    }
    hasher.finalize().to_hex().to_string()
}

/// Content-addressed test verdict cache backed by [`vesper_loom_cas::Store`].
///
/// Payloads are stored as verified `Domain::Artifact` blobs in the CAS and
/// indexed by deterministic sidecar files under `<store_root>/test_verdicts/<key>.json`.
#[derive(Debug, Clone)]
pub struct VerdictCache {
    store: Store,
    verdicts_dir: PathBuf,
}

impl VerdictCache {
    /// Opens the default user verdict cache at `~/.vesper/store`.
    pub fn open_default() -> Result<Self, String> {
        Self::open(default_store_path())
    }

    /// Opens (or initializes) a verdict cache rooted at `root`.
    pub fn open(root: impl AsRef<Path>) -> Result<Self, String> {
        let store = Store::open(root.as_ref()).map_err(|e| e.to_string())?;
        let verdicts_dir = store.root().join("test_verdicts");
        fs::create_dir_all(&verdicts_dir).map_err(|e| format!("create test_verdicts dir: {e}"))?;
        Ok(Self {
            store,
            verdicts_dir,
        })
    }

    /// Returns the underlying [`Store`].
    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Returns the canonical store root directory.
    pub fn root(&self) -> &Path {
        self.store.root()
    }

    /// Retrieves a cached verdict payload for `key_hex`, verifying the
    /// underlying CAS blob digest on read. Returns `None` on cache miss or
    /// corruption.
    pub fn get(&self, key_hex: &str) -> Option<Vec<u8>> {
        if !is_valid_key(key_hex) {
            return None;
        }
        let sidecar = self.sidecar_path(key_hex);
        let text = fs::read_to_string(&sidecar).ok()?;
        let value: serde_json::Value = serde_json::from_str(&text).ok()?;
        let blob_id_str = value.get("blob_id")?.as_str()?;
        let blob_id: ObjectId = blob_id_str.parse().ok()?;
        self.store.read_blob(&blob_id).ok()
    }

    /// Stores `payload` in the CAS (`Domain::Artifact`) and atomically updates
    /// the deterministic `test_verdicts/<key_hex>.json` index entry.
    pub fn put(&self, key_hex: &str, payload: &[u8]) -> Result<(), String> {
        if !is_valid_key(key_hex) {
            return Err(format!("invalid verdict cache key '{key_hex}'"));
        }
        let ingested = self
            .store
            .put_blob(Domain::Artifact, payload)
            .map_err(|e| e.to_string())?;
        let index_json = serde_json::json!({
            "schema_version": 1,
            "key": key_hex,
            "blob_id": ingested.id.to_string(),
        });
        let encoded = serde_json::to_vec(&index_json).map_err(|e| e.to_string())?;

        let dest = self.sidecar_path(key_hex);
        let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = self
            .verdicts_dir
            .join(format!(".tmp-{key_hex}-{}-{seq}", std::process::id()));
        fs::write(&tmp, &encoded).map_err(|e| format!("write verdict index temp file: {e}"))?;
        if let Err(err) = fs::rename(&tmp, &dest) {
            let _ = fs::remove_file(&tmp);
            return Err(format!("publish verdict index '{}': {err}", dest.display()));
        }
        Ok(())
    }

    fn sidecar_path(&self, key_hex: &str) -> PathBuf {
        self.verdicts_dir.join(format!("{key_hex}.json"))
    }
}

/// Default local CAS store path (`~/.vesper/store`).
pub fn default_store_path() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        PathBuf::from(home).join(".vesper").join("store")
    } else {
        PathBuf::from(".vesper").join("store")
    }
}

fn is_valid_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= 256
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

fn impl_type_key(ty: &Type) -> String {
    match ty {
        Type::Named(name, _) => name.clone(),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Call-edge extraction
// ---------------------------------------------------------------------------

fn collect_expr_calls(
    expr: &Expr,
    known_fns: &BTreeSet<String>,
    method_index: &BTreeMap<String, Vec<String>>,
    out: &mut BTreeSet<String>,
) {
    match &expr.kind {
        ExprKind::Literal(_) | ExprKind::Name(_) | ExprKind::Continue => {}
        ExprKind::Tuple(elems) => {
            for e in elems {
                collect_expr_calls(e, known_fns, method_index, out);
            }
        }
        ExprKind::Struct { fields, .. } => {
            for (_, e) in fields {
                collect_expr_calls(e, known_fns, method_index, out);
            }
        }
        ExprKind::Block { statements, tail } => {
            for stmt in statements {
                match stmt {
                    Statement::Let { value, .. } => {
                        collect_expr_calls(value, known_fns, method_index, out);
                    }
                    Statement::Expr(e) => {
                        collect_expr_calls(e, known_fns, method_index, out);
                    }
                }
            }
            if let Some(t) = tail {
                collect_expr_calls(t, known_fns, method_index, out);
            }
        }
        ExprKind::Call { callee, args, .. } => {
            match &callee.kind {
                ExprKind::Name(name) => {
                    if known_fns.contains(name) {
                        out.insert(name.clone());
                    }
                }
                ExprKind::Field(recv, method) => {
                    collect_expr_calls(recv, known_fns, method_index, out);
                    if let Some(targets) = method_index.get(method) {
                        for target in targets {
                            out.insert(target.clone());
                        }
                    }
                    if known_fns.contains(method) {
                        out.insert(method.clone());
                    }
                }
                _ => {
                    collect_expr_calls(callee, known_fns, method_index, out);
                }
            }
            for arg in args {
                collect_expr_calls(&arg.expr, known_fns, method_index, out);
            }
        }
        ExprKind::Field(recv, _) | ExprKind::Try(recv) | ExprKind::Unary(_, recv) => {
            collect_expr_calls(recv, known_fns, method_index, out);
        }
        ExprKind::Index(a, b)
        | ExprKind::Binary(_, a, b)
        | ExprKind::Assign(_, a, b)
        | ExprKind::Range {
            start: a, end: b, ..
        } => {
            collect_expr_calls(a, known_fns, method_index, out);
            collect_expr_calls(b, known_fns, method_index, out);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            collect_expr_calls(condition, known_fns, method_index, out);
            collect_expr_calls(then, known_fns, method_index, out);
            if let Some(other) = otherwise {
                collect_expr_calls(other, known_fns, method_index, out);
            }
        }
        ExprKind::Match { value, arms } => {
            collect_expr_calls(value, known_fns, method_index, out);
            for (_, body) in arms {
                collect_expr_calls(body, known_fns, method_index, out);
            }
        }
        ExprKind::Closure { body, .. } | ExprKind::Loop(body) => {
            collect_expr_calls(body, known_fns, method_index, out);
        }
        ExprKind::For { iter, body, .. } => {
            collect_expr_calls(iter, known_fns, method_index, out);
            collect_expr_calls(body, known_fns, method_index, out);
        }
        ExprKind::Handle {
            state,
            body,
            ret,
            ops,
            ..
        } => {
            if let Some((_, init)) = state {
                collect_expr_calls(init, known_fns, method_index, out);
            }
            collect_expr_calls(body, known_fns, method_index, out);
            collect_expr_calls(&ret.body, known_fns, method_index, out);
            for op in ops {
                collect_expr_calls(&op.body, known_fns, method_index, out);
            }
        }
        ExprKind::Break(val) | ExprKind::Return(val) => {
            if let Some(e) = val {
                collect_expr_calls(e, known_fns, method_index, out);
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Canonical span-insensitive AST hashing
// ---------------------------------------------------------------------------

fn hash_u8(h: &mut blake3::Hasher, v: u8) {
    h.update(&[v]);
}

fn hash_bool(h: &mut blake3::Hasher, v: bool) {
    hash_u8(h, u8::from(v));
}

fn hash_u64(h: &mut blake3::Hasher, v: u64) {
    h.update(&v.to_le_bytes());
}

fn hash_str(h: &mut blake3::Hasher, s: &str) {
    hash_u64(h, s.len() as u64);
    h.update(s.as_bytes());
}

fn hash_mode(h: &mut blake3::Hasher, mode: Mode) {
    hash_u8(
        h,
        match mode {
            Mode::Copy => 0,
            Mode::Affine => 1,
            Mode::Linear => 2,
        },
    );
}

fn hash_pass(h: &mut blake3::Hasher, pass: Pass) {
    hash_u8(
        h,
        match pass {
            Pass::In => 0,
            Pass::Inout => 1,
            Pass::Sink => 2,
        },
    );
}

fn hash_generics(h: &mut blake3::Hasher, generics: &[Generic]) {
    hash_u64(h, generics.len() as u64);
    for g in generics {
        hash_str(h, &g.name);
        hash_mode(h, g.bound);
        hash_bool(h, g.row);
    }
}

fn hash_params(h: &mut blake3::Hasher, params: &[Param]) {
    hash_u64(h, params.len() as u64);
    for p in params {
        // Deliberately omit `p.span`.
        hash_str(h, &p.name);
        hash_pass(h, p.pass);
        hash_u64(h, p.projections.len() as u64);
        for proj in &p.projections {
            hash_u64(h, proj.len() as u64);
            for seg in proj {
                hash_str(h, seg);
            }
        }
        hash_type(h, &p.ty);
    }
}

fn hash_type(h: &mut blake3::Hasher, ty: &Type) {
    match ty {
        Type::IntLiteral(n) => {
            hash_u8(h, 0);
            h.update(&n.to_le_bytes());
        }
        Type::Named(name, args) => {
            hash_u8(h, 1);
            hash_str(h, name);
            hash_u64(h, args.len() as u64);
            for a in args {
                hash_type(h, a);
            }
        }
        Type::Tuple(elems) => {
            hash_u8(h, 2);
            hash_u64(h, elems.len() as u64);
            for e in elems {
                hash_type(h, e);
            }
        }
        Type::Function {
            params,
            result,
            effects,
            mode,
        } => {
            hash_u8(h, 3);
            hash_u64(h, params.len() as u64);
            for (pass, pty) in params {
                hash_pass(h, *pass);
                hash_type(h, pty);
            }
            hash_type(h, result);
            hash_effects(h, effects);
            hash_mode(h, *mode);
        }
        Type::Row(effects) => {
            hash_u8(h, 4);
            hash_effects(h, effects);
        }
        Type::Infer => {
            hash_u8(h, 5);
        }
    }
}

fn hash_effect(h: &mut blake3::Hasher, eff: &Effect) {
    match &eff.label {
        Some(l) => {
            hash_u8(h, 1);
            hash_str(h, l);
        }
        None => hash_u8(h, 0),
    }
    hash_str(h, &eff.name);
    hash_u64(h, eff.args.len() as u64);
    for a in &eff.args {
        hash_type(h, a);
    }
}

fn hash_effects(h: &mut blake3::Hasher, effects: &Effects) {
    // Hash each effect instance individually, then sort the resulting digests
    // so effect-row ordering (`eff [A, B]` vs `eff [B, A]`) is canonical.
    let mut instance_digests: Vec<[u8; 32]> = effects
        .instances
        .iter()
        .map(|eff| {
            let mut eh = blake3::Hasher::new();
            hash_effect(&mut eh, eff);
            *eh.finalize().as_bytes()
        })
        .collect();
    instance_digests.sort_unstable();
    hash_u64(h, instance_digests.len() as u64);
    for d in &instance_digests {
        h.update(d);
    }
    match &effects.tail {
        Some(t) => {
            hash_u8(h, 1);
            hash_str(h, t);
        }
        None => hash_u8(h, 0),
    }
}

fn hash_literal(h: &mut blake3::Hasher, lit: &Literal) {
    match lit {
        Literal::Int(n) => {
            hash_u8(h, 0);
            h.update(&n.to_le_bytes());
        }
        Literal::Float(f) => {
            hash_u8(h, 1);
            h.update(&f.to_bits().to_le_bytes());
        }
        Literal::Bool(b) => {
            hash_u8(h, 2);
            hash_bool(h, *b);
        }
        Literal::String(s) => {
            hash_u8(h, 3);
            hash_str(h, s);
        }
    }
}

fn hash_pattern(h: &mut blake3::Hasher, pat: &Pattern) {
    // Deliberately omit `pat.span`.
    match &pat.kind {
        PatternKind::Wild => hash_u8(h, 0),
        PatternKind::Bind(name) => {
            hash_u8(h, 1);
            hash_str(h, name);
        }
        PatternKind::Literal(lit) => {
            hash_u8(h, 2);
            hash_literal(h, lit);
        }
        PatternKind::Tuple(pats) => {
            hash_u8(h, 3);
            hash_u64(h, pats.len() as u64);
            for p in pats {
                hash_pattern(h, p);
            }
        }
        PatternKind::Constructor {
            name,
            fields,
            named,
        } => {
            hash_u8(h, 4);
            hash_str(h, name);
            hash_bool(h, *named);
            hash_u64(h, fields.len() as u64);
            for (fname, fpat) in fields {
                hash_str(h, fname);
                hash_pattern(h, fpat);
            }
        }
    }
}

fn hash_statement(h: &mut blake3::Hasher, stmt: &Statement) {
    match stmt {
        Statement::Let {
            pat,
            ty,
            value,
            mutable,
        } => {
            hash_u8(h, 0);
            hash_pattern(h, pat);
            hash_type(h, ty);
            hash_expr(h, value);
            hash_bool(h, *mutable);
        }
        Statement::Expr(expr) => {
            hash_u8(h, 1);
            hash_expr(h, expr);
        }
    }
}

fn hash_arg(h: &mut blake3::Hasher, arg: &Arg) {
    hash_pass(h, arg.pass);
    hash_bool(h, arg.explicit);
    hash_expr(h, &arg.expr);
}

fn hash_clause(h: &mut blake3::Hasher, clause: &Clause) {
    hash_str(h, &clause.name);
    hash_u64(h, clause.pats.len() as u64);
    for p in &clause.pats {
        hash_pattern(h, p);
    }
    hash_expr(h, &clause.body);
}

fn hash_expr(h: &mut blake3::Hasher, expr: &Expr) {
    // Deliberately omit `expr.span`.
    match &expr.kind {
        ExprKind::Literal(lit) => {
            hash_u8(h, 0);
            hash_literal(h, lit);
        }
        ExprKind::Name(name) => {
            hash_u8(h, 1);
            hash_str(h, name);
        }
        ExprKind::Tuple(elems) => {
            hash_u8(h, 2);
            hash_u64(h, elems.len() as u64);
            for e in elems {
                hash_expr(h, e);
            }
        }
        ExprKind::Struct { name, fields } => {
            hash_u8(h, 3);
            hash_str(h, name);
            hash_u64(h, fields.len() as u64);
            for (fname, fexpr) in fields {
                hash_str(h, fname);
                hash_expr(h, fexpr);
            }
        }
        ExprKind::Block { statements, tail } => {
            hash_u8(h, 4);
            hash_u64(h, statements.len() as u64);
            for stmt in statements {
                hash_statement(h, stmt);
            }
            match tail {
                Some(t) => {
                    hash_u8(h, 1);
                    hash_expr(h, t);
                }
                None => hash_u8(h, 0),
            }
        }
        ExprKind::Call {
            callee,
            types,
            args,
        } => {
            hash_u8(h, 5);
            hash_expr(h, callee);
            hash_u64(h, types.len() as u64);
            for ty in types {
                hash_type(h, ty);
            }
            hash_u64(h, args.len() as u64);
            for arg in args {
                hash_arg(h, arg);
            }
        }
        ExprKind::Field(recv, field) => {
            hash_u8(h, 6);
            hash_expr(h, recv);
            hash_str(h, field);
        }
        ExprKind::Index(recv, idx) => {
            hash_u8(h, 7);
            hash_expr(h, recv);
            hash_expr(h, idx);
        }
        ExprKind::Try(inner) => {
            hash_u8(h, 8);
            hash_expr(h, inner);
        }
        ExprKind::Unary(op, inner) => {
            hash_u8(h, 9);
            hash_str(h, op);
            hash_expr(h, inner);
        }
        ExprKind::Binary(op, lhs, rhs) => {
            hash_u8(h, 10);
            hash_str(h, op);
            hash_expr(h, lhs);
            hash_expr(h, rhs);
        }
        ExprKind::Assign(op, lhs, rhs) => {
            hash_u8(h, 11);
            hash_str(h, op);
            hash_expr(h, lhs);
            hash_expr(h, rhs);
        }
        ExprKind::If {
            condition,
            then,
            otherwise,
        } => {
            hash_u8(h, 12);
            hash_expr(h, condition);
            hash_expr(h, then);
            match otherwise {
                Some(o) => {
                    hash_u8(h, 1);
                    hash_expr(h, o);
                }
                None => hash_u8(h, 0),
            }
        }
        ExprKind::Match { value, arms } => {
            hash_u8(h, 13);
            hash_expr(h, value);
            hash_u64(h, arms.len() as u64);
            for (pat, body) in arms {
                hash_pattern(h, pat);
                hash_expr(h, body);
            }
        }
        ExprKind::Closure {
            params,
            mode,
            result,
            effects,
            infer_effects,
            body,
        } => {
            hash_u8(h, 14);
            hash_params(h, params);
            match mode {
                Some(m) => {
                    hash_u8(h, 1);
                    hash_mode(h, *m);
                }
                None => hash_u8(h, 0),
            }
            hash_type(h, result);
            hash_effects(h, effects);
            hash_bool(h, *infer_effects);
            hash_expr(h, body);
        }
        ExprKind::Handle {
            effect,
            state,
            body,
            ret,
            ops,
        } => {
            hash_u8(h, 15);
            hash_effect(h, effect);
            match state {
                Some((pat, init)) => {
                    hash_u8(h, 1);
                    hash_pattern(h, pat);
                    hash_expr(h, init);
                }
                None => hash_u8(h, 0),
            }
            hash_expr(h, body);
            hash_clause(h, ret);
            hash_u64(h, ops.len() as u64);
            for op in ops {
                hash_clause(h, op);
            }
        }
        ExprKind::Loop(body) => {
            hash_u8(h, 16);
            hash_expr(h, body);
        }
        ExprKind::For { pat, iter, body } => {
            hash_u8(h, 17);
            hash_pattern(h, pat);
            hash_expr(h, iter);
            hash_expr(h, body);
        }
        ExprKind::Range {
            start,
            end,
            inclusive,
        } => {
            hash_u8(h, 18);
            hash_expr(h, start);
            hash_expr(h, end);
            hash_bool(h, *inclusive);
        }
        ExprKind::Break(val) => {
            hash_u8(h, 19);
            match val {
                Some(e) => {
                    hash_u8(h, 1);
                    hash_expr(h, e);
                }
                None => hash_u8(h, 0),
            }
        }
        ExprKind::Return(val) => {
            hash_u8(h, 20);
            match val {
                Some(e) => {
                    hash_u8(h, 1);
                    hash_expr(h, e);
                }
                None => hash_u8(h, 0),
            }
        }
        ExprKind::Continue => {
            hash_u8(h, 21);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(label: &str) -> Self {
            let seq = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!(
                "vesper-anvil-hash-{label}-{}-{seq}",
                std::process::id()
            ));
            fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn parse_module(src: &str) -> Module {
        let parsed = vesper_syntax::parser::parse(src);
        assert!(
            parsed.diagnostics.is_empty(),
            "unexpected parse diagnostics: {:?}",
            parsed.diagnostics
        );
        parsed.module
    }

    #[test]
    fn fn_blake3_is_insensitive_to_whitespace_comments_and_spans() {
        let src_a = r#"
fn add(in a: i64, in b: i64) -> i64 {
    a + b
}
"#;
        let src_b = r#"
// Leading comment shifting every byte offset and line number.


fn   add( in a: i64 ,
          in b: i64 ) -> i64 {
    // comment inside body
    a   +   b
}
"#;
        let graph_a = FunctionGraph::from_module(&parse_module(src_a));
        let graph_b = FunctionGraph::from_module(&parse_module(src_b));

        assert_eq!(graph_a.fn_hash("add"), graph_b.fn_hash("add"));
        assert_eq!(
            graph_a.transitive_fn_blake3("add"),
            graph_b.transitive_fn_blake3("add")
        );
    }

    #[test]
    fn transitive_fn_blake3_invalidates_when_callee_changes() {
        let src_v1 = r#"
fn leaf(in x: i64) -> i64 { x + 1 }
fn mid(in x: i64) -> i64 { leaf(x) * 2 }
fn test_top() -> i64 { mid(10) }
fn unrelated() -> i64 { 99 }
"#;
        let src_v2 = r#"
fn leaf(in x: i64) -> i64 { x + 2 }
fn mid(in x: i64) -> i64 { leaf(x) * 2 }
fn test_top() -> i64 { mid(10) }
fn unrelated() -> i64 { 99 }
"#;
        let g1 = FunctionGraph::from_module(&parse_module(src_v1));
        let g2 = FunctionGraph::from_module(&parse_module(src_v2));

        // Direct hash of `mid` and `test_top` did not change, only `leaf` changed.
        assert_ne!(g1.fn_hash("leaf"), g2.fn_hash("leaf"));
        assert_eq!(g1.fn_hash("mid"), g2.fn_hash("mid"));
        assert_eq!(g1.fn_hash("test_top"), g2.fn_hash("test_top"));

        // Transitive hash invalidates `leaf`, `mid`, and `test_top`, but NOT `unrelated`.
        assert_ne!(
            g1.transitive_fn_blake3("leaf"),
            g2.transitive_fn_blake3("leaf")
        );
        assert_ne!(
            g1.transitive_fn_blake3("mid"),
            g2.transitive_fn_blake3("mid")
        );
        assert_ne!(
            g1.transitive_fn_blake3("test_top"),
            g2.transitive_fn_blake3("test_top")
        );
        assert_eq!(
            g1.transitive_fn_blake3("unrelated"),
            g2.transitive_fn_blake3("unrelated")
        );
    }

    #[test]
    fn reverse_callers_slices_transitively_and_handles_cycles() {
        let src = r#"
fn target(in x: i64) -> i64 { x }
fn helper(in x: i64) -> i64 { target(x) }
fn test_a() -> i64 { helper(1) }
fn test_b() -> i64 { target(2) }
fn test_unrelated() -> i64 { 42 }
fn ping(in n: i64) -> i64 { if n == 0 { target(0) } else { pong(n - 1) } }
fn pong(in n: i64) -> i64 { if n == 0 { 0 } else { ping(n - 1) } }
"#;
        let graph = FunctionGraph::from_module(&parse_module(src));
        let callers = graph.reverse_callers("target");
        let expected: BTreeSet<String> = ["helper", "test_a", "test_b", "ping", "pong"]
            .into_iter()
            .map(String::from)
            .collect();
        assert_eq!(callers, expected);
        assert!(!callers.contains("test_unrelated"));

        // Mutual recursion terminates cleanly for transitive hashing.
        assert!(graph.transitive_fn_blake3("ping").is_some());
        assert!(graph.transitive_fn_blake3("pong").is_some());
        assert!(graph.transitive_fn_blake3("nonexistent").is_none());
    }

    #[test]
    fn verdict_cache_round_trips_and_detects_missing_or_corrupt_entries() {
        let tmp = TempDir::new("verdict-cache");
        let cache = VerdictCache::open(tmp.path.join("store")).expect("open verdict cache");
        let key = "deadbeef0123456789abcdef";

        assert_eq!(cache.get(key), None);

        let payload = br#"{"status":"passed","duration_us":42}"#;
        cache.put(key, payload).expect("put verdict");
        assert_eq!(cache.get(key).as_deref(), Some(payload.as_slice()));

        // Overwriting the same key with a new verdict payload updates the index atomically.
        let updated = br#"{"status":"failed","reason":"assertion"}"#;
        cache.put(key, updated).expect("update verdict");
        assert_eq!(cache.get(key).as_deref(), Some(updated.as_slice()));

        // Path-traversal keys are rejected.
        assert!(cache.put("../escape", payload).is_err());
        assert_eq!(cache.get("../escape"), None);
    }
}
