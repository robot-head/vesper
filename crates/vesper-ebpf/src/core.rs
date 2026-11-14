//! BTF CO-RE (Compile Once, Run Everywhere) relocations (design §27.1
//! item 4, `@btf_core`).
//!
//! A program describes the kernel types it touches with *local* types
//! ([`LocalType`]): only the members it uses, named as in the kernel. Each
//! relocation ([`CoreRelo`]) names an instruction whose immediate (or
//! memory offset) holds a value computed from the local type — a member's
//! byte offset, a type's size, whether a field exists, an enumerator's
//! value — and is rewritten for the running kernel by matching names
//! against the kernel's BTF:
//!
//! * [`apply`] does that in user space against a parsed target BTF (for
//!   example `/sys/kernel/btf/vmlinux`), the way libbpf and aya do;
//! * the ELF writer emits the relocations as `.BTF.ext` `core_relo`
//!   records, which aya-obj (`Object::relocate_btf`) and the kernel itself
//!   (`BPF_PROG_LOAD` with `core_relos`) apply.
//!
//! Matching follows libbpf's rules for the supported kinds: candidates are
//! the target types of the same kind whose name equals the local name with
//! any `___flavor` suffix removed; members match by name, looking through
//! anonymous structs and unions; member types must be of compatible kinds;
//! candidates that do not contain the field are dropped; candidates that
//! disagree are an error. Bitfield members get libbpf's load offset, load
//! size and the two shift amounts that extract them from a 64-bit load
//! (`FIELD_LSHIFT_U64`, `FIELD_RSHIFT_U64`); `TYPE_ID_LOCAL` and
//! `TYPE_ID_TARGET` give the type's id in the program's and the target's
//! BTF; `TYPE_MATCHES` is libbpf's recursive `bpf_core_types_match`.
//!
//! Candidates are searched in vmlinux first and, only when vmlinux has
//! none, in the kernel modules' split BTF ([`apply_with_modules`],
//! `/sys/kernel/btf/<module>`), as libbpf does.

use crate::btf::{Btf, BtfType};
use crate::insn::{RawInsn, class};
use std::fmt;

/// A program-side description of a kernel type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum LocalType {
    Void,
    Int {
        name: String,
        size: u32,
        signed: bool,
    },
    Ptr(Box<LocalType>),
    Array {
        elem: Box<LocalType>,
        len: u32,
    },
    Struct {
        name: String,
        union: bool,
        size: u32,
        members: Vec<LocalMember>,
    },
    Enum {
        name: String,
        size: u32,
        values: Vec<(String, i64)>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct LocalMember {
    pub name: String,
    pub ty: LocalType,
    /// Byte offset in the local layout (for a bitfield, the byte holding
    /// its first bit).
    pub offset: u32,
    /// Set for a bitfield member.
    pub bitfield: Option<Bitfield>,
}

/// A bitfield member's position in its struct.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Bitfield {
    /// Bit offset from the start of the struct.
    pub bit_offset: u32,
    /// Width in bits (1..=64).
    pub bits: u32,
}

impl LocalType {
    pub fn int(name: &str, size: u32, signed: bool) -> LocalType {
        LocalType::Int {
            name: name.to_owned(),
            size,
            signed,
        }
    }

    /// A struct whose members are laid out at the given byte offsets.
    pub fn structure(name: &str, size: u32, members: Vec<(&str, LocalType, u32)>) -> LocalType {
        LocalType::Struct {
            name: name.to_owned(),
            union: false,
            size,
            members: members
                .into_iter()
                .map(|(n, ty, offset)| LocalMember {
                    name: n.to_owned(),
                    ty,
                    offset,
                    bitfield: None,
                })
                .collect(),
        }
    }

    /// A struct with bitfield members: (name, type, bit offset, bit width;
    /// width 0 = an ordinary member at that bit offset).
    pub fn structure_bits(
        name: &str,
        size: u32,
        members: Vec<(&str, LocalType, u32, u32)>,
    ) -> LocalType {
        LocalType::Struct {
            name: name.to_owned(),
            union: false,
            size,
            members: members
                .into_iter()
                .map(|(n, ty, bit_offset, bits)| LocalMember {
                    name: n.to_owned(),
                    ty,
                    offset: bit_offset / 8,
                    bitfield: (bits != 0).then_some(Bitfield { bit_offset, bits }),
                })
                .collect(),
        }
    }

    pub fn enumeration(name: &str, size: u32, values: &[(&str, i64)]) -> LocalType {
        LocalType::Enum {
            name: name.to_owned(),
            size,
            values: values.iter().map(|(n, v)| ((*n).to_owned(), *v)).collect(),
        }
    }

    pub fn name(&self) -> &str {
        match self {
            LocalType::Int { name, .. }
            | LocalType::Struct { name, .. }
            | LocalType::Enum { name, .. } => name,
            _ => "",
        }
    }

    pub fn size(&self) -> u64 {
        match self {
            LocalType::Void => 0,
            LocalType::Int { size, .. }
            | LocalType::Struct { size, .. }
            | LocalType::Enum { size, .. } => *size as u64,
            LocalType::Ptr(_) => 8,
            LocalType::Array { elem, len } => elem.size() * *len as u64,
        }
    }
}

/// `enum bpf_core_relo_kind` values this crate supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum CoreKind {
    FieldByteOffset,
    FieldByteSize,
    FieldExists,
    FieldSigned,
    /// Left shift that puts the field's top bit at bit 63 of a 64-bit
    /// value loaded with the field's load size (little endian).
    FieldLshiftU64,
    /// Right shift that then moves the field to bit 0.
    FieldRshiftU64,
    TypeIdLocal,
    TypeIdTarget,
    TypeExists,
    TypeSize,
    EnumvalExists,
    EnumvalValue,
    TypeMatches,
}

impl CoreKind {
    pub fn code(self) -> u32 {
        match self {
            CoreKind::FieldByteOffset => 0,
            CoreKind::FieldByteSize => 1,
            CoreKind::FieldExists => 2,
            CoreKind::FieldSigned => 3,
            CoreKind::FieldLshiftU64 => 4,
            CoreKind::FieldRshiftU64 => 5,
            CoreKind::TypeIdLocal => 6,
            CoreKind::TypeIdTarget => 7,
            CoreKind::TypeExists => 8,
            CoreKind::TypeSize => 9,
            CoreKind::EnumvalExists => 10,
            CoreKind::EnumvalValue => 11,
            CoreKind::TypeMatches => 12,
        }
    }

    pub fn from_code(c: u32) -> Option<CoreKind> {
        Some(match c {
            0 => CoreKind::FieldByteOffset,
            1 => CoreKind::FieldByteSize,
            2 => CoreKind::FieldExists,
            3 => CoreKind::FieldSigned,
            4 => CoreKind::FieldLshiftU64,
            5 => CoreKind::FieldRshiftU64,
            6 => CoreKind::TypeIdLocal,
            7 => CoreKind::TypeIdTarget,
            8 => CoreKind::TypeExists,
            9 => CoreKind::TypeSize,
            10 => CoreKind::EnumvalExists,
            11 => CoreKind::EnumvalValue,
            12 => CoreKind::TypeMatches,
            _ => return None,
        })
    }

    fn is_field(self) -> bool {
        matches!(
            self,
            CoreKind::FieldByteOffset
                | CoreKind::FieldByteSize
                | CoreKind::FieldExists
                | CoreKind::FieldSigned
                | CoreKind::FieldLshiftU64
                | CoreKind::FieldRshiftU64
        )
    }

    fn is_type(self) -> bool {
        matches!(
            self,
            CoreKind::TypeIdLocal
                | CoreKind::TypeIdTarget
                | CoreKind::TypeExists
                | CoreKind::TypeSize
                | CoreKind::TypeMatches
        )
    }

    /// The kinds whose value is the program's own type id.
    pub fn needs_local_id(self) -> bool {
        matches!(self, CoreKind::TypeIdLocal | CoreKind::TypeIdTarget)
    }

    fn is_exists(self) -> bool {
        matches!(
            self,
            CoreKind::FieldExists
                | CoreKind::TypeExists
                | CoreKind::EnumvalExists
                | CoreKind::TypeMatches
        )
    }

    fn is_enumval(self) -> bool {
        matches!(self, CoreKind::EnumvalExists | CoreKind::EnumvalValue)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreError {
    /// A member name in a path does not exist in the local type.
    UnknownLocalMember(String),
    /// The access path does not fit the local type.
    BadAccess(String),
    /// No target type matches (the kind needs a match).
    NoMatch(String),
    /// Candidates give different values.
    Ambiguous(String),
    /// A bitfield that no load of at most 8 bytes covers, or a shift
    /// relocation of a member wider than 8 bytes.
    Bitfield(String),
    /// The instruction cannot carry the value.
    BadInstruction { slot: usize, detail: String },
    /// The instruction's value is not the local value.
    LocalMismatch {
        slot: usize,
        found: u64,
        expected: u64,
    },
}

impl fmt::Display for CoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CoreError::UnknownLocalMember(m) => write!(f, "CO-RE: unknown local member {m}"),
            CoreError::BadAccess(m) => write!(f, "CO-RE: bad access: {m}"),
            CoreError::NoMatch(m) => write!(f, "CO-RE: no target type matches {m}"),
            CoreError::Ambiguous(m) => write!(f, "CO-RE: candidates disagree for {m}"),
            CoreError::Bitfield(m) => write!(f, "CO-RE: bitfield {m} cannot be loaded"),
            CoreError::BadInstruction { slot, detail } => {
                write!(f, "CO-RE: instruction {slot}: {detail}")
            }
            CoreError::LocalMismatch {
                slot,
                found,
                expected,
            } => write!(
                f,
                "CO-RE: instruction {slot} holds {found}, the local value is {expected}"
            ),
        }
    }
}

impl std::error::Error for CoreError {}

/// What a relocation computes, relative to a local root type.
///
/// `access` is the libbpf access string as numbers: for field kinds the
/// first entry indexes the root as an array (normally 0) and each further
/// entry is a member index (struct/union) or element index (array); for
/// type kinds it is `[0]`; for enumerator kinds it is `[enumerator index]`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CoreSpec {
    pub kind: CoreKind,
    pub root: LocalType,
    pub access: Vec<u32>,
}

/// One relocation of the instruction at `slot`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct CoreRelo {
    pub slot: usize,
    pub spec: CoreSpec,
}

impl CoreSpec {
    /// A field relocation for `root.path[0].path[1]...` (member names;
    /// a decimal component indexes an array).
    pub fn field(kind: CoreKind, root: LocalType, path: &[&str]) -> Result<CoreSpec, CoreError> {
        if !kind.is_field() {
            return Err(CoreError::BadAccess(format!(
                "{kind:?} is not a field kind"
            )));
        }
        let mut access = vec![0];
        let mut cur = &root;
        for p in path {
            match cur {
                LocalType::Struct { members, .. } => {
                    let i = members
                        .iter()
                        .position(|m| m.name == *p)
                        .ok_or_else(|| CoreError::UnknownLocalMember((*p).to_owned()))?;
                    access.push(i as u32);
                    cur = &members[i].ty;
                }
                LocalType::Array { elem, len } => {
                    let i: u32 = p
                        .parse()
                        .map_err(|_| CoreError::BadAccess(format!("array index {p}")))?;
                    if i >= *len {
                        return Err(CoreError::BadAccess(format!("index {i} >= {len}")));
                    }
                    access.push(i);
                    cur = elem;
                }
                _ => return Err(CoreError::BadAccess(format!("{p} into a scalar"))),
            }
        }
        if path.is_empty() {
            return Err(CoreError::BadAccess("empty field path".into()));
        }
        Ok(CoreSpec { kind, root, access })
    }

    pub fn of_type(kind: CoreKind, root: LocalType) -> Result<CoreSpec, CoreError> {
        if !kind.is_type() {
            return Err(CoreError::BadAccess(format!("{kind:?} is not a type kind")));
        }
        Ok(CoreSpec {
            kind,
            root,
            access: vec![0],
        })
    }

    pub fn enumval(kind: CoreKind, root: LocalType, name: &str) -> Result<CoreSpec, CoreError> {
        if !kind.is_enumval() {
            return Err(CoreError::BadAccess(format!(
                "{kind:?} is not an enumval kind"
            )));
        }
        let LocalType::Enum { values, .. } = &root else {
            return Err(CoreError::BadAccess(
                "enumval relocation on a non-enum".into(),
            ));
        };
        let i = values
            .iter()
            .position(|v| v.0 == name)
            .ok_or_else(|| CoreError::UnknownLocalMember(name.to_owned()))?;
        Ok(CoreSpec {
            kind,
            root,
            access: vec![i as u32],
        })
    }

    /// The libbpf access string ("0:1:2").
    pub fn access_string(&self) -> String {
        self.access
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(":")
    }

    fn describe(&self) -> String {
        format!(
            "{:?} {}:{}",
            self.kind,
            self.root.name(),
            self.access_string()
        )
    }

    /// The local field reached by the access path: (bit offset, bitfield
    /// width or 0, type).
    fn local_field(&self) -> Result<(u64, u32, &LocalType), CoreError> {
        let (&first, rest) = self
            .access
            .split_first()
            .ok_or_else(|| CoreError::BadAccess("empty access".into()))?;
        let mut bits = first as u64 * self.root.size() * 8;
        let mut width = 0;
        let mut cur = &self.root;
        for &i in rest {
            match cur {
                LocalType::Struct { members, .. } => {
                    let m = members
                        .get(i as usize)
                        .ok_or_else(|| CoreError::BadAccess(format!("member {i}")))?;
                    match m.bitfield {
                        Some(b) => {
                            bits += b.bit_offset as u64;
                            width = b.bits;
                        }
                        None => bits += m.offset as u64 * 8,
                    }
                    cur = &m.ty;
                }
                LocalType::Array { elem, len } if i < *len => {
                    bits += i as u64 * elem.size() * 8;
                    cur = elem;
                }
                _ => return Err(CoreError::BadAccess(self.access_string())),
            }
        }
        Ok((bits, width, cur))
    }

    /// The value the instruction holds before relocation (computed from
    /// the local type, as clang computes it). The `TYPE_ID_*` kinds need
    /// the root's id in the program's BTF ([`Self::local_value_with`]).
    pub fn local_value(&self) -> Result<u64, CoreError> {
        self.local_value_with(None)
    }

    /// [`Self::local_value`] with the root's id in the program's BTF.
    pub fn local_value_with(&self, local_id: Option<u32>) -> Result<u64, CoreError> {
        if self.kind.needs_local_id() {
            return local_id.map(u64::from).ok_or_else(|| {
                CoreError::BadAccess(format!(
                    "{:?} needs the local type id of the program BTF",
                    self.kind
                ))
            });
        }
        Ok(match self.kind {
            k if k.is_field() => {
                let (bits, width, ty) = self.local_field()?;
                let signed = match ty {
                    LocalType::Int { signed, .. } => *signed,
                    LocalType::Enum { values, .. } => values.iter().any(|v| v.1 < 0),
                    _ => false,
                };
                field_value(k, bits, width, ty.size(), signed, &self.describe())?
            }
            CoreKind::TypeExists | CoreKind::EnumvalExists | CoreKind::TypeMatches => 1,
            CoreKind::TypeSize => self.root.size(),
            CoreKind::EnumvalValue => {
                let LocalType::Enum { values, .. } = &self.root else {
                    return Err(CoreError::BadAccess("not an enum".into()));
                };
                values
                    .get(self.access[0] as usize)
                    .ok_or_else(|| CoreError::BadAccess("enumerator index".into()))?
                    .1 as u64
            }
            _ => unreachable!("every kind is handled above"),
        })
    }

    /// Candidate ids for this relocation's root in `target`: same
    /// essential name, compatible kind. With `from`, only ids at or above
    /// it (the module's own types in a split BTF).
    pub fn candidates(&self, target: &Btf, from: u32) -> Vec<u32> {
        target
            .by_name(essential_name(self.root.name()))
            .iter()
            .copied()
            .filter(|&id| id >= from && kind_compatible(&self.root, target, id))
            .collect()
    }

    /// The value for the target kernel described by `target`, matching
    /// only `cands`; `None` when no candidate gives a value.
    fn value_over(&self, target: &Btf, cands: &[u32]) -> Result<Vec<u64>, CoreError> {
        let mut results = Vec::new();
        for &id in cands {
            let r = match self.kind {
                CoreKind::TypeExists => Some(1),
                CoreKind::TypeIdTarget => Some(id as u64),
                CoreKind::TypeMatches => Some(types_match(&self.root, target, id, false, 0) as u64),
                CoreKind::TypeSize => target.size_of(id),
                CoreKind::EnumvalExists | CoreKind::EnumvalValue => {
                    let LocalType::Enum { values, .. } = &self.root else {
                        return Err(CoreError::BadAccess("not an enum".into()));
                    };
                    let want = &values
                        .get(self.access[0] as usize)
                        .ok_or_else(|| CoreError::BadAccess("enumerator index".into()))?
                        .0;
                    let Some(BtfType::Enum { values: tv, .. }) = target.get(id) else {
                        continue;
                    };
                    match tv.iter().find(|(n, _)| target.name(*n) == want) {
                        Some((_, v)) if self.kind == CoreKind::EnumvalValue => Some(*v as u64),
                        Some(_) => Some(1),
                        None => None,
                    }
                }
                _ => self.target_field(target, id)?,
            };
            if let Some(v) = r {
                results.push(v);
            }
        }
        Ok(results)
    }

    fn combine(&self, mut results: Vec<u64>) -> Result<u64, CoreError> {
        results.sort_unstable();
        results.dedup();
        match results.as_slice() {
            [] if self.kind.is_exists() => Ok(0),
            [] => Err(CoreError::NoMatch(self.describe())),
            [v] => Ok(*v),
            // TYPE_MATCHES: some candidate matches
            _ if self.kind.is_exists() => Ok(1),
            _ => Err(CoreError::Ambiguous(self.describe())),
        }
    }

    /// The value for the target kernel described by `target`.
    pub fn target_value(&self, target: &Btf) -> Result<u64, CoreError> {
        self.target_value_with(target, &[], None)
    }

    /// The value for vmlinux `vmlinux`, searching `modules` (split BTF on
    /// top of `vmlinux`, see [`Btf::parse_split`]) only when vmlinux has no
    /// candidate. `local_id` is needed by `TYPE_ID_LOCAL`.
    pub fn target_value_with(
        &self,
        vmlinux: &Btf,
        modules: &[&Btf],
        local_id: Option<u32>,
    ) -> Result<u64, CoreError> {
        if self.kind == CoreKind::TypeIdLocal {
            return self.local_value_with(local_id);
        }
        let cands = self.candidates(vmlinux, 0);
        if !cands.is_empty() || modules.is_empty() {
            let r = self.value_over(vmlinux, &cands)?;
            return self.combine(r);
        }
        let mut all = Vec::new();
        for m in modules {
            let c = self.candidates(m, m.base_len());
            all.extend(self.value_over(m, &c)?);
        }
        self.combine(all)
    }

    /// Walk the access path in target type `cand`; `None` when a member is
    /// missing or of an incompatible kind (the candidate does not match).
    fn target_field(&self, target: &Btf, cand: u32) -> Result<Option<u64>, CoreError> {
        let (&first, rest) = self
            .access
            .split_first()
            .ok_or_else(|| CoreError::BadAccess("empty access".into()))?;
        let Some(root_size) = target.size_of(cand) else {
            return Ok(None);
        };
        let mut bits: u64 = first as u64 * root_size * 8;
        let mut width = 0u32;
        let mut local = &self.root;
        let mut cur = cand;
        for &i in rest {
            match local {
                LocalType::Struct { members, .. } => {
                    let m = members
                        .get(i as usize)
                        .ok_or_else(|| CoreError::BadAccess(format!("member {i}")))?;
                    let Some((ty, off, bitfield)) = find_member(target, cur, &m.name, 0) else {
                        return Ok(None);
                    };
                    if !kind_compatible(&m.ty, target, ty) {
                        return Ok(None);
                    }
                    bits += off as u64;
                    width = bitfield;
                    local = &m.ty;
                    cur = ty;
                }
                LocalType::Array { elem, .. } => {
                    let Some(BtfType::Array { elem: te, len, .. }) =
                        target.get(target.skip_mods(cur))
                    else {
                        return Ok(None);
                    };
                    if i >= *len {
                        return Ok(None);
                    }
                    let Some(es) = target.size_of(*te) else {
                        return Ok(None);
                    };
                    bits += i as u64 * es * 8;
                    width = 0;
                    local = elem;
                    cur = *te;
                }
                _ => return Err(CoreError::BadAccess(self.access_string())),
            }
        }
        let Some(size) = target.size_of(cur) else {
            return Ok(None);
        };
        let signed = match target.get(target.skip_mods(cur)) {
            Some(BtfType::Int { encoding, .. }) => encoding & crate::btf::INT_SIGNED != 0,
            Some(BtfType::Enum { signed, .. }) => *signed,
            _ => false,
        };
        field_value(self.kind, bits, width, size, signed, &self.describe()).map(Some)
    }
}

/// libbpf's `bpf_core_calc_field_relo` for a field at bit offset `bits`
/// of width `width` (0 = not a bitfield) whose type is `size` bytes: a
/// bitfield is loaded with the smallest naturally aligned power-of-two
/// load (from its type's size up to 8 bytes) that covers it.
fn field_value(
    kind: CoreKind,
    bits: u64,
    width: u32,
    size: u64,
    signed: bool,
    what: &str,
) -> Result<u64, CoreError> {
    let (byte_off, byte_sz, bit_sz) = if width == 0 {
        if !bits.is_multiple_of(8) {
            return Err(CoreError::Bitfield(what.to_owned()));
        }
        (bits / 8, size, size * 8)
    } else {
        let mut sz = size.max(1);
        let mut off = bits / 8 / sz * sz;
        while bits + width as u64 - off * 8 > sz * 8 {
            if sz >= 8 {
                return Err(CoreError::Bitfield(what.to_owned()));
            }
            sz *= 2;
            off = bits / 8 / sz * sz;
        }
        (off, sz, width as u64)
    };
    Ok(match kind {
        CoreKind::FieldByteOffset => byte_off,
        CoreKind::FieldByteSize => byte_sz,
        CoreKind::FieldExists => 1,
        CoreKind::FieldSigned => signed as u64,
        CoreKind::FieldLshiftU64 | CoreKind::FieldRshiftU64 => {
            if byte_sz > 8 {
                return Err(CoreError::Bitfield(what.to_owned()));
            }
            if kind == CoreKind::FieldLshiftU64 {
                64 - (bits + bit_sz - byte_off * 8)
            } else {
                64 - bit_sz
            }
        }
        _ => unreachable!("field kinds only"),
    })
}

/// libbpf's `bpf_core_names_match`: essential names equal; an anonymous
/// target matches only an anonymous local.
fn names_match(local: &str, target: &str) -> bool {
    if target.is_empty() {
        return local.is_empty();
    }
    essential_name(local) == essential_name(target)
}

/// libbpf's `__bpf_core_types_match` (`TYPE_MATCHES`): the local type,
/// with only the members it names, has the same shape in the target.
/// Behind a pointer, aggregates match by name and kind only (or a
/// forward declaration of the same kind).
fn types_match(local: &LocalType, target: &Btf, id: u32, behind_ptr: bool, depth: u32) -> bool {
    if depth > 32 {
        return false;
    }
    let tid = target.skip_mods(id);
    let Some(t) = target.get(tid) else {
        return false;
    };
    let tname = target.name(t.name_off());
    if !names_match(local.name(), tname) {
        return false;
    }
    match (local, t) {
        (LocalType::Void, BtfType::Void) => true,
        (
            LocalType::Int { size, signed, .. },
            BtfType::Int {
                size: ts, encoding, ..
            },
        ) => size == ts && *signed == (encoding & crate::btf::INT_SIGNED != 0),
        (
            LocalType::Enum { size, values, .. },
            BtfType::Enum {
                size: ts,
                values: tv,
                ..
            },
        ) => {
            size == ts
                && values.len() <= tv.len()
                && values
                    .iter()
                    .all(|(n, _)| tv.iter().any(|(tn, _)| names_match(n, target.name(*tn))))
        }
        (LocalType::Ptr(to), BtfType::Ptr { ty }) => types_match(to, target, *ty, true, depth + 1),
        (
            LocalType::Array { elem, len },
            BtfType::Array {
                elem: te, len: tl, ..
            },
        ) => len == tl && types_match(elem, target, *te, behind_ptr, depth + 1),
        (LocalType::Struct { union, .. }, BtfType::Fwd { union: tu, .. }) => {
            behind_ptr && union == tu
        }
        (
            LocalType::Struct { union, members, .. },
            BtfType::Struct {
                union: tu,
                members: tm,
                ..
            },
        ) => {
            if union != tu {
                return false;
            }
            if behind_ptr {
                return true;
            }
            members.len() <= tm.len()
                // bitfield widths are not compared (libbpf and the
                // kernel's bpf_core_composites_match do not either)
                && members.iter().all(|m| {
                    tm.iter().any(|t| {
                        names_match(&m.name, target.name(t.name))
                            && types_match(&m.ty, target, t.ty, false, depth + 1)
                    })
                })
        }
        _ => false,
    }
}

/// `name___flavor` → `name`.
pub fn essential_name(n: &str) -> &str {
    match n.find("___") {
        Some(i) => &n[..i],
        None => n,
    }
}

fn kind_compatible(local: &LocalType, target: &Btf, id: u32) -> bool {
    let t = target.get(target.skip_mods(id));
    match (local, t) {
        (LocalType::Struct { union, .. }, Some(BtfType::Struct { union: u, .. })) => union == u,
        (LocalType::Int { .. } | LocalType::Enum { .. }, Some(BtfType::Int { .. }))
        | (LocalType::Int { .. } | LocalType::Enum { .. }, Some(BtfType::Enum { .. })) => true,
        (LocalType::Ptr(_), Some(BtfType::Ptr { .. })) => true,
        (LocalType::Array { .. }, Some(BtfType::Array { .. })) => true,
        (LocalType::Void, Some(BtfType::Void)) => true,
        _ => false,
    }
}

/// Find member `name` of aggregate `id`, looking through anonymous
/// members: (member type, bit offset from the start of `id`, bitfield size).
fn find_member(btf: &Btf, id: u32, name: &str, depth: u32) -> Option<(u32, u32, u32)> {
    if depth > 32 {
        return None;
    }
    let Some(BtfType::Struct { members, .. }) = btf.get(btf.skip_mods(id)) else {
        return None;
    };
    for m in members {
        let n = btf.name(m.name);
        if n == name {
            return Some((m.ty, m.bit_offset, m.bitfield_size));
        }
        if n.is_empty()
            && let Some((t, off, bf)) = find_member(btf, m.ty, name, depth + 1)
        {
            return Some((t, m.bit_offset + off, bf));
        }
    }
    None
}

/// Write `value` into the instruction at `slot` (the forms the kernel's
/// `bpf_core_patch_insn` supports: ALU with an immediate, a load/store
/// offset, `LD_IMM64`), after checking it holds `expected`.
pub fn patch(
    insns: &mut [RawInsn],
    slot: usize,
    expected: u64,
    value: u64,
) -> Result<(), CoreError> {
    let bad = |d: &str| CoreError::BadInstruction {
        slot,
        detail: d.to_owned(),
    };
    let code = insns.get(slot).ok_or_else(|| bad("past the end"))?.code;
    match code & 0x07 {
        class::ALU | class::ALU64 if code & 0x08 == 0 => {
            let i = &mut insns[slot];
            if i.imm as u32 as u64 != expected && i.imm as i64 as u64 != expected {
                return Err(CoreError::LocalMismatch {
                    slot,
                    found: i.imm as u32 as u64,
                    expected,
                });
            }
            // a negative target value arrives sign-extended to u64
            i.imm = i32::try_from(value as i64)
                .or_else(|_| u32::try_from(value).map(|v| v as i32))
                .map_err(|_| bad("value does not fit a 32-bit immediate"))?;
        }
        class::LDX | class::ST | class::STX => {
            let i = &mut insns[slot];
            if i.off as i64 as u64 != expected {
                return Err(CoreError::LocalMismatch {
                    slot,
                    found: i.off as i64 as u64,
                    expected,
                });
            }
            i.off = i16::try_from(value).map_err(|_| bad("offset does not fit 16 bits"))?;
        }
        class::LD if code == 0x18 && slot + 1 < insns.len() => {
            let found =
                (insns[slot].imm as u32 as u64) | ((insns[slot + 1].imm as u32 as u64) << 32);
            if found != expected {
                return Err(CoreError::LocalMismatch {
                    slot,
                    found,
                    expected,
                });
            }
            insns[slot].imm = value as u32 as i32;
            insns[slot + 1].imm = (value >> 32) as u32 as i32;
        }
        _ => return Err(bad("unsupported instruction for a CO-RE relocation")),
    }
    Ok(())
}

/// Apply every relocation of `prog` for `target`; the result carries the
/// target values and no relocations.
pub fn apply(prog: &crate::Program, target: &Btf) -> Result<crate::Program, CoreError> {
    apply_with_modules(prog, target, &[])
}

/// [`apply`] with kernel modules: a relocation whose root type has no
/// candidate in `vmlinux` is resolved against `modules` (split BTF on top
/// of `vmlinux`, as `/sys/kernel/btf/<module>` is).
pub fn apply_with_modules(
    prog: &crate::Program,
    vmlinux: &Btf,
    modules: &[&Btf],
) -> Result<crate::Program, CoreError> {
    let mut out = prog.clone();
    let ids = if prog.core_relos.iter().any(|r| r.spec.kind.needs_local_id()) {
        crate::btf_ext::local_root_ids(prog).map_err(|e| CoreError::BadAccess(e.to_string()))?
    } else {
        Vec::new()
    };
    for (i, r) in prog.core_relos.iter().enumerate() {
        let id = ids.get(i).copied();
        let local = r.spec.local_value_with(id)?;
        let value = r.spec.target_value_with(vmlinux, modules, id)?;
        patch(&mut out.insns, r.slot, local, value)?;
    }
    out.core_relos.clear();
    Ok(out)
}
