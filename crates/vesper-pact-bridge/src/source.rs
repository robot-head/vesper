//! Successfully parsed and checked source packages.

use crate::error::{BridgeError, ErrorKind};
use crate::identity::PackageIdentity;
use std::sync::Arc;
use vesper::ast::Module;
use vesper::check::World;
use vesper_pact_binding::{BindingsDto, SourcePackage};

/// A package whose source parsed and checked without any diagnostic.
///
/// This is the only way source declarations enter a bridge session, so a
/// session never sees a module the compiler rejected.
#[derive(Clone, Debug)]
pub struct CheckedPackage {
    module: Module,
    world: Arc<World>,
    source: SourcePackage,
    source_bindings: BindingsDto,
}

impl CheckedPackage {
    /// Parses and checks `source` with the real Vesper front end. Any parse
    /// or check diagnostic rejects the package. Source `trait`, `impl ... for`
    /// and `model` declarations are elaborated by the compiler and their
    /// lowered bindings are available from [`CheckedPackage::source_bindings`].
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
        let checked = vesper::pact_frontend::check_package(&parsed.module, &identity);
        if !checked.checked.diagnostics.is_empty() {
            return Err(BridgeError::source(identity, checked.checked.diagnostics));
        }
        let world = Arc::new(checked.checked.world);
        let source = SourcePackage::new(identity, world.clone(), &parsed.module)?.with_fingerprint(
            vesper::pact_frontend::declaration_fingerprint(&parsed.module),
        );
        Ok(CheckedPackage {
            module: parsed.module,
            world,
            source,
            source_bindings: checked.bindings,
        })
    }

    pub fn identity(&self) -> &PackageIdentity {
        self.source.identity()
    }

    /// The checked module (read-only).
    pub fn module(&self) -> &Module {
        &self.module
    }

    /// The checker's world for this package (read-only).
    pub fn world(&self) -> &World {
        &self.world
    }

    /// The root-free view used by the binding core.
    pub fn source_package(&self) -> &SourcePackage {
        &self.source
    }

    /// Bindings lowered from this package's own source trait, canonical impl
    /// and named model declarations (empty when it declares none).
    pub fn source_bindings(&self) -> &BindingsDto {
        &self.source_bindings
    }

    /// Names of the ADTs declared by this package (excluding the prelude).
    pub fn adt_names(&self) -> impl Iterator<Item = &str> {
        self.source.adt_names()
    }

    pub fn effect_names(&self) -> impl Iterator<Item = &str> {
        self.source.effect_names()
    }
}
