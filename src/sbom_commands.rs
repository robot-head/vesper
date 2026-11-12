//! Deterministic CycloneDX inventory of validated, declared snapshot metadata.
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
};
use vesper_loom_calculus::{PackageName, Solution, validate_solution};

const HELP: &str = "Usage: vesper sbom [Loom.lock]\n\nEmit CycloneDX 1.6 JSON for the reachable Vesper/native package graph.\nThe entire snapshot is validated first. This inventories declared metadata,\nnot authenticated sources, compiler authority, actual native installations or CVEs.\nNo files are written and no package/build code executes.\n";

pub fn run(args: Vec<String>) -> Result<(), String> {
    let mut input = None;
    for arg in args {
        match arg.as_str() {
            "--help" | "-h" => {
                print!("{HELP}");
                return Ok(());
            }
            _ if arg.starts_with('-') => return Err(format!("unknown sbom option '{arg}'")),
            _ => {
                if input.replace(arg).is_some() {
                    return Err("sbom accepts one solution snapshot".into());
                }
            }
        }
    }
    let input = input.as_deref().unwrap_or("Loom.lock");
    let bytes = fs::read(input).map_err(|e| format!("{input}: {e}"))?;
    let solution =
        serde_json::from_slice(&bytes).map_err(|e| format!("{input}: invalid snapshot: {e}"))?;
    let bom = build(&solution)?;
    println!(
        "{}",
        serde_json::to_string_pretty(&bom).map_err(|e| e.to_string())?
    );
    Ok(())
}

fn encode(value: &str) -> String {
    let mut encoded = String::new();
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            encoded.push(byte as char);
        } else {
            encoded.push('%');
            encoded.push(HEX[(byte >> 4) as usize] as char);
            encoded.push(HEX[(byte & 15) as usize] as char);
        }
    }
    encoded
}

fn package_ref(name: &PackageName, version: &str) -> String {
    // PackageName guarantees exactly two components. Preserve the namespace's
    // leading @: @std/x and the reserved std/x must never collapse.
    let (namespace, package) = name
        .as_str()
        .split_once('/')
        .expect("validated package identity");
    format!(
        "pkg:generic/vesper/{}/{}@{}",
        encode(namespace),
        encode(package),
        encode(version)
    )
}

fn native_ref(name: &str, version: &str) -> String {
    format!(
        "pkg:generic/vesper-native/{}@{}",
        encode(name),
        encode(version)
    )
}

fn property(name: &str, value: impl Serialize) -> Result<Value, String> {
    Ok(json!({"name":name,"value":serde_json::to_string(&value).map_err(|e|e.to_string())?}))
}

fn build(solution: &Solution) -> Result<Value, String> {
    let validation = validate_solution(solution);
    if !validation.diagnostics.is_empty() {
        return Err(format!(
            "invalid snapshot: {}",
            serde_json::to_string(&validation.diagnostics).map_err(|e| e.to_string())?
        ));
    }
    let graph = validation
        .graph
        .ok_or("snapshot validation produced no graph")?;
    if graph.roots.is_empty() {
        return Err("SBOM requires a nonempty root graph".into());
    }
    let manifests = solution
        .packages
        .iter()
        .map(|s| (&s.manifest.package.name, &s.manifest))
        .collect::<BTreeMap<_, _>>();
    let package_refs = graph
        .packages
        .iter()
        .map(|(name, node)| (name.clone(), package_ref(name, &node.version.to_string())))
        .collect::<BTreeMap<_, _>>();
    let native_refs = graph
        .system_packages
        .iter()
        .map(|(name, version)| (name.clone(), native_ref(name, &version.to_string())))
        .collect::<BTreeMap<_, _>>();
    let mut components = BTreeMap::<String, Value>::new();
    let mut dependencies = BTreeMap::<String, BTreeSet<String>>::new();
    for (name, node) in &graph.packages {
        let reference = &package_refs[name];
        let manifest = manifests
            .get(name)
            .ok_or("validated graph has no package manifest")?;
        let mut properties = vec![
            json!({"name":"vesper:qualified-name","value":name.as_str()}),
            property("vesper:features", &node.features)?,
            property("vesper:capability-ceiling", &node.capability_ceiling)?,
            property("vesper:required-capabilities", &node.required_capabilities)?,
            property("vesper:forbid-unsafe-ffi", node.forbid_unsafe_ffi)?,
            property("vesper:dependency-contracts", &node.dependencies)?,
            property("vesper:native-requirements", &node.system_dependencies)?,
        ];
        // Manifest license text is not necessarily a validated SPDX expression.
        if let Some(license) = &manifest.package.license {
            properties.push(json!({"name":"vesper:declared-license","value":license}));
        }
        components.insert(
            reference.clone(),
            json!({
                "type":if graph.roots.contains(name) {"application"} else {"library"},
                "bom-ref":reference,"name":name.as_str(),"version":node.version.to_string(),
                "purl":reference,"properties":properties
            }),
        );
        let mut targets = BTreeSet::new();
        for edge in &node.dependencies {
            targets.insert(
                package_refs
                    .get(&edge.package)
                    .ok_or("validated graph has an unresolved package edge")?
                    .clone(),
            );
        }
        for edge in &node.system_dependencies {
            targets.insert(
                native_refs
                    .get(&edge.name)
                    .ok_or("validated graph has an unresolved native edge")?
                    .clone(),
            );
        }
        dependencies.insert(reference.clone(), targets);
    }
    for (name, version) in &graph.system_packages {
        let reference = &native_refs[name];
        components.insert(reference.clone(),json!({
            "type":"library","bom-ref":reference,"name":name,"version":version.to_string(),"purl":reference,
            "properties":[{"name":"vesper:identity-model","value":"local-catalog-native-semver"}]
        }));
        dependencies.insert(reference.clone(), BTreeSet::new());
    }
    let unused_native = solution
        .system_packages
        .iter()
        .filter(|s| !graph.system_packages.contains_key(&s.name))
        .map(|s| &s.name)
        .collect::<BTreeSet<_>>();
    let mut metadata_properties = vec![
        json!({"name":"vesper:inventory-basis","value":"declared_metadata"}),
        property("vesper:snapshot-roots", &graph.roots)?,
        property("vesper:unused-packages", &validation.unused_selections)?,
        property("vesper:unused-native-packages", unused_native)?,
    ];
    metadata_properties.push(
        json!({"name":"vesper:source-and-installation-verification","value":"not_performed"}),
    );
    Ok(json!({
        "$schema":"http://cyclonedx.org/schema/bom-1.6.schema.json",
        "bomFormat":"CycloneDX","specVersion":"1.6","version":1,
        "metadata":{"properties":metadata_properties},
        "components":components.into_values().collect::<Vec<_>>(),
        "dependencies":dependencies.into_iter().map(|(reference,targets)|json!({"ref":reference,"dependsOn":targets})).collect::<Vec<_>>()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purls_preserve_namespaces_domains_build_metadata_and_utf8() {
        let reserved = PackageName::parse("std/lib").unwrap();
        let third_party = PackageName::parse("@std/lib").unwrap();
        assert_ne!(
            package_ref(&reserved, "1.0.0"),
            package_ref(&third_party, "1.0.0")
        );
        assert_eq!(
            package_ref(&third_party, "1.0.0+build"),
            "pkg:generic/vesper/%40std/lib@1.0.0%2Bbuild"
        );
        assert_ne!(native_ref("lib", "1.0.0"), package_ref(&reserved, "1.0.0"));
        assert_eq!(encode("libc++"), "libc%2B%2B");
        assert_eq!(encode("日本"), "%E6%97%A5%E6%9C%AC");
    }
}
