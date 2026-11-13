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
//! disagree are an error. Bitfields, `TYPE_ID_*` and `TYPE_MATCHES` are not
//! supported and are typed errors.

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
    /// Byte offset in the local layout.
    pub offset: u32,
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
    TypeExists,
    TypeSize,
    EnumvalExists,
    EnumvalValue,
}

impl CoreKind {
    pub fn code(self) -> u32 {
        match self {
            CoreKind::FieldByteOffset => 0,
            CoreKind::FieldByteSize => 1,
            CoreKind::FieldExists => 2,
            CoreKind::FieldSigned => 3,
            CoreKind::TypeExists => 8,
            CoreKind::TypeSize => 9,
            CoreKind::EnumvalExists => 10,
            CoreKind::EnumvalValue => 11,
        }
    }

    pub fn from_code(c: u32) -> Option<CoreKind> {
        Some(match c {
            0 => CoreKind::FieldByteOffset,
            1 => CoreKind::FieldByteSize,
            2 => CoreKind::FieldExists,
            3 => CoreKind::FieldSigned,
            8 => CoreKind::TypeExists,
            9 => CoreKind::TypeSize,
            10 => CoreKind::EnumvalExists,
            11 => CoreKind::EnumvalValue,
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
    /// Bitfield members are not supported.
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
            CoreError::Bitfield(m) => write!(f, "CO-RE: bitfield {m} is not supported"),
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
        if !matches!(kind, CoreKind::TypeExists | CoreKind::TypeSize) {
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

    /// The local field reached by the access path: (byte offset, type).
    fn local_field(&self) -> Result<(u64, &LocalType), CoreError> {
        let (&first, rest) = self
            .access
            .split_first()
            .ok_or_else(|| CoreError::BadAccess("empty access".into()))?;
        let mut off = first as u64 * self.root.size();
        let mut cur = &self.root;
        for &i in rest {
            match cur {
                LocalType::Struct { members, .. } => {
                    let m = members
                        .get(i as usize)
                        .ok_or_else(|| CoreError::BadAccess(format!("member {i}")))?;
                    off += m.offset as u64;
                    cur = &m.ty;
                }
                LocalType::Array { elem, len } if i < *len => {
                    off += i as u64 * elem.size();
                    cur = elem;
                }
                _ => return Err(CoreError::BadAccess(self.access_string())),
            }
        }
        Ok((off, cur))
    }

    /// The value the instruction holds before relocation (computed from
    /// the local type, as clang computes it).
    pub fn local_value(&self) -> Result<u64, CoreError> {
        Ok(match self.kind {
            CoreKind::FieldByteOffset => self.local_field()?.0,
            CoreKind::FieldByteSize => self.local_field()?.1.size(),
            CoreKind::FieldExists | CoreKind::TypeExists | CoreKind::EnumvalExists => 1,
            CoreKind::FieldSigned => match self.local_field()?.1 {
                LocalType::Int { signed, .. } => *signed as u64,
                LocalType::Enum { values, .. } => values.iter().any(|v| v.1 < 0) as u64,
                _ => 0,
            },
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
        })
    }

    /// The value for the target kernel described by `target`.
    pub fn target_value(&self, target: &Btf) -> Result<u64, CoreError> {
        let essential = essential_name(self.root.name());
        let mut results: Vec<u64> = Vec::new();
        let mut any_candidate = false;
        for &id in target.by_name(essential) {
            if !kind_compatible(&self.root, target, id) {
                continue;
            }
            any_candidate = true;
            let r = match self.kind {
                CoreKind::TypeExists => Some(1),
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
        results.dedup();
        let exists_kind = matches!(
            self.kind,
            CoreKind::FieldExists | CoreKind::TypeExists | CoreKind::EnumvalExists
        );
        match results.as_slice() {
            [] if exists_kind => Ok(0),
            [] if !any_candidate => Err(CoreError::NoMatch(self.describe())),
            [] => Err(CoreError::NoMatch(self.describe())),
            [v] => Ok(*v),
            _ if exists_kind => Ok(1),
            _ => Err(CoreError::Ambiguous(self.describe())),
        }
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
                    if bitfield != 0 {
                        return Err(CoreError::Bitfield(m.name.clone()));
                    }
                    if !kind_compatible(&m.ty, target, ty) {
                        return Ok(None);
                    }
                    bits += off as u64;
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
                    local = elem;
                    cur = *te;
                }
                _ => return Err(CoreError::BadAccess(self.access_string())),
            }
        }
        if !bits.is_multiple_of(8) {
            return Err(CoreError::Bitfield(self.access_string()));
        }
        Ok(Some(match self.kind {
            CoreKind::FieldByteOffset => bits / 8,
            CoreKind::FieldByteSize => match target.size_of(cur) {
                Some(s) => s,
                None => return Ok(None),
            },
            CoreKind::FieldExists => 1,
            CoreKind::FieldSigned => match target.get(target.skip_mods(cur)) {
                Some(BtfType::Int { encoding, .. }) => {
                    (encoding & crate::btf::INT_SIGNED != 0) as u64
                }
                Some(BtfType::Enum { signed, .. }) => *signed as u64,
                _ => 0,
            },
            _ => unreachable!("field kinds only"),
        }))
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
            i.imm = i32::try_from(value)
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
    let mut out = prog.clone();
    for r in &prog.core_relos {
        let local = r.spec.local_value()?;
        let value = r.spec.target_value(target)?;
        patch(&mut out.insns, r.slot, local, value)?;
    }
    out.core_relos.clear();
    Ok(out)
}
