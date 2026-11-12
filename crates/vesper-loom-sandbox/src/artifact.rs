//! Classification of WebAssembly binaries before compilation.
//!
//! The sandbox executes only `wasm32-wasip2` components. A preview1
//! (`wasm32-wasip1`) core module is a different artifact with a different
//! import surface (`wasi_snapshot_preview1`), so it is rejected with a
//! precise classification rather than an opaque compile error. Adapting
//! preview1 modules (`wasi_snapshot_preview1.reactor.wasm`) is out of scope.

use serde::{Deserialize, Serialize};
use wasmparser::{Encoding, Parser, Payload};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArtifactKind {
    /// A component-model binary (the only kind this host runs).
    Component,
    /// A core module importing `wasi_snapshot_preview1` (or the older
    /// `wasi_unstable`), i.e. a `wasm32-wasip1` build.
    Preview1Module,
    /// A core module with no WASI preview1 imports.
    CoreModule,
    /// Not a parseable WebAssembly binary header.
    NotWasm,
}

/// Classifies `bytes` by header and, for core modules, by import namespaces.
pub fn classify(bytes: &[u8]) -> ArtifactKind {
    let mut encoding = None;
    for payload in Parser::new(0).parse_all(bytes) {
        let Ok(payload) = payload else {
            return encoding.map_or(ArtifactKind::NotWasm, |e| match e {
                Encoding::Component => ArtifactKind::Component,
                Encoding::Module => ArtifactKind::CoreModule,
            });
        };
        match payload {
            Payload::Version { encoding: e, .. } => {
                if encoding.is_some() {
                    // Nested module/component inside a component.
                    continue;
                }
                encoding = Some(e);
                if e == Encoding::Component {
                    return ArtifactKind::Component;
                }
            }
            Payload::ImportSection(reader) => {
                for import in reader.into_imports() {
                    let Ok(import) = import else { break };
                    if import.module == "wasi_snapshot_preview1" || import.module == "wasi_unstable"
                    {
                        return ArtifactKind::Preview1Module;
                    }
                }
            }
            Payload::End(_) => break,
            _ => {}
        }
    }
    match encoding {
        Some(Encoding::Module) => ArtifactKind::CoreModule,
        Some(Encoding::Component) => ArtifactKind::Component,
        None => ArtifactKind::NotWasm,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_and_garbage_are_not_wasm() {
        assert_eq!(classify(b""), ArtifactKind::NotWasm);
        assert_eq!(classify(b"\x7fELF\x02\x01\x01"), ArtifactKind::NotWasm);
    }

    #[test]
    fn bare_core_module_header() {
        assert_eq!(classify(b"\0asm\x01\0\0\0"), ArtifactKind::CoreModule);
    }

    #[test]
    fn bare_component_header() {
        assert_eq!(classify(b"\0asm\x0d\0\x01\0"), ArtifactKind::Component);
    }
}
