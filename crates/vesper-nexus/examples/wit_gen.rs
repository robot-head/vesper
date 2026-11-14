//! Print the bindings generated for a WIT file (default: `PLUGIN_WIT`).
//!
//! ```sh
//! cargo run -p vesper-nexus --example wit_gen -- [file.wit [world]] [--rust]
//! ```

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rust = args.iter().any(|a| a == "--rust");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let source = match positional.first() {
        Some(path) => std::fs::read_to_string(path).unwrap_or_else(|e| {
            eprintln!("{path}: {e}");
            std::process::exit(2)
        }),
        None => vesper_nexus::PLUGIN_WIT.to_string(),
    };
    let world = positional.get(1).map(|s| s.as_str());
    match vesper_nexus::wit_gen::generate(&source, world) {
        Ok(b) => print!("{}", if rust { b.rust } else { b.vesper }),
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1)
        }
    }
}
