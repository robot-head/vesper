//! Fixed Lean AST witnesses for a sealed projection of the actual formatter Doc.
//! Hash definitions are metadata; the theorem proves the document algebra only.
use crate::proof_model::{DocumentModel, ModelDoc, ProofError, ProofLimits};
use vesper_lean_syntax::{
    BinaryOp, Binder, Declaration, Definition, DefinitionKind, Ident, LeanOption, Module,
    ModuleName, Name, OptionValue, Term,
};

fn add(a: usize, b: usize) -> Result<usize, ProofError> {
    a.checked_add(b)
        .ok_or(ProofError::Resource("emitter arithmetic"))
}
fn mul(a: usize, b: usize) -> Result<usize, ProofError> {
    a.checked_mul(b)
        .ok_or(ProofError::Resource("emitter arithmetic"))
}
fn bound(value: usize, maximum: usize, name: &'static str) -> Result<(), ProofError> {
    if value > maximum {
        Err(ProofError::Resource(name))
    } else {
        Ok(())
    }
}

// The central printer escapes every string scalar below in this way. This
// computes its byte length without allocating a temporary escaped string.
fn quoted_bytes(value: &str) -> Result<usize, ProofError> {
    value.chars().try_fold(2, |count, c| {
        add(
            count,
            match c {
                '\\' | '\n' | '\r' | '\t' | '"' => 2,
                c if c.is_control() && (c as u32) <= 255 => 4,
                '\u{2028}' | '\u{2029}' => 6,
                c => c.len_utf8(),
            },
        )
    })
}

#[derive(Default)]
struct Preflight {
    nodes: usize,
    text: usize,
    comments: usize,
    depth: usize,
    ast_nodes: usize,
    quoted_bytes: usize,
    work: usize,
}
impl Preflight {
    fn charge(&mut self, amount: usize, limits: ProofLimits) -> Result<(), ProofError> {
        self.work = add(self.work, amount)?;
        bound(self.work, limits.max_work, "emitter work")
    }
    fn visit(
        &mut self,
        doc: &ModelDoc,
        depth: usize,
        limits: ProofLimits,
    ) -> Result<usize, ProofError> {
        self.charge(64, limits)?;
        bound(depth, limits.max_depth, "emitter depth")?;
        self.depth = self.depth.max(depth);
        self.nodes = add(self.nodes, 1)?;
        bound(self.nodes, limits.max_nodes, "emitter nodes")?;
        // Constructor application, its fixed name and optional list/nat node.
        // Actual child terms are counted recursively, including every occurrence.
        self.ast_nodes = add(self.ast_nodes, 4)?;
        let mut child_depth = 0;
        match doc {
            ModelDoc::Text(text) | ModelDoc::Comment(text) => {
                self.text = add(self.text, text.len())?;
                bound(self.text, limits.max_text_bytes, "emitter text")?;
                self.charge(text.len(), limits)?;
                self.quoted_bytes = add(self.quoted_bytes, quoted_bytes(text)?)?;
                if matches!(doc, ModelDoc::Comment(_)) {
                    self.comments = add(self.comments, 1)?;
                }
                child_depth = 1;
            }
            ModelDoc::Nest(_, child) | ModelDoc::Group(child) => {
                child_depth = self.visit(child, add(depth, 1)?, limits)?;
            }
            ModelDoc::Concat(children) => {
                // Every child contributes an AST slot. Do not reserve or build
                // that vector until this entire read-only preflight succeeds.
                bound(children.len(), limits.max_nodes, "emitter list")?;
                for child in children {
                    child_depth = child_depth.max(self.visit(child, add(depth, 1)?, limits)?);
                }
                child_depth = add(child_depth, 1)?; // Term::List under application.
            }
            ModelDoc::Nil
            | ModelDoc::HardLine
            | ModelDoc::SoftLineOrSpace
            | ModelDoc::SoftLineOrEmpty => {}
        }
        add(child_depth, 1)
    }

    fn check(model: &DocumentModel, limits: ProofLimits) -> Result<usize, ProofError> {
        limits.validate()?;
        // Even an empty document needs these finite fixed template envelopes.
        // Reject impossible tiny limits before any source payload traversal.
        bound(4096, limits.max_lean_bytes, "emitter Lean bytes")?;
        bound(8192, limits.max_metadata_bytes, "emitter metadata")?;
        // These sealed statistics reject tighter caller caps before tree walking.
        bound(model.node_count(), limits.max_nodes, "emitter nodes")?;
        bound(model.text_bytes(), limits.max_text_bytes, "emitter text")?;
        bound(model.depth(), limits.max_depth, "emitter depth")?;
        let mut measured = Self::default();
        measured.charge(512, limits)?;
        let ast_depth = measured.visit(model.doc(), 1, limits)?;
        if measured.nodes != model.node_count()
            || measured.text != model.text_bytes()
            || measured.depth != model.depth()
            || measured.comments != model.comments().len()
        {
            return Err(ProofError::Conservation);
        }
        // Fixed template is <4096 bytes. Each constructor's fixed qualified
        // syntax, punctuation and Nat literal is <128 bytes; payload escapes
        // are counted separately. Lists are flat AST lists, not cons chains.
        let rendered = add(add(4096, mul(measured.nodes, 128)?)?, measured.quoted_bytes)?;
        bound(rendered, limits.max_lean_bytes, "emitter Lean bytes")?;
        // Includes owned AST nodes/argument slots, fixed-name components,
        // payload clones and printer temporary collections. Conservative and
        // finite: maxima need not all fit under one metadata/work allowance.
        let metadata = add(add(8192, mul(measured.nodes, 768)?)?, measured.text)?;
        bound(metadata, limits.max_metadata_bytes, "emitter metadata")?;
        bound(ast_depth, 512, "emitter AST depth")?;
        let print_work = mul(rendered, add(mul(ast_depth, 2)?, 16)?)?;
        let work = add(
            measured.work,
            add(
                add(add(mul(metadata, 2)?, print_work)?, measured.text)?,
                mul(measured.ast_nodes, 64)?,
            )?,
        )?;
        bound(work, limits.max_work, "emitter work")?;
        Ok(rendered)
    }
}

fn syntax(error: vesper_lean_syntax::Error) -> ProofError {
    ProofError::LeanSyntax(error.to_string())
}
fn name(parts: &[&str]) -> Result<Name, ProofError> {
    Name::qualified(
        parts
            .iter()
            .map(|part| Ident::new(*part))
            .collect::<Result<Vec<_>, _>>()
            .map_err(syntax)?,
    )
    .map_err(syntax)
}
fn reference(parts: &[&str]) -> Result<Term, ProofError> {
    Ok(Term::Ref(name(parts)?.rooted()))
}
fn local(value: &str) -> Result<Term, ProofError> {
    Ok(Term::Ref(Name::single(value).map_err(syntax)?))
}
fn constructor(value: &str) -> Result<Term, ProofError> {
    reference(&["Vesper", "StylusSafety", "Doc", value])
}
fn reserve<T>(count: usize) -> Result<Vec<T>, ProofError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(count)
        .map_err(|_| ProofError::Allocation)?;
    Ok(values)
}
fn text(value: &str) -> Result<String, ProofError> {
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| ProofError::Allocation)?;
    owned.push_str(value);
    Ok(owned)
}
fn doc_term(doc: &ModelDoc) -> Result<Term, ProofError> {
    Ok(match doc {
        ModelDoc::Nil => constructor("nil")?,
        ModelDoc::Text(value) => Term::app(constructor("text")?, [Term::String(text(value)?)]),
        ModelDoc::Comment(value) => {
            Term::app(constructor("comment")?, [Term::String(text(value)?)])
        }
        ModelDoc::HardLine => constructor("hardLine")?,
        ModelDoc::SoftLineOrSpace => constructor("softLineOrSpace")?,
        ModelDoc::SoftLineOrEmpty => constructor("softLineOrEmpty")?,
        ModelDoc::Nest(indent, child) => Term::app(
            constructor("nest")?,
            [
                Term::Nat(
                    u128::try_from(*indent)
                        .map_err(|_| ProofError::InvalidDocument)?
                        .into(),
                ),
                doc_term(child)?,
            ],
        ),
        ModelDoc::Concat(children) => {
            let mut terms = reserve(children.len())?;
            for child in children {
                terms.push(doc_term(child)?);
            }
            Term::app(constructor("concat")?, [Term::List(terms)])
        }
        ModelDoc::Group(child) => Term::app(constructor("group")?, [doc_term(child)?]),
    })
}
fn hex(bytes: [u8; 32]) -> Result<String, ProofError> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut value = String::new();
    value
        .try_reserve_exact(64)
        .map_err(|_| ProofError::Allocation)?;
    for byte in bytes {
        value.push(char::from(DIGITS[usize::from(byte >> 4)]));
        value.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    Ok(value)
}
fn definition(value: &str, result: Term, body: Term) -> Result<Declaration, ProofError> {
    Ok(Declaration::Definition(Definition {
        name: Name::single(value).map_err(syntax)?,
        binders: Vec::new(),
        result,
        body,
        kind: DefinitionKind::Def,
    }))
}
fn rendered_comments(mode: &str) -> Result<Term, ProofError> {
    let render = Term::app(
        reference(&["Vesper", "StylusSafety", "render"])?,
        [
            local("width")?,
            local("col")?,
            reference(&["Vesper", "StylusSafety", "Mode", mode])?,
            reference(&["Vesper", "StylusGenerated", "document"])?,
        ],
    );
    Ok(Term::app(
        reference(&["Vesper", "StylusSafety", "commentsOf"])?,
        [render],
    ))
}

/// Preflight before constructing the AST or invoking its recursive printer.
/// The source association is supplied only by the private source-format wrapper.
pub(crate) fn emit(
    model: &DocumentModel,
    source: [u8; 32],
    output: [u8; 32],
    limits: ProofLimits,
) -> Result<String, ProofError> {
    let rendered_bound = Preflight::check(model, limits)?;
    let declarations = vec![
        definition(
            "document",
            reference(&["Vesper", "StylusSafety", "Doc"])?,
            doc_term(model.doc())?,
        )?,
        definition(
            "source_blake3",
            reference(&["String"])?,
            Term::String(hex(source)?),
        )?,
        definition(
            "output_blake3",
            reference(&["String"])?,
            Term::String(hex(output)?),
        )?,
        Declaration::Theorem {
            name: Name::single("comments_preserved").map_err(syntax)?,
            binders: vec![
                Binder::explicit(Ident::new("width").map_err(syntax)?, reference(&["Nat"])?),
                Binder::explicit(Ident::new("col").map_err(syntax)?, reference(&["Nat"])?),
            ],
            proposition: Term::binary(
                BinaryOp::Eq,
                rendered_comments("flat")?,
                rendered_comments("broken")?,
            ),
            proof: Term::app(
                reference(&["Vesper", "StylusSafety", "comments_render_invariant"])?,
                [
                    reference(&["Vesper", "StylusGenerated", "document"])?,
                    local("width")?,
                    local("col")?,
                ],
            ),
        },
    ];
    let module = Module {
        imports: vec![ModuleName::new("StylusAndLintSafety").map_err(syntax)?],
        declarations: vec![
            Declaration::SetOption {
                option: LeanOption::MaxRecDepth,
                value: OptionValue::Nat(4096),
            },
            Declaration::SetOption {
                option: LeanOption::MaxHeartbeats,
                value: OptionValue::Nat(1_000_000),
            },
            Declaration::Namespace {
                name: name(&["Vesper", "StylusGenerated"])?,
                declarations,
            },
        ],
    };
    let rendered = module.render().map_err(syntax)?;
    if rendered.len() > rendered_bound {
        return Err(ProofError::Conservation);
    }
    bound(rendered.len(), limits.max_lean_bytes, "emitter Lean bytes")?;
    Ok(rendered)
}
