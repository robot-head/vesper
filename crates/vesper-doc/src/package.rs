//! Package-level pages: the `impl`/`model` method listing of a file page and
//! the overview page of a package bundle.
use crate::{
    capabilities::{self, PackageCapabilities},
    hoogle::ImplMethod,
    visual::escape,
};
use serde::{Deserialize, Serialize};

/// One documented file of a package overview.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileSummary {
    pub file: String,
    pub href: String,
    pub module_docs: String,
    pub functions: usize,
    pub types: usize,
    pub impl_methods: usize,
}

/// The impl and model methods of one file, for its page.
pub fn render_impls(impls: &[ImplMethod]) -> String {
    let mut out = String::from(
        "<section id=\"codex-impls\"><h2>Impls and Pact models</h2><p>Methods of source <code>impl</code> and <code>model</code> declarations, indexed by signature search under their conformance.</p>",
    );
    if impls.is_empty() {
        out.push_str("<p>No impl or model declaration.</p>");
    } else {
        out.push_str("<ul>");
        for m in impls {
            out.push_str(&format!(
                "<li><code>{}</code>: {} of <code>{}</code> for <code>{}</code>{}</li>",
                escape(&m.path()),
                if m.model.is_some() {
                    "model method"
                } else {
                    "impl method"
                },
                escape(&m.trait_ref()),
                escape(&m.self_ty),
                if m.public { "" } else { " (private model)" }
            ));
        }
        out.push_str("</ul>");
    }
    out.push_str("</section>");
    out
}

/// The overview page of a package bundle: files, package capabilities and a
/// link to the package-wide search page.
pub fn render_overview(
    name: &str,
    files: &[FileSummary],
    caps: &PackageCapabilities,
    search_href: &str,
) -> String {
    let mut out = format!(
        "<!doctype html><html lang=\"en\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>{0} - Vesper Codex</title><style>body{{max-width:1000px;margin:2rem auto;padding:1rem;font:16px system-ui}}code{{font:14px ui-monospace,monospace}}</style><h1>{0}</h1><p><a href=\"{1}\">Package-wide signature search</a></p>",
        escape(name),
        escape(search_href)
    );
    out.push_str("<section id=\"codex-files\"><h2>Files</h2><ul>");
    for f in files {
        out.push_str(&format!(
            "<li><a href=\"{}\"><code>{}</code></a>: {} function(s), {} type(s), {} impl/model method(s){}</li>",
            escape(&f.href),
            escape(&f.file),
            f.functions,
            f.types,
            f.impl_methods,
            if f.module_docs.is_empty() {
                String::new()
            } else {
                format!(" &mdash; {}", escape(f.module_docs.lines().next().unwrap_or("")))
            }
        ));
    }
    out.push_str(
        "</ul></section><section id=\"codex-package-capabilities\"><h2>Package capabilities</h2>",
    );
    out.push_str(&capabilities::render_package(caps));
    out.push_str("</section></html>\n");
    out
}
