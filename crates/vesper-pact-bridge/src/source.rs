//! Successfully parsed and checked source packages.

use crate::error::{BridgeError, ErrorKind};
use crate::identity::PackageIdentity;
use std::collections::BTreeMap;
use vesper::ast::{Item, Module, Span};
use vesper::check::World;

/// A package whose source parsed and checked without any diagnostic.
///
/// This is the only way source declarations enter a bridge session, so a
/// session never sees a module the compiler rejected.
#[derive(Clone, Debug)]
pub struct CheckedPackage {
    identity: PackageIdentity,
    module: Module,
    world: World,
    adt_spans: BTreeMap<String, Span>,
    effect_spans: BTreeMap<String, Span>,
}

impl CheckedPackage {
    /// Parses and checks `source` with the real Vesper front end. Any parse
    /// or check diagnostic rejects the package.
    pub fn check(identity: PackageIdentity, source: &str) -> Result<Self, BridgeError> {
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
        let parsed = vesper::parser::parse(source);
        if !parsed.diagnostics.is_empty() {
            return Err(BridgeError::source(identity, parsed.diagnostics));
        }
        let checked = vesper::check::check(&parsed.module);
        if !checked.diagnostics.is_empty() {
            return Err(BridgeError::source(identity, checked.diagnostics));
        }
        let mut adt_spans = BTreeMap::new();
        let mut effect_spans = BTreeMap::new();
        for item in &parsed.module.items {
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
        Ok(CheckedPackage {
            identity,
            module: parsed.module,
            world: checked.world,
            adt_spans,
            effect_spans,
        })
    }

    pub fn identity(&self) -> &PackageIdentity {
        &self.identity
    }

    /// The checked module (read-only).
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// The checker's world for this package (read-only).
    pub fn world(&self) -> &World {
        &self.world
    }

    /// Names of the ADTs declared by this package (excluding the prelude).
    pub fn adt_names(&self) -> impl Iterator<Item = &str> {
        self.adt_spans.keys().map(String::as_str)
    }

    pub fn effect_names(&self) -> impl Iterator<Item = &str> {
        self.effect_spans.keys().map(String::as_str)
    }

    pub(crate) fn adt_span(&self, name: &str) -> Option<Span> {
        self.adt_spans.get(name).cloned()
    }
}
