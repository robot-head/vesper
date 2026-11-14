//! Layout checks of `.vsys` wire structs against a C header.
//!
//! A `.vsys` `wire struct` restates a C struct. Nothing in the descriptor ties
//! the two together, so a reordered field or a changed width in the header
//! would silently corrupt every slice of that struct. This module renders a
//! tiny C program that prints the compiler's view of the struct (`sizeof`,
//! `_Alignof`, and `offsetof`/`sizeof` of every field, nested fields
//! included), and checks that output against the compiled wire layout.
//!
//! The library never runs a compiler; the caller compiles and runs the probe
//! with the target's C compiler and passes the output back. Output is
//! untrusted text: malformed output is a typed mismatch, never a panic.
use super::*;

/// First line of every probe's output.
pub const C_LAYOUT_SCHEMA: &str = "vesper-c-layout/1";
/// Largest accepted probe output.
pub const MAX_PROBE_OUTPUT: usize = 64 * 1024;

/// One way the C compiler's layout differs from the wire layout.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CLayoutMismatch {
    /// The probe output is not what the probe prints (line is 1-based; 0
    /// means the output as a whole, for example over budget or truncated).
    Malformed { line: usize },
    /// `sizeof` differs: slice strides would disagree.
    Size { wire: usize, c: usize },
    /// C requires a stronger alignment than the wire layout checks for, so a
    /// slice the Bridge accepts could be misaligned for C.
    Alignment { wire: usize, c: usize },
    FieldOffset {
        field: String,
        wire: usize,
        c: usize,
    },
    FieldSize {
        field: String,
        wire: usize,
        c: usize,
    },
    /// A wire field the probe did not report.
    MissingField { field: String },
}

fn c_ident(s: &str) -> bool {
    s.as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// Every field path of a layout with its offset and size, nested first-level
/// struct fields expanded as `outer.inner`.
fn paths(
    layout: &crate::WireLayout,
    prefix: &str,
    base: usize,
    out: &mut Vec<(String, usize, usize)>,
) {
    for f in layout.fields() {
        let path = if prefix.is_empty() {
            f.name().to_owned()
        } else {
            format!("{prefix}.{}", f.name())
        };
        out.push((path.clone(), base + f.offset(), f.size()));
        if let crate::FieldKind::Struct(nested) = f.kind() {
            paths(nested, &path, base + f.offset(), out);
        }
    }
}

impl WireStructDescriptor {
    /// C11 source of a program that prints how the C compiler lays out
    /// `c_type` (`name` or `struct name`) declared in `include` (a normalized
    /// relative path, included with quotes).
    pub fn c_layout_probe(&self, c_type: &str, include: &str) -> Result<String, Error> {
        let ty_ok = match c_type.strip_prefix("struct ") {
            Some(tag) => c_ident(tag),
            None => c_ident(c_type),
        };
        if !ty_ok {
            return Err(Error::new(
                ErrorKind::Syntax,
                0..0,
                "C type must be an identifier or `struct identifier`",
            ));
        }
        if !parser::header(include) {
            return Err(Error::new(
                ErrorKind::Link,
                0..0,
                "include must be a normalized relative path",
            ));
        }
        let mut fields = vec![];
        paths(self.layout(), "", 0, &mut fields);
        let mut src = format!(
            "#include <stddef.h>\n#include <stdio.h>\n#include \"{include}\"\nint main(void) {{\n    printf(\"{C_LAYOUT_SCHEMA}\\n\");\n    printf(\"size %zu\\n\", sizeof({c_type}));\n    printf(\"align %zu\\n\", (size_t)_Alignof({c_type}));\n"
        );
        for (path, _, _) in &fields {
            if !path.split('.').all(c_ident) {
                return Err(Error::new(
                    ErrorKind::Syntax,
                    0..0,
                    format!("field `{path}` is not a C identifier"),
                ));
            }
            src.push_str(&format!(
                "    printf(\"field {path} %zu %zu\\n\", offsetof({c_type}, {path}), sizeof((({c_type} *)0)->{path}));\n"
            ));
        }
        src.push_str("    return 0;\n}\n");
        Ok(src)
    }

    /// Compare probe output with the wire layout. Every mismatch is returned.
    pub fn check_c_layout(&self, output: &str) -> Result<(), Vec<CLayoutMismatch>> {
        use CLayoutMismatch::*;
        if output.len() > MAX_PROBE_OUTPUT {
            return Err(vec![Malformed { line: 0 }]);
        }
        let mut lines = output.lines().enumerate();
        if lines.next().map(|(_, l)| l) != Some(C_LAYOUT_SCHEMA) {
            return Err(vec![Malformed { line: 1 }]);
        }
        let mut size = None;
        let mut align = None;
        let mut c_fields: Vec<(String, usize, usize)> = vec![];
        for (n, line) in lines {
            let words: Vec<&str> = line.split(' ').collect();
            let num = |w: &str| w.parse::<usize>().ok();
            match words.as_slice() {
                ["size", v] if size.is_none() => {
                    size = Some(num(v).ok_or(vec![Malformed { line: n + 1 }])?)
                }
                ["align", v] if align.is_none() => {
                    align = Some(num(v).ok_or(vec![Malformed { line: n + 1 }])?)
                }
                ["field", name, off, sz] if !c_fields.iter().any(|f| f.0 == *name) => {
                    match (num(off), num(sz)) {
                        (Some(o), Some(s)) => c_fields.push(((*name).to_owned(), o, s)),
                        _ => return Err(vec![Malformed { line: n + 1 }]),
                    }
                }
                [""] => {}
                _ => return Err(vec![Malformed { line: n + 1 }]),
            }
        }
        let (Some(c_size), Some(c_align)) = (size, align) else {
            return Err(vec![Malformed { line: 0 }]);
        };
        let layout = self.layout();
        let mut problems = vec![];
        if c_size != layout.size() {
            problems.push(Size {
                wire: layout.size(),
                c: c_size,
            });
        }
        if c_align > layout.alignment() {
            problems.push(Alignment {
                wire: layout.alignment(),
                c: c_align,
            });
        }
        let mut wire = vec![];
        paths(layout, "", 0, &mut wire);
        for (path, offset, size) in wire {
            match c_fields.iter().find(|f| f.0 == path) {
                None => problems.push(MissingField { field: path }),
                Some((_, o, s)) => {
                    if *o != offset {
                        problems.push(FieldOffset {
                            field: path.clone(),
                            wire: offset,
                            c: *o,
                        });
                    }
                    if *s != size {
                        problems.push(FieldSize {
                            field: path,
                            wire: size,
                            c: *s,
                        });
                    }
                }
            }
        }
        if problems.is_empty() {
            Ok(())
        } else {
            Err(problems)
        }
    }
}
