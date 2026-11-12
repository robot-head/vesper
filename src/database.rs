//! Salsa-backed revisioned source inputs and memoized frontend queries.
//!
//! Semantic editor consumers must use [`guarded_analysis`]. Recovery inputs
//! support diagnostic-only analysis; the original parse/check/diagnostics queries
//! below retain their historical batch behavior.
use crate::{
    ast::{self, Diagnostic},
    check::{self, Checked},
    parser::{self, Parse},
    tooling::frontend::{AnalysisFailure, AnalysisLimits, CheckedSource, check_source},
};
use serde::Serialize;
use std::{collections::BTreeMap, io::Write, sync::Arc};
#[salsa::input]
pub struct SourceFile {
    #[returns(ref)]
    pub text: String,
}
#[salsa::db]
#[derive(Clone, Default)]
pub struct Database {
    storage: salsa::Storage<Self>,
}
#[salsa::db]
impl salsa::Database for Database {}
#[salsa::tracked(returns(ref), no_eq)]
pub fn parse_file(db: &dyn salsa::Database, file: SourceFile) -> Parse {
    parser::parse(file.text(db))
}
#[salsa::tracked(returns(ref), no_eq)]
pub fn check_file(db: &dyn salsa::Database, file: SourceFile) -> Checked {
    check::check(&parse_file(db, file).module)
}
#[salsa::tracked(returns(ref))]
pub fn diagnostics(db: &dyn salsa::Database, file: SourceFile) -> Vec<Diagnostic> {
    let p = parse_file(db, file);
    if p.diagnostics.is_empty() {
        check_file(db, file).diagnostics.clone()
    } else {
        p.diagnostics.clone()
    }
}

/// Identity of the actual input observed by a guarded query. Oversized legacy
/// inputs deliberately have no hash: hashing them would bypass the work guard.
/// Such failures receive a distinct opaque identity on every query execution.
#[derive(Clone, Debug)]
pub struct SourceIdentity {
    byte_len: usize,
    blake3: Option<[u8; 32]>,
    opaque: Option<Arc<()>>,
}
impl SourceIdentity {
    pub fn byte_len(&self) -> usize {
        self.byte_len
    }
    pub fn blake3(&self) -> Option<[u8; 32]> {
        self.blake3
    }
}
impl PartialEq for SourceIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.byte_len == other.byte_len
            && match (self.blake3, other.blake3) {
                (Some(a), Some(b)) => a == b,
                (None, None) => match (&self.opaque, &other.opaque) {
                    (Some(a), Some(b)) => Arc::ptr_eq(a, b),
                    _ => false,
                },
                _ => false,
            }
    }
}
impl Eq for SourceIdentity {}

/// A current-source checked snapshot or structured refusal. Construction is
/// private; neither a caller-supplied World nor a completeness flag is accepted.
#[derive(Debug)]
pub struct AnalysisSnapshot {
    identity: SourceIdentity,
    result: Result<Arc<CheckedSource>, AnalysisFailure>,
}
// Salsa's value-update machinery requires PartialEq even for a no_eq query.
// Successful snapshots compare allocation identity, never deep Worlds or spans.
impl PartialEq for AnalysisSnapshot {
    fn eq(&self, other: &Self) -> bool {
        self.identity == other.identity
            && match (&self.result, &other.result) {
                (Ok(a), Ok(b)) => Arc::ptr_eq(a, b),
                (Err(a), Err(b)) => a == b,
                _ => false,
            }
    }
}
impl AnalysisSnapshot {
    pub fn source_identity(&self) -> &SourceIdentity {
        &self.identity
    }
    pub fn checked(&self) -> Result<&CheckedSource, &AnalysisFailure> {
        self.result.as_deref()
    }
    /// Retain a validated snapshot beyond a database borrow without deep cloning.
    pub fn checked_arc(&self) -> Result<Arc<CheckedSource>, &AnalysisFailure> {
        self.result.as_ref().map(Arc::clone)
    }
}

/// Bounded editor frontend, independent of the legacy unchecked query chain.
/// Always observes raw source text, including edits with equivalent signatures.
#[salsa::tracked(returns(ref), no_eq)]
pub fn guarded_analysis(db: &dyn salsa::Database, file: SourceFile) -> AnalysisSnapshot {
    let source = file.text(db);
    let limits = AnalysisLimits::default();
    let result = check_source(source, limits).map(Arc::new);
    let hash = result
        .as_ref()
        .ok()
        .map(|checked| checked.source_hash())
        .or_else(|| {
            (source.len() <= limits.max_source_bytes)
                .then(|| *blake3::hash(source.as_bytes()).as_bytes())
        });
    AnalysisSnapshot {
        identity: SourceIdentity {
            byte_len: source.len(),
            blake3: hash,
            opaque: hash.is_none().then(|| Arc::new(())),
        },
        result,
    }
}

/// The semantic signature schema is deliberately separate from ABI/cache
/// certificates. Completeness applies only to the supported headers described
/// here, never to generated code, current ownership, positions, or body proofs.
pub const SIGNATURE_SCHEMA: &str = "vesper.editor.checked-signature/1";
pub const MAX_SIGNATURE_BYTES: usize = 1024 * 1024;
const MAX_HEADER_DECLARATIONS: usize = 4096;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignatureAvailability {
    Complete,
    Unavailable { reason: &'static str },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum SignatureIdentity {
    // Equality compares the full bounded canonical DTO bytes, not only a hash.
    Semantic(Arc<[u8]>),
    Source(SourceIdentity),
}

/// A range-free signature result. Successful equality intentionally excludes
/// current source identity; position providers must use guarded_analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureSnapshot {
    availability: SignatureAvailability,
    digest: Option<[u8; 32]>,
    identity: SignatureIdentity,
    declarations: Vec<String>,
}
impl SignatureSnapshot {
    pub fn availability(&self) -> SignatureAvailability {
        self.availability
    }
    /// Present only when complete supported semantic extraction succeeded.
    pub fn digest(&self) -> Option<[u8; 32]> {
        self.digest
    }
    /// The full input identity is retained for unavailable extraction only.
    pub fn fallback_source_identity(&self) -> Option<&SourceIdentity> {
        match &self.identity {
            SignatureIdentity::Source(source) => Some(source),
            SignatureIdentity::Semantic(_) => None,
        }
    }
    pub fn normalized_bytes(&self) -> Option<usize> {
        match &self.identity {
            SignatureIdentity::Semantic(bytes) => Some(bytes.len()),
            SignatureIdentity::Source(_) => None,
        }
    }
}

/// Signature-dependent consumers get a real Salsa equality cutoff here. A body
/// edit still reruns guarded checking, then equal supported DTOs stop propagation.
#[salsa::tracked(returns(ref))]
pub fn signature_snapshot(db: &dyn salsa::Database, file: SourceFile) -> SignatureSnapshot {
    let analysis = guarded_analysis(db, file);
    let extraction = analysis
        .checked()
        .map_err(|_| "source analysis unavailable")
        .and_then(extract_signature);
    match extraction {
        Ok((bytes, declarations)) => SignatureSnapshot {
            availability: SignatureAvailability::Complete,
            digest: Some(*blake3::hash(&bytes).as_bytes()),
            identity: SignatureIdentity::Semantic(bytes.into()),
            declarations,
        },
        Err(reason) => SignatureSnapshot {
            availability: SignatureAvailability::Unavailable { reason },
            digest: None,
            identity: SignatureIdentity::Source(analysis.identity.clone()),
            declarations: vec![],
        },
    }
}

/// A signature-dependent declaration inventory with no ranges. Complete results
/// omit current source identity; unavailable results retain the raw fallback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignatureInventory {
    availability: SignatureAvailability,
    digest: Option<[u8; 32]>,
    declarations: Vec<String>,
    fallback_source: Option<SourceIdentity>,
}
impl SignatureInventory {
    pub fn availability(&self) -> SignatureAvailability {
        self.availability
    }
    pub fn digest(&self) -> Option<[u8; 32]> {
        self.digest
    }
    pub fn declarations(&self) -> &[String] {
        &self.declarations
    }
    pub fn fallback_source_identity(&self) -> Option<&SourceIdentity> {
        self.fallback_source.as_ref()
    }
}
#[salsa::tracked(returns(ref))]
pub fn signature_inventory(db: &dyn salsa::Database, file: SourceFile) -> SignatureInventory {
    let signature = signature_snapshot(db, file);
    SignatureInventory {
        availability: signature.availability,
        digest: signature.digest,
        declarations: signature.declarations.clone(),
        fallback_source: signature.fallback_source_identity().cloned(),
    }
}

#[derive(Serialize)]
struct ParameterHeader<'a> {
    name: &'a str,
    pass: ast::Pass,
    projections: &'a [Vec<String>],
    ty: &'a ast::Type,
}
#[derive(Serialize)]
struct FunctionHeader<'a> {
    name: &'a str,
    generics: &'a [ast::Generic],
    parameters: Vec<ParameterHeader<'a>>,
    declared_result: &'a ast::Type,
    effects: &'a ast::Effects,
    has_body: bool,
}
impl<'a> From<&'a ast::Function> for FunctionHeader<'a> {
    fn from(function: &'a ast::Function) -> Self {
        Self {
            name: &function.name,
            generics: &function.generics,
            parameters: function
                .params
                .iter()
                .map(|p| ParameterHeader {
                    name: &p.name,
                    pass: p.pass,
                    projections: &p.projections,
                    ty: &p.ty,
                })
                .collect(),
            declared_result: &function.result,
            effects: &function.effects,
            has_body: function.body.is_some(),
        }
    }
}
#[derive(Serialize)]
struct AdtHeader<'a> {
    name: &'a str,
    mode: ast::Mode,
    generics: &'a [ast::Generic],
    variants: &'a [ast::Variant],
    is_enum: bool,
}
#[derive(Serialize)]
struct EffectHeader<'a> {
    name: &'a str,
    linear: bool,
    generics: &'a [ast::Generic],
    operations: Vec<FunctionHeader<'a>>,
}
#[derive(Serialize)]
struct SemanticHeaders<'a> {
    schema: &'static str,
    adts: BTreeMap<&'a str, AdtHeader<'a>>,
    functions: BTreeMap<&'a str, FunctionHeader<'a>>,
    effects: BTreeMap<&'a str, EffectHeader<'a>>,
    // These contain actual inferred latent capability/type refinements which
    // the source declaration alone does not expose.
    function_results: &'a BTreeMap<String, ast::Type>,
    string_capacity: usize,
}

fn extract_signature(checked: &CheckedSource) -> Result<(Vec<u8>, Vec<String>), &'static str> {
    let world = checked.world();
    let module = &checked.parsed().module;
    if !module.model_uses.is_empty()
        || !module.trait_bounds.is_empty()
        || module
            .items
            .iter()
            .any(|item| matches!(item, ast::Item::Trait(_) | ast::Item::Conformance(_)))
        || !world.elaboration.traits.is_empty()
        || !world.elaboration.conformances.is_empty()
        || !world.elaboration.generated.is_empty()
        || world.elaboration.package.is_some()
        || world.elaboration.revision.is_some()
    {
        return Err("trait/model headers are outside supported signature extraction");
    }
    let count = world
        .adts
        .len()
        .checked_add(world.functions.len())
        .and_then(|n| n.checked_add(world.effects.len()))
        .ok_or("signature declaration count overflow")?;
    if count > MAX_HEADER_DECLARATIONS {
        return Err("signature declaration limit");
    }
    // Successful checking may still leave an unresolved result annotation in
    // today's compiler. Such a World is usable for diagnostics, but cannot
    // establish a complete semantic header identity.
    for (name, function) in &world.functions {
        if function.body.is_some() && world.function_results.get(name).is_none_or(unresolved_type) {
            return Err("actual function result is unresolved or unavailable");
        }
        if function.params.iter().any(|p| unresolved_type(&p.ty)) {
            return Err("function parameter type is unresolved");
        }
        if unresolved_effects(&function.effects) {
            return Err("function effect argument type is unresolved");
        }
    }
    for adt in world.adts.values() {
        if adt
            .variants
            .iter()
            .flat_map(|v| &v.fields)
            .any(|f| unresolved_type(&f.ty))
        {
            return Err("ADT field type is unresolved");
        }
    }
    for effect in world.effects.values() {
        if effect.ops.iter().any(|op| {
            op.body.is_some()
                || unresolved_type(&op.result)
                || op.params.iter().any(|p| unresolved_type(&p.ty))
                || unresolved_effects(&op.effects)
        }) {
            return Err("effect operation header is unresolved or unsupported");
        }
    }
    let headers = SemanticHeaders {
        schema: SIGNATURE_SCHEMA,
        adts: world
            .adts
            .iter()
            .map(|(name, a)| {
                (
                    name.as_str(),
                    AdtHeader {
                        name: &a.name,
                        mode: a.mode,
                        generics: &a.generics,
                        variants: &a.variants,
                        is_enum: a.is_enum,
                    },
                )
            })
            .collect(),
        functions: world
            .functions
            .iter()
            .map(|(name, f)| (name.as_str(), f.into()))
            .collect(),
        effects: world
            .effects
            .iter()
            .map(|(name, e)| {
                (
                    name.as_str(),
                    EffectHeader {
                        name: &e.name,
                        linear: e.linear,
                        generics: &e.generics,
                        operations: e.ops.iter().map(FunctionHeader::from).collect(),
                    },
                )
            })
            .collect(),
        function_results: &world.function_results,
        string_capacity: world.string_capacity,
    };
    let mut output = HeaderOutput(Vec::new());
    serde_json::to_writer(&mut output, &headers)
        .map_err(|_| "signature serialization limit or failure")?;
    let declarations = world
        .adts
        .keys()
        .map(|name| format!("type::{name}"))
        .chain(world.functions.keys().map(|name| format!("fn::{name}")))
        .chain(world.effects.keys().map(|name| format!("effect::{name}")))
        .collect();
    Ok((output.0, declarations))
}

fn unresolved_effects(effects: &ast::Effects) -> bool {
    effects
        .instances
        .iter()
        .flat_map(|e| &e.args)
        .any(unresolved_type)
}

fn unresolved_type(root: &ast::Type) -> bool {
    let mut stack = vec![root];
    let mut remaining = 8192usize;
    while let Some(ty) = stack.pop() {
        let Some(next) = remaining.checked_sub(1) else {
            return true;
        };
        remaining = next;
        match ty {
            ast::Type::Infer => return true,
            ast::Type::Named(_, args) | ast::Type::Tuple(args) => {
                if args.len() > remaining {
                    return true;
                }
                stack.extend(args);
            }
            ast::Type::Function {
                params,
                result,
                effects,
                ..
            } => {
                if params.len() > remaining {
                    return true;
                }
                stack.extend(params.iter().map(|(_, ty)| ty));
                stack.push(result);
                for effect in &effects.instances {
                    if effect.args.len() > remaining {
                        return true;
                    }
                    stack.extend(&effect.args);
                }
            }
            ast::Type::Row(effects) => {
                for effect in &effects.instances {
                    if effect.args.len() > remaining {
                        return true;
                    }
                    stack.extend(&effect.args);
                }
            }
            ast::Type::IntLiteral(_) => (),
        }
        if stack.len() > remaining {
            return true;
        }
    }
    false
}

struct HeaderOutput(Vec<u8>);
impl Write for HeaderOutput {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() > MAX_SIGNATURE_BYTES.saturating_sub(self.0.len()) {
            return Err(std::io::Error::other("signature byte limit"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use salsa::Setter;
    #[test]
    fn memoization_and_invalidation() {
        let mut db = Database::default();
        let file = SourceFile::new(&db, "fn main() {}".into());
        let first = parse_file(&db, file) as *const Parse;
        assert_eq!(first, parse_file(&db, file) as *const Parse);
        assert!(diagnostics(&db, file).is_empty());
        file.set_text(&mut db).to("fn main() { unknown }".into());
        assert!(diagnostics(&db, file).iter().any(|d| d.code == "E0201"));
    }
}

/// A sealed AST-free recovery snapshot. Equality is allocation identity, keeping
/// Salsa input updates bounded and preventing accidental deep CST comparisons.
#[derive(Clone, Debug)]
pub struct SyntaxSnapshot(Arc<vesper_syntax::recovery::RecoverySyntax>);
impl SyntaxSnapshot {
    pub fn new(syntax: Arc<vesper_syntax::recovery::RecoverySyntax>) -> Self {
        Self(syntax)
    }
    pub fn syntax(&self) -> &vesper_syntax::recovery::RecoverySyntax {
        &self.0
    }
    pub fn syntax_arc(&self) -> &Arc<vesper_syntax::recovery::RecoverySyntax> {
        &self.0
    }
}
impl PartialEq for SyntaxSnapshot {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}
impl Eq for SyntaxSnapshot {}
#[salsa::input]
pub struct RecoveryFile {
    #[returns(ref)]
    pub snapshot: SyntaxSnapshot,
}
#[derive(Debug)]
struct RecoveryQuery {
    input: SyntaxSnapshot,
    result: Result<
        Arc<crate::tooling::recovery::RecoveryDiagnostics>,
        crate::tooling::recovery::RecoveryFailure,
    >,
}
impl PartialEq for RecoveryQuery {
    fn eq(&self, other: &Self) -> bool {
        self.input == other.input
            && match (&self.result, &other.result) {
                (Ok(a), Ok(b)) => Arc::ptr_eq(a, b),
                (Err(a), Err(b)) => a == b,
                _ => false,
            }
    }
}
// Salsa input setters invalidate unconditionally. This actual tracked identity
// boundary backdates identical sealed Arc values without comparing source/CSTs.
#[salsa::tracked(returns(ref))]
fn recovery_input(db: &dyn salsa::Database, file: RecoveryFile) -> SyntaxSnapshot {
    file.snapshot(db).clone()
}
#[salsa::tracked(returns(ref), no_eq)]
fn recovery_query(db: &dyn salsa::Database, file: RecoveryFile) -> RecoveryQuery {
    let input = recovery_input(db, file);
    let result = crate::tooling::recovery::analyze(
        input.syntax(),
        crate::tooling::recovery::RecoveryLimits::default(),
    )
    .map(Arc::new);
    RecoveryQuery {
        input: input.clone(),
        result,
    }
}
/// Diagnostic-only analysis of the actual sealed snapshot carried by this input.
/// Consumers must match its exact source/hash to their current document. Neither
/// this query nor RecoverySyntax can authorize semantic editor operations.
pub fn recovery_diagnostics(
    db: &dyn salsa::Database,
    file: RecoveryFile,
) -> &Result<
    Arc<crate::tooling::recovery::RecoveryDiagnostics>,
    crate::tooling::recovery::RecoveryFailure,
> {
    &recovery_query(db, file).result
}
