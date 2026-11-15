//! Fixed Lean-theory entry point. No trusted prefix, parse-only mode, or unchecked indexing.
#![forbid(unsafe_code)]

use logicaffeine_kernel::dag::{
    check_export_in,
    env::Kind,
    export::{Observe, Stats},
    name::NameId,
    tc::{Engine, Theory},
    Kernel, Verdict,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::HashSet,
    fs,
    io::{BufReader, Cursor},
    path::{Path, PathBuf},
};

const MAX_EXPORT: u64 = 2 * 1024 * 1024 * 1024;
const MAX_MANIFEST: u64 = 64 * 1024 * 1024;

fn bounded(path: &Path, cap: u64) -> Result<Vec<u8>, String> {
    if !fs::symlink_metadata(path)
        .map_err(|e| e.to_string())?
        .is_file()
        || fs::metadata(path).map_err(|e| e.to_string())?.len() > cap
    {
        return Err("input file type or byte limit".into());
    }
    let bytes = fs::read(path).map_err(|e| e.to_string())?;
    if bytes.len() as u64 > cap {
        return Err("input grew beyond byte limit".into());
    }
    Ok(bytes)
}

fn kernel() -> Kernel {
    let mut k = Kernel::new();
    k.engine = Engine::Sem;
    k.theory = Theory::Lean;
    k
}

fn verdict(v: &Verdict) -> (bool, &'static str) {
    match v {
        Verdict::Accept => (true, "accepted"),
        Verdict::Reject(_) => (false, "rejected"),
        Verdict::Decline(_) => (false, "unsupported"),
    }
}

fn name(k: &mut Kernel, components: &Value) -> Result<NameId, String> {
    let parts = components.as_array().ok_or("name components")?;
    if parts.is_empty() || parts.len() > 128 {
        return Err("name depth".into());
    }
    let mut n = NameId::ANON;
    for part in parts {
        let a = part.as_array().ok_or("name component")?;
        if a.len() != 2 {
            return Err("name component arity".into());
        }
        let text = a[1].as_str().ok_or("name component payload")?;
        n = match a[0].as_u64() {
            Some(0) if !text.is_empty() && text.len() <= 1024 => k.s.names.str(n, text),
            Some(1) => k.s.names.num(n, text.parse().map_err(|_| "numeric name")?),
            _ => return Err("name component tag".into()),
        };
    }
    Ok(n)
}

fn verify_inventory(k: &mut Kernel, expected: &Value) -> Result<usize, String> {
    let entries = expected.as_array().ok_or("expected inventory array")?;
    if entries.is_empty() || entries.len() > 524288 {
        return Err("inventory count".into());
    }
    let mut seen = HashSet::new();
    for entry in entries {
        let a = entry.as_array().ok_or("inventory entry")?;
        if a.len() != 2 {
            return Err("inventory entry arity".into());
        }
        let n = name(k, &a[0])?;
        if !seen.insert(n) {
            return Err("duplicate expected declaration".into());
        }
        let c = k
            .env
            .get(n)
            .ok_or("Logos omitted an eligible declaration")?;
        if c.is_unsafe() || c.is_partial() || !k.env.is_settled(n) {
            return Err("unsettled or skipped declaration in checked environment".into());
        }
        let form = match c.kind {
            Kind::Axiom { .. } => 0,
            Kind::Defn { .. } => 1,
            Kind::Thm { .. } => 2,
            Kind::Opaque { .. } => 3,
            Kind::Induct(_) => 4,
            Kind::Ctor(_) => 5,
            Kind::Rec(_) => 6,
            Kind::Quot(_) => 7,
        };
        if a[1].as_u64() != Some(form) {
            return Err("checked declaration kind changed".into());
        }
    }
    if k.env.constants().count() != seen.len() {
        return Err("checked environment contains an unassociated declaration".into());
    }
    Ok(seen.len())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    id: String,
    expected_accept: bool,
    expected_reason: String,
    input: String,
}

fn controls(root: &Path) -> Result<Value, String> {
    let cases: Vec<Control> = serde_json::from_slice(&bounded(&root.join("cases.json"), 65536)?)
        .map_err(|e| e.to_string())?;
    if cases.is_empty() || cases.len() > 256 {
        return Err("control count".into());
    }
    let mut rows = Vec::new();
    let mut seen = HashSet::new();
    for case in cases {
        if !seen.insert(case.id.clone()) || case.input.contains(['/', '\\']) || case.input == ".." {
            return Err("control identity".into());
        }
        let bytes = bounded(&root.join(case.input), 4 * 1024 * 1024)?;
        let mut k = kernel();
        let (v, _) = check_export_in(&mut k, Cursor::new(bytes), None, Observe::default());
        let (accepted, reason) = verdict(&v);
        rows.push(json!({"id":case.id,"expected_accept":case.expected_accept,
            "actual_accept":accepted,"expected_reason":case.expected_reason,
            "actual_reason":reason,
            "matched":accepted == case.expected_accept && reason == case.expected_reason}));
    }
    Ok(Value::Array(rows))
}

fn check(input: &Path, manifest: &Path) -> Result<Value, String> {
    let expected: Value =
        serde_json::from_slice(&bounded(manifest, MAX_MANIFEST)?).map_err(|e| e.to_string())?;
    let f = fs::File::open(input).map_err(|e| e.to_string())?;
    let bytes = f.metadata().map_err(|e| e.to_string())?.len();
    if bytes == 0 || bytes > MAX_EXPORT {
        return Err("export byte limit".into());
    }
    let mut k = kernel();
    let (v, stats): (Verdict, Stats) = check_export_in(
        &mut k,
        BufReader::with_capacity(1024 * 1024, f),
        Some(bytes),
        Observe::default(),
    );
    match v {
        Verdict::Accept => {}
        Verdict::Reject(e) => return Err(format!("Logos rejected: {e}")),
        Verdict::Decline(e) => return Err(format!("Logos declined: {e}")),
    }
    let count = verify_inventory(&mut k, &expected)?;
    // These primitives occur in all three fixed closures. Refuse a port that
    // silently loses their validation and falls back to unbounded normalization.
    for primitive in [
        "Nat.add",
        "Nat.sub",
        "Nat.mul",
        "Nat.div",
        "Nat.mod",
        "Nat.gcd",
        "Nat.bitwise",
        "Nat.shiftLeft",
        "Nat.shiftRight",
        "Char.ofNat",
        "String.ofList",
    ] {
        if !stats.validated_primitives.iter().any(|p| p == primitive) {
            return Err(format!("primitive was not validated: {primitive}"));
        }
    }
    let result = json!({"checked":count,"declarations":stats.declarations,
        "validated_primitives":stats.validated_primitives});
    // The process is about to exit; avoid recursively dropping a multi-million-node DAG.
    std::mem::forget(k);
    Ok(result)
}

fn run() -> Result<Value, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    match args.as_slice() {
        [mode, root] if mode == "--controls" => controls(&PathBuf::from(root)),
        [mode, input, manifest] if mode == "--check-manifest" => {
            check(&PathBuf::from(input), &PathBuf::from(manifest))
        }
        _ => Err("expected --controls ROOT or --check-manifest EXPORT INVENTORY".into()),
    }
}

fn main() {
    let worker = std::thread::Builder::new()
        .name("logos-kernel".into())
        .stack_size(8usize << 30)
        .spawn(run)
        .expect("checker thread");
    match worker.join() {
        Ok(Ok(result)) => println!("{result}"),
        Ok(Err(e)) => {
            eprintln!("{}", e.chars().take(4096).collect::<String>());
            std::process::exit(1);
        }
        Err(_) => {
            eprintln!("Logos checker panicked");
            std::process::exit(1);
        }
    }
}
