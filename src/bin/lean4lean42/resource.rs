//! Fixed resource-model source and kernel-projection associations.
#![forbid(unsafe_code)]
use super::{artifact, protocol::Inventory, *};
use std::collections::{BTreeMap, BTreeSet};

pub const MODULE: &str = "ResourceModel42Kernel";
pub const ORIGINAL: &str = "ResourceModel42.Acceptance";
pub const MARKER: &[u8] = b"VESPER_RESOURCE_KERNEL_SNAPSHOT_V1:";
const MIB: usize = 1024 * 1024;
const BUNDLE: &[u8] = include_bytes!("../../../tools/lean4lean42/resource-source-manifest.json");
pub(super) const TOOL: &[u8] =
    include_bytes!("../../../tools/resource-model42/KernelSnapshot.lean");
pub(super) const HISTORICAL: &[u8] =
    include_bytes!("../../../docs/rfcs/resource-model42-source-manifest.json");
// Dependency order is fixed, not inferred from user input.
pub(super) const SOURCES: [(&str, &[u8]); 12] = [
    (
        "MetatheoryFull",
        include_bytes!("../../../lean/MetatheoryFull.lean"),
    ),
    (
        "ResourceModel42.Syntax",
        include_bytes!("../../../lean/ResourceModel42/Syntax.lean"),
    ),
    (
        "ResourceModel42.Checker",
        include_bytes!("../../../lean/ResourceModel42/Checker.lean"),
    ),
    (
        "ResourceModel42.CheckerProofs",
        include_bytes!("../../../lean/ResourceModel42/CheckerProofs.lean"),
    ),
    (
        "ResourceModel42.Evaluator",
        include_bytes!("../../../lean/ResourceModel42/Evaluator.lean"),
    ),
    (
        "ResourceModel42.ErasureProofs",
        include_bytes!("../../../lean/ResourceModel42/ErasureProofs.lean"),
    ),
    (
        "ResourceModel42.LedgerProofs",
        include_bytes!("../../../lean/ResourceModel42/LedgerProofs.lean"),
    ),
    (
        "ResourceModel42.MetricsProofs",
        include_bytes!("../../../lean/ResourceModel42/MetricsProofs.lean"),
    ),
    (
        "ResourceModel42.EvaluatorProofs",
        include_bytes!("../../../lean/ResourceModel42/EvaluatorProofs.lean"),
    ),
    (
        "ResourceModel42.Proofs",
        include_bytes!("../../../lean/ResourceModel42/Proofs.lean"),
    ),
    (
        "ResourceModel42",
        include_bytes!("../../../lean/ResourceModel42.lean"),
    ),
    (
        "ResourceModel42.Acceptance",
        include_bytes!("../../../lean/ResourceModel42/Acceptance.lean"),
    ),
];
fn bad(detail: &str) -> Error {
    Error::new(ErrorKind::ToolIdentity, detail)
}
fn cap(detail: &str) -> Error {
    Error::new(ErrorKind::OutputLimit, detail)
}
fn hash(bytes: &[u8]) -> Digest {
    *blake3::hash(bytes).as_bytes()
}
pub(super) fn module_path(module: &str, suffix: &str) -> PathBuf {
    let mut path = PathBuf::new();
    for component in module.split('.') {
        path.push(component);
    }
    path.set_extension(suffix);
    path
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Bundle {
    schema: u32,
    domain: String,
    source_commit: String,
    specification_sha256: String,
    files: Vec<FileDigest>,
}
/// Validate exact embedded bytes, including the separate historical SHA256 manifest.
pub fn verify_bundle() -> Result<Digest, Error> {
    let bundle: Bundle = artifact::bounded_json(BUNDLE, MIB)?;
    if bundle.schema != 1
        || bundle.domain != "vesper.lean4lean42.resource-source-bundle/1"
        || bundle.source_commit != "ad7208b825513d1e6095fcb80c9621d000f570e2"
        || bundle.specification_sha256
            != "8df79941a11ca2d777d8d5c5e7f4db26ef1a57f06babe2e71f3951a1f676883d"
        || bundle.files.len() != 14
    {
        return Err(bad("resource source bundle header"));
    }
    let mut expected = BTreeMap::new();
    for (module, bytes) in SOURCES {
        expected.insert(
            format!(
                "lean/{}",
                module_path(module, "lean")
                    .to_string_lossy()
                    .replace('\\', "/")
            ),
            bytes,
        );
    }
    expected.insert("tools/resource-model42/KernelSnapshot.lean".into(), TOOL);
    expected.insert(
        "docs/rfcs/resource-model42-source-manifest.json".into(),
        HISTORICAL,
    );
    let mut previous = "";
    for file in &bundle.files {
        let bytes = expected
            .remove(&file.path)
            .ok_or_else(|| bad("unexpected resource bundle path"))?;
        if file.path.as_str() <= previous
            || file.bytes != bytes.len() as u64
            || file.blake3 != hash(bytes)
        {
            return Err(bad("resource source bundle byte association"));
        }
        previous = &file.path;
    }
    if !expected.is_empty() {
        return Err(bad("missing resource source bundle file"));
    }
    Ok(hash(BUNDLE))
}
pub(super) fn bundle_bytes() -> &'static [u8] {
    BUNDLE
}

/// Compact report association. Detailed occurrence tables live in the bound producer sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResourceAssociation {
    pub schema: u32,
    pub scope: String,
    pub module: String,
    pub resource_bundle_blake3: Digest,
    pub source_manifest_blake3: Digest,
    pub snapshot_tool: FileDigest,
    pub request: FileDigest,
    pub producer_report: FileDigest,
    pub projection: FileDigest,
    pub original_manifest_blake3: Digest,
    pub retained_constants: u64,
    pub omitted_constants: u64,
    pub proof_aliases: u64,
    pub output_constants: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ModuleRecord {
    name: String,
    path: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Omission {
    name: String,
    parent: String,
    module: String,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Duplicate {
    name: String,
    first_module: String,
    repeated_module: String,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Occurrence {
    name: String,
    module: String,
    index: u64,
    disposition: String,
    alias: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Alias {
    name: String,
    original_name: String,
    source_module: String,
    canonical_module: String,
    source_occurrence: u64,
    original_group: Vec<String>,
    reason: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Producer {
    schema_version: u32,
    scope: String,
    original_module: String,
    projection_module: String,
    projection_path: String,
    original_modules: Vec<ModuleRecord>,
    trusted_modules: Vec<ModuleRecord>,
    original_constants: u64,
    retained_constants: u64,
    omitted_constants: u64,
    omitted: Vec<Omission>,
    original_constant_occurrences: u64,
    duplicate_constants: u64,
    duplicates: Vec<Duplicate>,
    occurrences: Vec<Occurrence>,
    alias_scope: String,
    proof_alias_count: u64,
    proof_aliases: Vec<Alias>,
    output_constants: u64,
    logical_expression_nodes: u64,
    exact_inventory: bool,
    constant_info_equal: bool,
    kernel_replayed: bool,
    reimport_verified: bool,
    extensions_empty: bool,
    ir_empty: bool,
}
/// Immutable actual preparation. No deserialization or caller-supplied accepted flag exists.
pub struct PreparedResource {
    workspace: artifact::Workspace,
    sysroot: PathBuf,
    tool_root: PathBuf,
    receipt: Vec<u8>,
    files: Vec<FileDigest>,
    association: ResourceAssociation,
    expected: BTreeSet<String>,
    aliases: Vec<String>,
    catalog: Vec<String>,
    origins: BTreeMap<String, String>,
    alias_originals: Vec<String>,
    omission_parents: Vec<String>,
}
impl PreparedResource {
    pub fn association(&self) -> &ResourceAssociation {
        &self.association
    }
    pub fn root(&self) -> &Path {
        &self.workspace.path
    }
    pub fn projection_root(&self) -> PathBuf {
        self.root().join("projection")
    }
    pub fn source_path(&self) -> PathBuf {
        self.root().join("sources/ResourceModel42/Acceptance.lean")
    }
    pub fn projection_path(&self) -> PathBuf {
        self.root().join("projection/ResourceModel42Kernel.olean")
    }
    pub fn request_path(&self) -> PathBuf {
        self.root().join("request.json")
    }
    pub fn report_path(&self) -> PathBuf {
        self.root().join("producer-report.json")
    }
    pub fn owned_names(&self) -> &BTreeSet<String> {
        &self.expected
    }
    pub fn aliases(&self) -> &[String] {
        &self.aliases
    }
    pub fn catalog(&self) -> &[String] {
        &self.catalog
    }
    pub(super) fn files(&self) -> &[FileDigest] {
        &self.files
    }
    pub(super) fn tool_root(&self) -> &Path {
        &self.tool_root
    }
    pub(super) fn receipt(&self) -> &[u8] {
        &self.receipt
    }
    pub(super) fn sysroot(&self) -> &Path {
        &self.sysroot
    }
    /// Checked live allocation envelope. A BTree node is conservatively prepaid
    /// as 1 KiB per entry plus one root node; stored string buffers are additional.
    pub(super) fn retained_bytes(&self) -> Result<usize, Error> {
        fn add(total: &mut usize, n: usize) -> Result<(), Error> {
            *total = total
                .checked_add(n)
                .ok_or_else(|| cap("resource retained metadata overflow"))?;
            Ok(())
        }
        fn entries(n: usize, size: usize) -> Result<usize, Error> {
            n.checked_mul(size)
                .ok_or_else(|| cap("resource retained collection overflow"))
        }
        fn strings(total: &mut usize, values: &Vec<String>) -> Result<(), Error> {
            add(total, entries(values.capacity(), size_of::<String>())?)?;
            for value in values {
                add(total, value.capacity())?;
            }
            Ok(())
        }
        let mut total = size_of::<Self>();
        for bytes in [
            self.workspace.path.capacity(),
            self.sysroot.capacity(),
            self.tool_root.capacity(),
            self.receipt.capacity(),
            self.association.scope.capacity(),
            self.association.module.capacity(),
        ] {
            add(&mut total, bytes)?;
        }
        add(
            &mut total,
            entries(self.files.capacity(), size_of::<FileDigest>())?,
        )?;
        for file in &self.files {
            add(&mut total, file.path.capacity())?;
        }
        for file in [
            &self.association.snapshot_tool,
            &self.association.request,
            &self.association.producer_report,
            &self.association.projection,
        ] {
            add(&mut total, file.path.capacity())?;
        }
        for values in [
            &self.aliases,
            &self.alias_originals,
            &self.omission_parents,
            &self.catalog,
        ] {
            strings(&mut total, values)?;
        }
        for n in [self.expected.len(), self.origins.len()] {
            add(
                &mut total,
                entries(
                    n.checked_add(1)
                        .ok_or_else(|| cap("resource tree root overflow"))?,
                    1024,
                )?,
            )?;
        }
        for name in &self.expected {
            add(&mut total, name.capacity())?;
        }
        for (name, module) in &self.origins {
            add(&mut total, name.capacity())?;
            add(&mut total, module.capacity())?;
        }
        Ok(total)
    }
    pub fn revalidate(&self, limits: &Limits) -> Result<(), Error> {
        if self.retained_bytes()? > limits.inventory_bytes {
            return Err(cap("resource retained metadata cap"));
        }
        if verify_bundle()? != self.association.resource_bundle_blake3
            || artifact::resource_files(self.root(), limits)? != self.files
        {
            return Err(bad(
                "prepared resource source/object/report envelope changed",
            ));
        }
        artifact::validate_resource_tool(self, limits)
    }
    /// Bind the actual typed inventory's entire owned image to the producer's exact set.
    pub fn validate_inventory(&self, inventory: &Inventory) -> Result<(), Error> {
        if inventory.target != 3 {
            return Err(bad("resource inventory target"));
        }
        let owned = inventory
            .modules
            .iter()
            .filter(|m| m.name.is(&[MODULE]))
            .collect::<Vec<_>>();
        if owned.len() != 1 || Path::new(&owned[0].path) != self.projection_path() {
            return Err(bad("resource inventory module association"));
        }
        let mut images = BTreeMap::new();
        for constant in inventory
            .constants
            .iter()
            .filter(|c| c.module_id == owned[0].id)
        {
            let image = constant.name.resource_display()?;
            if constant.skip != 0
                || constant.form == 3
                || images.insert(image, constant.form).is_some()
            {
                return Err(bad(
                    "resource owned skip, opaque or noninjective name image",
                ));
            }
        }
        if images.len() != self.expected.len() || images.keys().any(|n| !self.expected.contains(n))
        {
            return Err(bad("resource canonical and alias inventory differs"));
        }
        for name in self
            .catalog
            .iter()
            .chain(self.aliases.iter())
            .chain(self.alias_originals.iter())
        {
            if images.get(name) != Some(&2) {
                return Err(bad("resource required theorem form"));
            }
        }
        for name in [
            "validate",
            "checkProgram",
            "validateInvocation",
            "evalSteps",
        ] {
            if images.get(&format!("Vesper.ResourceModel42.{name}")) != Some(&1) {
                return Err(bad("resource required API definition form"));
            }
        }
        for name in &self.omission_parents {
            if images.get(name) != Some(&1) {
                return Err(bad("resource omitted helper owner must be definition"));
            }
        }
        // The fixed catalog's source module must have retained that exact original name.
        for (name, module) in &self.origins {
            if !SOURCES.iter().any(|(m, _)| *m == module) || !images.contains_key(name) {
                return Err(bad("resource source catalog origin"));
            }
        }
        Ok(())
    }
}

fn display_name(name: &str) -> Result<(), Error> {
    if name.is_empty() || name.len() > 4096 || name.split('.').count() > 512 {
        return Err(cap("resource producer name bounds"));
    }
    for component in name.split('.') {
        let bytes = component.as_bytes();
        let identifier = bytes
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
            && bytes
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || matches!(*b, b'_' | b'\''));
        let number = component
            .parse::<u64>()
            .is_ok_and(|n| n.to_string() == component);
        if !identifier && !number && component != "_@" {
            return Err(bad("unsupported resource producer name"));
        }
    }
    Ok(())
}

/// Catalog only fixed, embedded source commands. No guest source or theorem assumptions enter.
fn catalog() -> Result<BTreeMap<String, String>, Error> {
    let mut roots = BTreeMap::new();
    for (module, bytes) in SOURCES
        .iter()
        .filter(|(m, _)| m.starts_with("ResourceModel42"))
    {
        let mut clean = Vec::with_capacity(bytes.len());
        let (mut i, mut depth, mut string) = (0, 0usize, false);
        while i < bytes.len() {
            if depth > 0 {
                if bytes[i..].starts_with(b"/-") {
                    depth += 1;
                    clean.extend_from_slice(b"  ");
                    i += 2;
                } else if bytes[i..].starts_with(b"-/") {
                    depth -= 1;
                    clean.extend_from_slice(b"  ");
                    i += 2;
                } else {
                    clean.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            } else if string {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    clean.extend_from_slice(b"  ");
                    i += 2;
                } else {
                    if bytes[i] == b'"' {
                        string = false;
                    }
                    clean.push(if bytes[i] == b'\n' { b'\n' } else { b' ' });
                    i += 1;
                }
            } else if bytes[i..].starts_with(b"/-") {
                depth = 1;
                clean.extend_from_slice(b"  ");
                i += 2;
            } else if bytes[i..].starts_with(b"--") {
                while i < bytes.len() && bytes[i] != b'\n' {
                    clean.push(b' ');
                    i += 1;
                }
            } else if bytes[i] == b'"' {
                string = true;
                clean.push(b' ');
                i += 1;
            } else {
                clean.push(bytes[i]);
                i += 1;
            }
        }
        if depth != 0 || string {
            return Err(bad("fixed source command scan"));
        }
        let clean = String::from_utf8(clean).map_err(|_| bad("fixed source UTF8"))?;
        let mut scopes: Vec<Option<&str>> = Vec::new();
        for line in clean.lines() {
            let mut line = line.trim();
            while let Some(attr) = line.strip_prefix("@[") {
                line = attr
                    .split_once(']')
                    .ok_or_else(|| bad("fixed attribute"))?
                    .1
                    .trim();
            }
            if let Some(namespace) = line.strip_prefix("namespace ") {
                let n = namespace
                    .split_whitespace()
                    .next()
                    .ok_or_else(|| bad("fixed namespace"))?;
                display_name(n)?;
                scopes.push(Some(n));
            } else if line == "section"
                || line.starts_with("section ")
                || line == "noncomputable section"
                || line == "mutual"
            {
                scopes.push(None);
            } else if line == "end" || line.starts_with("end ") {
                scopes.pop().ok_or_else(|| bad("fixed scope"))?;
            } else {
                let declaration = line.strip_prefix("protected ").unwrap_or(line);
                if let Some(rest) = declaration.strip_prefix("theorem ") {
                    let name = rest
                        .split(|c: char| c.is_whitespace() || matches!(c, '(' | '{' | ':' | '['))
                        .next()
                        .ok_or_else(|| bad("fixed theorem"))?;
                    display_name(name)?;
                    let mut parts = scopes.iter().filter_map(|s| *s).collect::<Vec<_>>();
                    parts.push(name);
                    let name = parts.join(".");
                    if !name.starts_with("Vesper.ResourceModel42.")
                        || roots.insert(name, (*module).to_owned()).is_some()
                    {
                        return Err(bad("fixed theorem catalog"));
                    }
                } else if declaration
                    .split_whitespace()
                    .any(|w| matches!(w, "theorem" | "lemma"))
                {
                    return Err(bad("unsupported fixed theorem command"));
                }
            }
        }
        if !scopes.is_empty() {
            return Err(bad("fixed unclosed scope"));
        }
    }
    if roots.len() != 284 {
        return Err(bad("fixed theorem source catalog completeness"));
    }
    for (name, module) in [
        ("validate", "ResourceModel42.Syntax"),
        ("checkProgram", "ResourceModel42.Checker"),
        ("validateInvocation", "ResourceModel42.Evaluator"),
        ("evalSteps", "ResourceModel42.Evaluator"),
    ] {
        if roots
            .insert(format!("Vesper.ResourceModel42.{name}"), module.into())
            .is_some()
        {
            return Err(bad("API catalog collision"));
        }
    }
    Ok(roots)
}

pub(super) fn bind(
    workspace: artifact::Workspace,
    tool_root: PathBuf,
    sysroot: PathBuf,
    receipt: Vec<u8>,
    report_bytes: &[u8],
    limits: &Limits,
) -> Result<PreparedResource, Error> {
    let p: Producer = artifact::bounded_json(report_bytes, 16 * MIB)?;
    let root = &workspace.path;
    let add = |a: u64, b: u64| {
        a.checked_add(b)
            .ok_or_else(|| cap("resource counter overflow"))
    };
    if p.schema_version != 1
        || p.scope != "kernel_logical_projection"
        || p.original_module != ORIGINAL
        || p.projection_module != MODULE
        || Path::new(&p.projection_path) != root.join("projection/ResourceModel42Kernel.olean")
        || p.alias_scope != "synthetic_theorem_proof_variants"
        || !p.exact_inventory
        || !p.constant_info_equal
        || !p.kernel_replayed
        || !p.reimport_verified
        || !p.extensions_empty
        || !p.ir_empty
        || p.original_constants > 65536
        || p.output_constants > 65536
        || p.duplicate_constants > 65536
        || p.original_constant_occurrences > 131072
        || p.logical_expression_nodes > 20_000_000
        || p.original_constants != add(p.retained_constants, p.omitted_constants)?
        || p.original_constant_occurrences != add(p.original_constants, p.duplicate_constants)?
        || p.output_constants != add(p.retained_constants, p.proof_alias_count)?
        || p.proof_alias_count > p.duplicate_constants
        || p.omitted.len() as u64 != p.omitted_constants
        || p.duplicates.len() as u64 != p.duplicate_constants
        || p.occurrences.len() as u64 != p.original_constant_occurrences
        || p.proof_aliases.len() as u64 != p.proof_alias_count
    {
        return Err(bad("resource snapshot scope, flags or counter partition"));
    }
    let mut modules = BTreeSet::new();
    for m in &p.original_modules {
        if !SOURCES.iter().any(|(name, _)| *name == m.name)
            || !modules.insert(m.name.clone())
            || Path::new(&m.path) != root.join("originals").join(module_path(&m.name, "olean"))
        {
            return Err(bad("resource original module association"));
        }
    }
    if modules.len() != 12 || p.trusted_modules.len() > 8192 {
        return Err(bad("resource module closure bounds"));
    }
    let mut trusted = BTreeSet::new();
    for m in &p.trusted_modules {
        if !trusted.insert(m.name.as_str())
            || !["Init", "Std", "Lean", "Lake"].contains(&m.name.split('.').next().unwrap_or(""))
        {
            return Err(bad("resource trusted module name"));
        }
        display_name(&m.name)?;
        let expected = sysroot.join("lib/lean").join(module_path(&m.name, "olean"));
        if Path::new(&m.path) != expected {
            return Err(bad("resource trusted module routing"));
        }
        artifact::regular_meta(&expected)?;
    }
    let mut grouped: BTreeMap<&str, Vec<&Occurrence>> = BTreeMap::new();
    let mut positions = BTreeSet::new();
    let mut module_indices: BTreeMap<&str, BTreeSet<u64>> = BTreeMap::new();
    for o in &p.occurrences {
        display_name(&o.name)?;
        if !modules.contains(&o.module) || !positions.insert((o.module.as_str(), o.index)) {
            return Err(bad("resource occurrence association"));
        }
        module_indices.entry(&o.module).or_default().insert(o.index);
        grouped.entry(&o.name).or_default().push(o);
    }
    for indices in module_indices.values() {
        if indices
            .iter()
            .copied()
            .enumerate()
            .any(|(i, n)| n != i as u64)
        {
            return Err(bad("resource noncontiguous raw module indices"));
        }
    }
    if grouped.len() as u64 != p.original_constants {
        return Err(bad("resource unique original partition"));
    }
    let mut omitted = BTreeMap::new();
    for o in &p.omitted {
        display_name(&o.name)?;
        display_name(&o.parent)?;
        if o.reason != "matched_partial_unsafe_rec"
            || o.name != format!("{}._unsafe_rec", o.parent)
            || !modules.contains(&o.module)
            || omitted.insert(o.name.as_str(), o).is_some()
        {
            return Err(bad("resource omitted auxiliary mapping"));
        }
    }
    let mut aliases = BTreeMap::new();
    for (i, a) in p.proof_aliases.iter().enumerate() {
        if a.name != format!("ResourceKernelProofAliases.realization_{i}")
            || a.reason != "checked_safe_theorem_variant"
            || grouped.contains_key(a.name.as_str())
            || !modules.contains(&a.source_module)
            || !modules.contains(&a.canonical_module)
            || a.original_group.is_empty()
            || a.original_group.len() > 65536
            || !a.original_group.contains(&a.original_name)
            || a.original_group.iter().collect::<BTreeSet<_>>().len() != a.original_group.len()
            || aliases.insert(a.name.as_str(), a).is_some()
        {
            return Err(bad("resource alternate proof mapping"));
        }
        for member in &a.original_group {
            display_name(member)?;
            if !grouped.contains_key(member.as_str()) || omitted.contains_key(member.as_str()) {
                return Err(bad("resource alternate theorem group"));
            }
        }
    }
    let mut expected = BTreeSet::new();
    let mut aliases_seen = BTreeSet::new();
    for (name, occurrences) in &grouped {
        if let Some(omission) = omitted.get(name) {
            if occurrences.len() != 1
                || occurrences[0].module != omission.module
                || occurrences[0].disposition != "omitted_auxiliary"
                || occurrences[0].alias.is_some()
            {
                return Err(bad("resource omission occurrence"));
            }
            let parent = grouped
                .get(omission.parent.as_str())
                .ok_or_else(|| bad("resource omitted parent absent"))?;
            if omitted.contains_key(omission.parent.as_str())
                || !parent.iter().any(|o| {
                    o.module == omission.module && o.disposition == "retained_canonical_info"
                })
            {
                return Err(bad("resource omitted parent module"));
            }
        } else {
            expected.insert((*name).to_owned());
            if !occurrences
                .iter()
                .any(|o| o.disposition == "retained_canonical_info")
            {
                return Err(bad("resource missing canonical raw occurrence"));
            }
            for o in occurrences {
                match o.disposition.as_str() {
                    "retained_canonical_info" if o.alias.is_none() => {}
                    "proof_alias" => {
                        let label = o
                            .alias
                            .as_deref()
                            .ok_or_else(|| bad("resource missing alias label"))?;
                        let a = aliases
                            .get(label)
                            .ok_or_else(|| bad("resource missing alias descriptor"))?;
                        if a.original_name != *name
                            || a.source_module != o.module
                            || a.source_occurrence != o.index
                            || !aliases_seen.insert(label)
                            || !occurrences.iter().any(|canonical| {
                                canonical.module == a.canonical_module
                                    && canonical.disposition == "retained_canonical_info"
                            })
                        {
                            return Err(bad("resource alias occurrence provenance"));
                        }
                    }
                    _ => return Err(bad("resource occurrence disposition")),
                }
            }
        }
    }
    if expected.len() as u64 != p.retained_constants || aliases_seen.len() != aliases.len() {
        return Err(bad("resource retained/alias partition"));
    }
    let mut duplicates: BTreeMap<(&str, &str, &str), usize> = BTreeMap::new();
    for (name, occurrences) in &grouped {
        let first = occurrences[0];
        for repeated in &occurrences[1..] {
            *duplicates
                .entry((name, first.module.as_str(), repeated.module.as_str()))
                .or_default() += 1;
        }
    }
    for d in &p.duplicates {
        if !matches!(
            d.reason.as_str(),
            "identical_safe_theorem_realization" | "safe_theorem_proof_variant"
        ) {
            return Err(bad("resource duplicate reason"));
        }
        let count = duplicates
            .get_mut(&(
                d.name.as_str(),
                d.first_module.as_str(),
                d.repeated_module.as_str(),
            ))
            .ok_or_else(|| bad("resource duplicate occurrence association"))?;
        if *count == 0 {
            return Err(bad("resource duplicate repeated occurrence"));
        }
        *count -= 1;
    }
    if duplicates.values().any(|n| *n != 0) {
        return Err(bad("resource missing duplicate descriptor"));
    }
    for name in aliases.keys() {
        expected.insert((*name).to_owned());
    }
    if expected.len() as u64 != p.output_constants {
        return Err(bad("resource exact output partition"));
    }
    let origins = catalog()?;
    for (name, module) in &origins {
        if !grouped.get(name.as_str()).is_some_and(|os| {
            os.iter()
                .any(|o| o.module == *module && o.disposition == "retained_canonical_info")
        }) {
            return Err(bad("resource catalog source occurrence"));
        }
    }
    if omitted.keys().any(|name| !grouped.contains_key(name)) {
        return Err(bad("resource unassociated omitted entry"));
    }
    let files = artifact::resource_files(root, limits)?;
    let mut expected_files = BTreeSet::new();
    for (module, bytes) in SOURCES {
        let source = format!(
            "sources/{}",
            module_path(module, "lean")
                .to_string_lossy()
                .replace('\\', "/")
        );
        let object = format!(
            "originals/{}",
            module_path(module, "olean")
                .to_string_lossy()
                .replace('\\', "/")
        );
        let actual = files
            .iter()
            .find(|f| f.path == source)
            .ok_or_else(|| bad("missing fixed resource source"))?;
        if actual.bytes != bytes.len() as u64 || actual.blake3 != hash(bytes) {
            return Err(bad("fixed compiled resource source changed"));
        }
        expected_files.insert(source);
        expected_files.insert(object);
    }
    for (path, bytes) in [
        ("projector/KernelSnapshot.lean", TOOL),
        ("historical-source-manifest.json", HISTORICAL),
        ("resource-source-manifest.json", BUNDLE),
    ] {
        let actual = files
            .iter()
            .find(|f| f.path == path)
            .ok_or_else(|| bad("missing fixed resource metadata"))?;
        if actual.bytes != bytes.len() as u64 || actual.blake3 != hash(bytes) {
            return Err(bad("fixed resource metadata changed"));
        }
        expected_files.insert(path.into());
    }
    for path in [
        "request.json",
        "producer-report.json",
        "projection/ResourceModel42Kernel.olean",
    ] {
        expected_files.insert(path.into());
    }
    if files.len() != expected_files.len()
        || files.iter().any(|f| !expected_files.contains(&f.path))
    {
        return Err(bad("unexpected resource file or sidecar"));
    }

    let file = |name: &str| {
        files
            .iter()
            .find(|f| f.path == name)
            .cloned()
            .ok_or_else(|| bad("missing prepared resource file"))
    };
    let originals = files
        .iter()
        .filter(|f| f.path.starts_with("originals/"))
        .cloned()
        .collect::<Vec<_>>();
    if originals.len() != 12 || originals.iter().any(|f| !f.path.ends_with(".olean")) {
        return Err(bad("resource original object sidecars"));
    }
    let projection_files = files
        .iter()
        .filter(|f| f.path.starts_with("projection/"))
        .collect::<Vec<_>>();
    if projection_files.len() != 1
        || projection_files[0].path != "projection/ResourceModel42Kernel.olean"
    {
        return Err(bad("resource projected sidecar envelope"));
    }
    let association = ResourceAssociation {
        schema: 1,
        scope: "kernel_logical_projection".into(),
        module: MODULE.into(),
        resource_bundle_blake3: verify_bundle()?,
        source_manifest_blake3: hash(HISTORICAL),
        snapshot_tool: file("projector/KernelSnapshot.lean")?,
        request: file("request.json")?,
        producer_report: file("producer-report.json")?,
        projection: file("projection/ResourceModel42Kernel.olean")?,
        original_manifest_blake3: artifact::manifest_hash(
            b"vesper.lean4lean42.resource-original-objects/1\0",
            &originals,
        ),
        retained_constants: p.retained_constants,
        omitted_constants: p.omitted_constants,
        proof_aliases: p.proof_alias_count,
        output_constants: p.output_constants,
    };
    if association.producer_report.blake3 != hash(report_bytes) {
        return Err(bad("resource captured producer report association"));
    }
    let alias_originals = p
        .proof_aliases
        .iter()
        .map(|a| a.original_name.clone())
        .collect();
    let omission_parents = p.omitted.iter().map(|o| o.parent.clone()).collect();
    let aliases = p.proof_aliases.into_iter().map(|a| a.name).collect();
    let catalog = origins
        .keys()
        .filter(|n| {
            !matches!(
                n.rsplit('.').next(),
                Some("validate" | "checkProgram" | "validateInvocation" | "evalSteps")
            )
        })
        .cloned()
        .collect();
    Ok(PreparedResource {
        workspace,
        sysroot,
        tool_root,
        receipt,
        files,
        association,
        expected,
        aliases,
        catalog,
        origins,
        alias_originals,
        omission_parents,
    })
}
