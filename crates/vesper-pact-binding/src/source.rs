//! Root-free views of checked source packages.
//!
//! The binding core never names the compiler's `World`: the compiler (or a
//! crate above it) implements [`SourceWorld`] for its checked declarations,
//! so the dependency direction stays `vesper -> vesper-pact-binding ->
//! {vesper-core, vesper-pact}`.

use crate::error::{BridgeError, ErrorKind};
use crate::identity::PackageIdentity;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use vesper_core::ast::{Adt, EffectDecl, Item, Mode, Module, Span, Type};

/// The checked declarations of one package (or of the prelude) as seen by
/// the binding core. Implementations must report the checker's own modal
/// kinding: mode schemes are verified against [`SourceWorld::mode`].
pub trait SourceWorld: fmt::Debug + Send + Sync {
    /// All ADTs visible to the package, including the prelude.
    fn adts(&self) -> &BTreeMap<String, Adt>;
    /// All effects visible to the package, including the prelude.
    fn effects(&self) -> &BTreeMap<String, EffectDecl>;
    /// The checker's mode of `ty` under the parameter `bounds`.
    fn mode(&self, ty: &Type, bounds: &BTreeMap<String, Mode>) -> Mode;
}

/// A package whose source the caller has already parsed and checked without
/// diagnostics, together with the spans of the declarations it owns.
#[derive(Clone, Debug)]
pub struct SourcePackage {
    identity: PackageIdentity,
    world: Arc<dyn SourceWorld>,
    adt_spans: BTreeMap<String, Span>,
    effect_spans: BTreeMap<String, Span>,
    fingerprint: Option<String>,
}

impl SourcePackage {
    /// Wraps a checked package. `module` supplies the names and spans of the
    /// package's own declarations; every other ADT/effect of `world` is
    /// treated as prelude. The identity must be valid and not reserved.
    pub fn new(
        identity: PackageIdentity,
        world: Arc<dyn SourceWorld>,
        module: &Module,
    ) -> Result<Self, BridgeError> {
        if let Err(msg) = identity.validate() {
            return Err(BridgeError::single(ErrorKind::Invalid, None, msg));
        }
        if identity.is_builtin() {
            return Err(BridgeError::single(
                ErrorKind::Invalid,
                None,
                format!("package name {} is reserved", identity.name),
            ));
        }
        let mut adt_spans = BTreeMap::new();
        let mut effect_spans = BTreeMap::new();
        for item in &module.items {
            match item {
                Item::Adt(a) => {
                    adt_spans.insert(a.name.clone(), a.span.clone());
                }
                Item::Effect(e) => {
                    effect_spans.insert(e.name.clone(), e.span.clone());
                }
                _ => {}
            }
        }
        Ok(SourcePackage {
            identity,
            world,
            adt_spans,
            effect_spans,
            fingerprint: None,
        })
    }

    /// Binds the session [`Revision`](crate::Revision) to a caller-computed
    /// content fingerprint of the package's source declarations, so any
    /// declaration change (not only one visible in the lowered Pact program)
    /// invalidates evidence. It must be independent of spans and order.
    pub fn with_fingerprint(mut self, fingerprint: impl Into<String>) -> Self {
        self.fingerprint = Some(fingerprint.into());
        self
    }

    pub fn fingerprint(&self) -> Option<&str> {
        self.fingerprint.as_deref()
    }

    pub fn identity(&self) -> &PackageIdentity {
        &self.identity
    }

    pub fn world(&self) -> &dyn SourceWorld {
        self.world.as_ref()
    }

    /// Names of the ADTs declared by this package (excluding the prelude).
    pub fn adt_names(&self) -> impl Iterator<Item = &str> {
        self.adt_spans.keys().map(String::as_str)
    }

    pub fn effect_names(&self) -> impl Iterator<Item = &str> {
        self.effect_spans.keys().map(String::as_str)
    }

    pub fn adt_span(&self, name: &str) -> Option<Span> {
        self.adt_spans.get(name).cloned()
    }
}
