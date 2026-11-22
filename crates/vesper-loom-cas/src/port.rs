//! Adapters to the Vesper port of the codecs (vesper/, built by the
//! `vesper-loom-cas-port` package in port/): names, object ids, tree and derivation encodings. Each
//! function has the signature of the Rust code it replaces; it flattens the
//! crate's values into the words and bytes `vesper/src/lib.ves` documents,
//! and rebuilds the crate's values and errors from what comes back.

use std::collections::BTreeMap;

use crate::derivation::{DerivationInputs, InputSchema};
use crate::error::{DerivationError, NameError, TreeError};
use crate::id::{Digest, Domain, OBJECT_ID_BYTES, ObjectId, ObjectKind};
use crate::tree::{EntryKind, MAX_TREE_ENTRIES, Tree};

#[rustfmt::skip]
#[allow(clippy::all, dead_code, unsafe_code)]
#[path = "../port/binding.rs"]
pub(crate) mod binding;

fn word(n: usize) -> u64 {
    n as u64
}

fn digest(bytes: &[u8]) -> Digest {
    let mut d = [0u8; 32];
    d.copy_from_slice(&bytes[..32]);
    Digest(d)
}

const NAME_ERRORS: [NameError; 9] = [
    NameError::Empty,
    NameError::TooLong,
    NameError::DotOrDotDot,
    NameError::Separator,
    NameError::ForbiddenCharacter,
    NameError::NulOrControl,
    NameError::WindowsReserved,
    NameError::TrailingDotOrSpace,
    NameError::NotUtf8,
];

pub(crate) fn name_error(code: u64) -> NameError {
    NAME_ERRORS[(code - 1) as usize]
}

/// `validate_name`.
pub(crate) fn validate_name(name: &str) -> Result<(), NameError> {
    match binding::name_error(name.as_bytes()) {
        0 => Ok(()),
        code => Err(name_error(code)),
    }
}

/// The digest of `bytes` under `domain`'s and `kind`'s object context.
pub(crate) fn object_digest(domain: Domain, kind: ObjectKind, bytes: &[u8]) -> Digest {
    digest(&binding::object_digest(domain.tag(), kind.tag(), bytes))
}

/// `Digest::to_hex`.
pub(crate) fn to_hex(d: &Digest) -> String {
    String::from_utf8(binding::to_hex(&d.0)).expect("hex digits are ASCII")
}

/// `Digest::from_hex`.
pub(crate) fn from_hex(text: &str) -> Option<Digest> {
    binding::digest_from_hex(text.as_bytes())
        .ok()
        .map(|b| digest(&b))
}

/// `ObjectId::from_str`'s parse.
pub(crate) fn parse_object_id(text: &str) -> Option<ObjectId> {
    let b = binding::parse_object_id(text.as_bytes()).ok()?;
    ObjectId::decode(&b)
}

/// The flat entry columns of a tree, in its canonical order.
struct Columns {
    names: Vec<u8>,
    ends: Vec<u64>,
    kinds: Vec<u8>,
    digests: Vec<u8>,
    sizes: Vec<u64>,
}

fn columns(tree: &Tree) -> Columns {
    let mut c = Columns {
        names: Vec::new(),
        ends: Vec::with_capacity(tree.len()),
        kinds: Vec::with_capacity(tree.len()),
        digests: Vec::with_capacity(tree.len() * 32),
        sizes: Vec::with_capacity(tree.len()),
    };
    for (name, entry) in tree.entries() {
        c.names.extend_from_slice(name.as_bytes());
        c.ends.push(word(c.names.len()));
        c.kinds.push(entry.kind.tag());
        c.digests.extend_from_slice(&entry.digest.0);
        c.sizes.push(entry.size);
    }
    c
}

/// `Tree::encode`.
pub(crate) fn tree_encode(tree: &Tree) -> Vec<u8> {
    let c = columns(tree);
    binding::tree_encode(&c.names, &c.ends, &c.kinds, &c.digests, &c.sizes)
}

/// `Tree::id`'s digest.
pub(crate) fn tree_digest(tree: &Tree) -> Digest {
    let c = columns(tree);
    digest(&binding::tree_digest(
        tree.domain().tag(),
        &c.names,
        &c.ends,
        &c.kinds,
        &c.digests,
        &c.sizes,
    ))
}

fn text(bytes: &[u8], at: u64, len: u64) -> &[u8] {
    &bytes[at as usize..(at + len) as usize]
}

fn name(bytes: &[u8], at: u64, len: u64) -> String {
    String::from_utf8_lossy(text(bytes, at, len)).into_owned()
}

/// `Tree::decode`: the Vesper scan reports each entry and its own error;
/// the entries are inserted in order (the case-collision check), and the
/// first error of either kind wins, as in the Rust decoder.
pub(crate) fn tree_decode(domain: Domain, bytes: &[u8]) -> Result<Tree, TreeError> {
    let words = binding::tree_decode(bytes);
    let (entries, end) = words.split_at(words.len() - 3);
    let mut tree = Tree::new(domain);
    let mut previous: Option<(u64, u64)> = None;
    for &[at, len, tag, size, local] in entries.as_chunks::<5>().0 {
        let entry_name = name(bytes, at, len);
        match local {
            0 => {}
            1 => {
                let (pa, pl) = previous.expect("an unsorted entry has a predecessor");
                return Err(TreeError::NotSorted {
                    previous: name(bytes, pa, pl),
                    next: entry_name,
                });
            }
            2 => return Err(TreeError::DirectorySize { name: entry_name }),
            code => {
                return Err(TreeError::InvalidName {
                    name: entry_name,
                    reason: name_error(code - 16),
                });
            }
        }
        let kind = EntryKind::from_tag(tag as u8).expect("the scan checked the tag");
        let d = digest(&bytes[(at + len + 1) as usize..]);
        tree.insert(
            &entry_name,
            ObjectId::new(domain, kind.object_kind(), d),
            kind,
            size,
        )?;
        previous = Some((at, len));
    }
    match end[0] {
        0 => Ok(tree),
        1 => Err(TreeError::BadMagic),
        2 => Err(TreeError::UnsupportedVersion(end[1] as u32)),
        3 => Err(TreeError::Truncated),
        4 => Err(TreeError::TrailingBytes),
        5 => Err(TreeError::TooManyEntries {
            count: end[1],
            limit: MAX_TREE_ENTRIES,
        }),
        6 => Err(TreeError::UnknownEntryKind(end[1] as u8)),
        7 => Err(TreeError::InvalidName {
            name: name(bytes, end[1], end[2]),
            reason: NameError::NotUtf8,
        }),
        status => panic!("invalid tree scan status {status}"),
    }
}

const FIELDS: [&str; 8] = [
    "builder",
    "builder_version",
    "target",
    "option key",
    "option value",
    "input name",
    "options",
    "inputs",
];

/// The derivation inputs' strings and ids in `drv_encode`'s order.
struct Flat {
    strs: Vec<u8>,
    ends: Vec<u64>,
    ids: Vec<u8>,
    texts: Vec<String>,
}

fn flat(inputs: &DerivationInputs) -> Flat {
    let mut f = Flat {
        strs: Vec::new(),
        ends: Vec::new(),
        ids: Vec::with_capacity(inputs.inputs.len() * OBJECT_ID_BYTES),
        texts: Vec::new(),
    };
    let mut push = |s: &str| {
        f.strs.extend_from_slice(s.as_bytes());
        f.ends.push(word(f.strs.len()));
        f.texts.push(s.to_owned());
    };
    push(&inputs.builder);
    push(&inputs.builder_version);
    if let Some(t) = &inputs.target {
        push(t);
    }
    for (k, v) in &inputs.options {
        push(k);
        push(v);
    }
    for name in inputs.inputs.keys() {
        push(name);
    }
    for id in inputs.inputs.values() {
        id.encode_into(&mut f.ids);
    }
    f
}

fn truncated(value: &str) -> String {
    value.chars().take(64).collect()
}

/// The crate's error for a failed `drv_encode`/`drv_decode`. `label` is the
/// text of the string an InvalidLabel names (`info2` locates it in a
/// decoded encoding).
fn drv_error(e: binding::DrvErr, info: u64, label: impl FnOnce() -> String) -> DerivationError {
    use binding::DrvErr as E;
    let field = || FIELDS[(info >> 32) as usize];
    match e {
        E::BadMagic => DerivationError::BadMagic,
        E::UnsupportedSchema => DerivationError::UnsupportedSchema(info as u32),
        E::Truncated => DerivationError::Truncated,
        E::TrailingBytes => DerivationError::TrailingBytes,
        E::SourceOutputDomain => DerivationError::SourceOutputDomain,
        E::UnknownDomain => DerivationError::UnknownDomain(info as u8),
        E::InvalidLabel => {
            // `check_label` reports an empty label as it is and every other
            // invalid label cut to 64 characters.
            DerivationError::InvalidLabel {
                field: field(),
                value: truncated(&label()),
            }
        }
        E::NoInputs => DerivationError::NoInputs,
        E::TooManyEntries => DerivationError::TooManyEntries {
            field: field(),
            limit: 4096,
        },
        E::NotSorted => DerivationError::NotSorted {
            field: FIELDS[info as usize],
        },
        E::InvalidObjectId => DerivationError::InvalidObjectId,
        E::StringTooLong => DerivationError::InvalidLabel {
            field: "string",
            value: format!("<{info} bytes>"),
        },
        E::StringNotUtf8 => unreachable!("handled by drv_decode"),
    }
}

/// `DerivationInputs::encode`.
pub(crate) fn drv_encode(inputs: &DerivationInputs) -> Result<Vec<u8>, DerivationError> {
    let f = flat(inputs);
    let mut info = 0;
    binding::drv_encode(
        inputs.output_domain.tag(),
        inputs.target.is_some(),
        word(inputs.options.len()),
        word(inputs.inputs.len()),
        &f.strs,
        &f.ends,
        &f.ids,
        &mut info,
    )
    .map_err(|e| drv_error(e, info, || f.texts[(info & 0xffff_ffff) as usize].clone()))
}

/// `DerivationInputs::key`'s digest.
pub(crate) fn drv_key(inputs: &DerivationInputs) -> Result<Digest, DerivationError> {
    let f = flat(inputs);
    let mut info = 0;
    binding::drv_key(
        inputs.output_domain.tag(),
        inputs.target.is_some(),
        word(inputs.options.len()),
        word(inputs.inputs.len()),
        &f.strs,
        &f.ends,
        &f.ids,
        &mut info,
    )
    .map(|d| digest(&d))
    .map_err(|e| drv_error(e, info, || f.texts[(info & 0xffff_ffff) as usize].clone()))
}

/// `DerivationKey::of_encoding`'s digest.
pub(crate) fn derivation_hash(encoding: &[u8]) -> Digest {
    digest(&binding::derivation_hash(encoding))
}

/// `DerivationInputs::decode`.
pub(crate) fn drv_decode(bytes: &[u8]) -> Result<DerivationInputs, DerivationError> {
    let (mut info, mut info2) = (0, 0);
    let words = match binding::drv_decode(bytes, &mut info, &mut info2) {
        Ok(w) => w,
        Err(binding::DrvErr::StringNotUtf8) => {
            return Err(DerivationError::InvalidLabel {
                field: "string",
                value: name(bytes, info, info2),
            });
        }
        Err(e) => {
            return Err(drv_error(e, info, || {
                name(bytes, info2 / 8192, info2 % 8192)
            }));
        }
    };
    let domain = Domain::from_tag(words[0] as u8).expect("the scan checked the domain");
    let has_target = words[1] == 1;
    let (n_options, n_inputs) = (words[2] as usize, words[3] as usize);
    let n_strings = 2 + usize::from(has_target) + 2 * n_options + n_inputs;
    let spans = &words[4..4 + 2 * n_strings];
    let ids = &words[4 + 2 * n_strings..];
    let s = |i: usize| -> String {
        String::from_utf8(text(bytes, spans[2 * i], spans[2 * i + 1]).to_vec())
            .expect("the scan checked UTF-8")
    };
    let base = 2 + usize::from(has_target);
    let mut options = BTreeMap::new();
    for j in 0..n_options {
        options.insert(s(base + 2 * j), s(base + 2 * j + 1));
    }
    let mut inputs = BTreeMap::new();
    for (j, &at) in ids.iter().enumerate().take(n_inputs) {
        let at = at as usize;
        let id =
            ObjectId::decode(&bytes[at..at + OBJECT_ID_BYTES]).expect("the scan checked the id");
        inputs.insert(s(base + 2 * n_options + j), id);
    }
    Ok(DerivationInputs {
        schema: InputSchema::V1,
        output_domain: domain,
        builder: s(0),
        builder_version: s(1),
        target: has_target.then(|| s(2)),
        options,
        inputs,
    })
}

#[cfg(test)]
#[allow(dead_code)]
#[path = "../examples/lower_table.rs"]
mod lower_table;

#[cfg(test)]
mod tests {
    use super::binding;
    use super::lower_table;

    /// The port's case folding (`fold.ves`, from the generated `lower.ves`)
    /// against `str::to_lowercase`, on seeded strings that mix ASCII, the
    /// sigmas and the chars around them (case-ignorable marks and
    /// punctuation, cased letters), U+0130 and arbitrary chars. Only on a
    /// toolchain with the table's Unicode version.
    #[test]
    fn name_fold_is_to_lowercase() {
        if !lower_table::same_unicode(include_str!("../vesper/src/lower.ves")) {
            return;
        }
        const POOL: &[char] = &[
            'A',
            'z',
            '.',
            '\'',
            ':',
            ' ',
            '\u{3a3}',
            '\u{3c3}',
            '\u{3c2}',
            '\u{391}',
            '\u{3b1}',
            '\u{301}',
            '\u{345}',
            '\u{2b0}',
            '\u{2019}',
            '\u{130}',
            '\u{1e9e}',
            '\u{2126}',
            '\u{10400}',
            '\u{1f600}',
            '\u{fb03}',
        ];
        let mut state: u64 = 0x5eed;
        let mut next = move || {
            state = state.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut z = state;
            z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            z ^ (z >> 31)
        };
        for _ in 0..50_000 {
            let len = next() % 8;
            let s: String = (0..len)
                .map(|_| {
                    let r = next();
                    if r % 4 == 0 {
                        char::from_u32((r >> 8) as u32 % 0x11_0000).unwrap_or('x')
                    } else {
                        POOL[(r >> 8) as usize % POOL.len()]
                    }
                })
                .collect();
            let folded = binding::name_fold(s.as_bytes());
            assert_eq!(String::from_utf8_lossy(&folded), s.to_lowercase(), "{s:?}");
        }
        // Every char on its own and before a final sigma position.
        for c in (0..=0x10_ffffu32).filter_map(char::from_u32) {
            for s in [
                c.to_string(),
                format!("{c}\u{3a3}"),
                format!("A{c}\u{3a3}"),
                format!("A\u{3a3}{c}"),
            ] {
                let folded = binding::name_fold(s.as_bytes());
                assert_eq!(String::from_utf8_lossy(&folded), s.to_lowercase(), "{s:?}");
            }
        }
    }
}
