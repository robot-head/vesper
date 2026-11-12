//! Trusted definition registry: global identity separate from printed names.
//!
//! A [`DefId`] is the resolved identity of a global definition. The printed
//! path and any span are display data only. Untrusted macro output may *claim*
//! a `DefId`; [`DefinitionRegistry::validate_claim`] checks the claim against
//! this host-supplied registry, so a guest cannot invent a definition, claim
//! that a definition belongs to a package, or reach one its package cannot see.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::error::ForgeError;

/// Version of the serialized [`DefinitionRegistry`].
pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PackageId(pub u32);

impl fmt::Display for PackageId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pkg{}", self.0)
    }
}

/// Package-qualified global definition identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DefId {
    pub package: PackageId,
    pub item: u32,
}

impl fmt::Display for DefId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.package, self.item)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Visibility {
    Public,
    Package,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum DefKind {
    Function,
    /// An effect declaration; handlers are constructed for it.
    Effect,
    /// An effect operation (performing it requires the effect's authority).
    EffectOp,
    /// A handler implementation.
    Handler,
    /// A macro definition (its package is the macro package).
    Macro,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackageInfo {
    pub id: PackageId,
    pub name: String,
    /// Direct dependencies whose public definitions this package may name.
    pub dependencies: BTreeSet<PackageId>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefInfo {
    pub id: DefId,
    /// Printed path, e.g. `@vesper-wire/hash::WireHash::hash_into`.
    pub path: String,
    pub kind: DefKind,
    pub visibility: Visibility,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegistryWire {
    schema_version: u32,
    packages: Vec<PackageInfo>,
    definitions: Vec<DefInfo>,
}

/// Host-trusted packages and definitions.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RegistryWire", into = "RegistryWire")]
pub struct DefinitionRegistry {
    packages: BTreeMap<PackageId, PackageInfo>,
    definitions: BTreeMap<DefId, DefInfo>,
}

impl TryFrom<RegistryWire> for DefinitionRegistry {
    type Error = ForgeError;
    fn try_from(w: RegistryWire) -> Result<Self, ForgeError> {
        if w.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(ForgeError::Schema {
                what: "definition registry",
                found: w.schema_version,
                expected: REGISTRY_SCHEMA_VERSION,
            });
        }
        let mut r = Self::default();
        for p in w.packages {
            r.add_package(p)?;
        }
        for d in w.definitions {
            r.add_definition(d)?;
        }
        Ok(r)
    }
}

impl From<DefinitionRegistry> for RegistryWire {
    fn from(r: DefinitionRegistry) -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            packages: r.packages.into_values().collect(),
            definitions: r.definitions.into_values().collect(),
        }
    }
}

impl DefinitionRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_package(&mut self, info: PackageInfo) -> Result<(), ForgeError> {
        if self.packages.contains_key(&info.id) {
            return Err(ForgeError::DuplicateEntry {
                what: "package",
                id: info.id.to_string(),
            });
        }
        self.packages.insert(info.id, info);
        Ok(())
    }

    pub fn add_definition(&mut self, info: DefInfo) -> Result<(), ForgeError> {
        if !self.packages.contains_key(&info.id.package) {
            return Err(ForgeError::UnknownPackage(info.id.package));
        }
        if self.definitions.contains_key(&info.id) {
            return Err(ForgeError::DuplicateEntry {
                what: "definition",
                id: info.id.to_string(),
            });
        }
        self.definitions.insert(info.id, info);
        Ok(())
    }

    pub fn package(&self, id: PackageId) -> Option<&PackageInfo> {
        self.packages.get(&id)
    }

    pub fn definition(&self, id: DefId) -> Option<&DefInfo> {
        self.definitions.get(&id)
    }

    /// Whether code written in `from` may name `def`.
    pub fn visible_from(&self, from: PackageId, def: DefId) -> Result<(), ForgeError> {
        let info = self
            .definitions
            .get(&def)
            .ok_or(ForgeError::UnknownDefinition(def))?;
        if def.package == from {
            return Ok(());
        }
        let pkg = self
            .packages
            .get(&from)
            .ok_or(ForgeError::UnknownPackage(from))?;
        if !pkg.dependencies.contains(&def.package) {
            return Err(ForgeError::NotADependency { from, def });
        }
        if info.visibility != Visibility::Public {
            return Err(ForgeError::PrivateDefinition { from, def });
        }
        Ok(())
    }

    /// Validates an untrusted pre-resolved `DefId` claim on a token written
    /// by code in package `from`, whose printed text is `printed`.
    ///
    /// The claim must name an existing definition visible from `from`, and the
    /// printed path must be the registry's path for it (so diagnostics cannot
    /// display one name while the identity is another).
    pub fn validate_claim(
        &self,
        from: PackageId,
        printed: &str,
        def: DefId,
    ) -> Result<&DefInfo, ForgeError> {
        self.visible_from(from, def)?;
        let info = &self.definitions[&def];
        if info.path != printed {
            return Err(ForgeError::PrintedNameMismatch {
                def,
                printed: printed.to_owned(),
                registered: info.path.clone(),
            });
        }
        Ok(info)
    }
}
