//! BTF (BPF Type Format): a reader for every kind the kernel defines and a
//! builder for the `.BTF` this crate emits.
//!
//! The reader is used on the kernel's own BTF (`/sys/kernel/btf/vmlinux`,
//! about 150k types) to resolve kfunc ids and CO-RE relocations, and on
//! objects this crate wrote. Malformed input is a [`BtfError`], never a
//! panic: every offset and length is checked.

use std::collections::HashMap;
use std::fmt;

pub const BTF_MAGIC: u16 = 0xeb9f;

pub const KIND_INT: u32 = 1;
pub const KIND_PTR: u32 = 2;
pub const KIND_ARRAY: u32 = 3;
pub const KIND_STRUCT: u32 = 4;
pub const KIND_UNION: u32 = 5;
pub const KIND_ENUM: u32 = 6;
pub const KIND_FWD: u32 = 7;
pub const KIND_TYPEDEF: u32 = 8;
pub const KIND_VOLATILE: u32 = 9;
pub const KIND_CONST: u32 = 10;
pub const KIND_RESTRICT: u32 = 11;
pub const KIND_FUNC: u32 = 12;
pub const KIND_FUNC_PROTO: u32 = 13;
pub const KIND_VAR: u32 = 14;
pub const KIND_DATASEC: u32 = 15;
pub const KIND_FLOAT: u32 = 16;
pub const KIND_DECL_TAG: u32 = 17;
pub const KIND_TYPE_TAG: u32 = 18;
pub const KIND_ENUM64: u32 = 19;

pub const INT_SIGNED: u32 = 1;
pub const INT_CHAR: u32 = 2;
pub const INT_BOOL: u32 = 4;

/// `BTF_FUNC_STATIC`, `BTF_FUNC_GLOBAL`, `BTF_FUNC_EXTERN`.
pub const FUNC_STATIC: u32 = 0;
pub const FUNC_GLOBAL: u32 = 1;
pub const FUNC_EXTERN: u32 = 2;
pub const VAR_GLOBAL_ALLOCATED: u32 = 1;
pub const VAR_GLOBAL_EXTERN: u32 = 2;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BtfError(pub String);

impl fmt::Display for BtfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "BTF: {}", self.0)
    }
}

impl std::error::Error for BtfError {}

fn bad(m: impl Into<String>) -> BtfError {
    BtfError(m.into())
}

/// A struct or union member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Member {
    pub name: u32,
    pub ty: u32,
    /// Bit offset from the start of the aggregate.
    pub bit_offset: u32,
    /// Nonzero for a bitfield (only with `kind_flag`).
    pub bitfield_size: u32,
}

/// One type. Names are offsets into the string section ([`Btf::name`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BtfType {
    Void,
    Int {
        name: u32,
        size: u32,
        encoding: u32,
        offset: u32,
        bits: u32,
    },
    Ptr {
        ty: u32,
    },
    Array {
        elem: u32,
        index: u32,
        len: u32,
    },
    Struct {
        name: u32,
        size: u32,
        union: bool,
        members: Vec<Member>,
    },
    Enum {
        name: u32,
        size: u32,
        signed: bool,
        values: Vec<(u32, i64)>,
    },
    Fwd {
        name: u32,
        union: bool,
    },
    Typedef {
        name: u32,
        ty: u32,
    },
    Volatile {
        ty: u32,
    },
    Const {
        ty: u32,
    },
    Restrict {
        ty: u32,
    },
    Func {
        name: u32,
        proto: u32,
        linkage: u32,
    },
    FuncProto {
        ret: u32,
        params: Vec<(u32, u32)>,
    },
    Var {
        name: u32,
        ty: u32,
        linkage: u32,
    },
    Datasec {
        name: u32,
        size: u32,
        vars: Vec<(u32, u32, u32)>,
    },
    Float {
        name: u32,
        size: u32,
    },
    DeclTag {
        name: u32,
        ty: u32,
        component: i32,
    },
    TypeTag {
        name: u32,
        ty: u32,
    },
}

impl BtfType {
    pub fn name_off(&self) -> u32 {
        match *self {
            BtfType::Int { name, .. }
            | BtfType::Struct { name, .. }
            | BtfType::Enum { name, .. }
            | BtfType::Fwd { name, .. }
            | BtfType::Typedef { name, .. }
            | BtfType::Func { name, .. }
            | BtfType::Var { name, .. }
            | BtfType::Datasec { name, .. }
            | BtfType::Float { name, .. }
            | BtfType::DeclTag { name, .. }
            | BtfType::TypeTag { name, .. } => name,
            _ => 0,
        }
    }
}

/// Parsed BTF.
#[derive(Clone, Debug)]
pub struct Btf {
    /// Index = type id; entry 0 is `void`.
    pub types: Vec<BtfType>,
    strings: Vec<u8>,
    by_name: HashMap<String, Vec<u32>>,
}

fn rd32(b: &[u8], at: usize) -> Result<u32, BtfError> {
    at.checked_add(4)
        .and_then(|e| b.get(at..e))
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| bad("truncated"))
}

impl Btf {
    /// Parse a little-endian BTF blob.
    pub fn parse(btf: &[u8]) -> Result<Btf, BtfError> {
        if btf.len() < 24 || u16::from_le_bytes([btf[0], btf[1]]) != BTF_MAGIC {
            return Err(bad("bad header"));
        }
        if btf[2] != 1 {
            return Err(bad(format!("unsupported version {}", btf[2])));
        }
        let hdr_len = rd32(btf, 4)? as usize;
        if hdr_len < 24 {
            return Err(bad("header too short"));
        }
        let type_off = rd32(btf, 8)? as usize;
        let type_len = rd32(btf, 12)? as usize;
        let str_off = rd32(btf, 16)? as usize;
        let str_len = rd32(btf, 20)? as usize;
        let span = |o: usize, l: usize| -> Result<&[u8], BtfError> {
            let s = hdr_len.checked_add(o).ok_or_else(|| bad("overflow"))?;
            let e = s.checked_add(l).ok_or_else(|| bad("overflow"))?;
            btf.get(s..e).ok_or_else(|| bad("section out of range"))
        };
        let types = span(type_off, type_len)?;
        let strings = span(str_off, str_len)?.to_vec();
        if !strings.is_empty() && (strings[0] != 0 || *strings.last().expect("nonempty") != 0) {
            return Err(bad("string section must start and end with NUL"));
        }
        let mut out = vec![BtfType::Void];
        let mut at = 0;
        while at < types.len() {
            let name = rd32(types, at)?;
            let info = rd32(types, at + 4)?;
            let sot = rd32(types, at + 8)?;
            at += 12;
            let kind = (info >> 24) & 0x1f;
            let vlen = (info & 0xffff) as usize;
            let kflag = info >> 31 != 0;
            let mut word = || -> Result<u32, BtfError> {
                let v = rd32(types, at)?;
                at += 4;
                Ok(v)
            };
            let t = match kind {
                KIND_INT => {
                    let w = word()?;
                    BtfType::Int {
                        name,
                        size: sot,
                        encoding: (w >> 24) & 0xf,
                        offset: (w >> 16) & 0xff,
                        bits: w & 0xff,
                    }
                }
                KIND_PTR => BtfType::Ptr { ty: sot },
                KIND_ARRAY => BtfType::Array {
                    elem: word()?,
                    index: word()?,
                    len: word()?,
                },
                KIND_STRUCT | KIND_UNION => {
                    let mut members = Vec::with_capacity(vlen.min(4096));
                    for _ in 0..vlen {
                        let n = word()?;
                        let ty = word()?;
                        let off = word()?;
                        let (bit_offset, bitfield_size) = if kflag {
                            (off & 0xff_ffff, off >> 24)
                        } else {
                            (off, 0)
                        };
                        members.push(Member {
                            name: n,
                            ty,
                            bit_offset,
                            bitfield_size,
                        });
                    }
                    BtfType::Struct {
                        name,
                        size: sot,
                        union: kind == KIND_UNION,
                        members,
                    }
                }
                KIND_ENUM => {
                    let mut values = Vec::with_capacity(vlen.min(4096));
                    for _ in 0..vlen {
                        let n = word()?;
                        let v = word()?;
                        values.push((n, if kflag { v as i32 as i64 } else { v as i64 }));
                    }
                    BtfType::Enum {
                        name,
                        size: sot,
                        signed: kflag,
                        values,
                    }
                }
                KIND_ENUM64 => {
                    let mut values = Vec::with_capacity(vlen.min(4096));
                    for _ in 0..vlen {
                        let n = word()?;
                        let lo = word()? as u64;
                        let hi = word()? as u64;
                        values.push((n, (lo | (hi << 32)) as i64));
                    }
                    BtfType::Enum {
                        name,
                        size: sot,
                        signed: kflag,
                        values,
                    }
                }
                KIND_FWD => BtfType::Fwd { name, union: kflag },
                KIND_TYPEDEF => BtfType::Typedef { name, ty: sot },
                KIND_VOLATILE => BtfType::Volatile { ty: sot },
                KIND_CONST => BtfType::Const { ty: sot },
                KIND_RESTRICT => BtfType::Restrict { ty: sot },
                KIND_FUNC => BtfType::Func {
                    name,
                    proto: sot,
                    linkage: vlen as u32,
                },
                KIND_FUNC_PROTO => {
                    let mut params = Vec::with_capacity(vlen.min(4096));
                    for _ in 0..vlen {
                        params.push((word()?, word()?));
                    }
                    BtfType::FuncProto { ret: sot, params }
                }
                KIND_VAR => BtfType::Var {
                    name,
                    ty: sot,
                    linkage: word()?,
                },
                KIND_DATASEC => {
                    let mut vars = Vec::with_capacity(vlen.min(4096));
                    for _ in 0..vlen {
                        vars.push((word()?, word()?, word()?));
                    }
                    BtfType::Datasec {
                        name,
                        size: sot,
                        vars,
                    }
                }
                KIND_FLOAT => BtfType::Float { name, size: sot },
                KIND_DECL_TAG => BtfType::DeclTag {
                    name,
                    ty: sot,
                    component: word()? as i32,
                },
                KIND_TYPE_TAG => BtfType::TypeTag { name, ty: sot },
                k => return Err(bad(format!("unknown kind {k}"))),
            };
            out.push(t);
        }
        let mut b = Btf {
            types: out,
            strings,
            by_name: HashMap::new(),
        };
        // validate references and names up front so lookups cannot fail
        let n = b.types.len() as u32;
        let mut by_name: HashMap<String, Vec<u32>> = HashMap::new();
        for (id, t) in b.types.iter().enumerate() {
            let refs: Vec<u32> = match t {
                BtfType::Ptr { ty }
                | BtfType::Typedef { ty, .. }
                | BtfType::Volatile { ty }
                | BtfType::Const { ty }
                | BtfType::Restrict { ty }
                | BtfType::Var { ty, .. }
                | BtfType::DeclTag { ty, .. }
                | BtfType::TypeTag { ty, .. } => vec![*ty],
                BtfType::Func { proto, .. } => vec![*proto],
                BtfType::Array { elem, index, .. } => vec![*elem, *index],
                BtfType::Struct { members, .. } => members.iter().map(|m| m.ty).collect(),
                BtfType::FuncProto { ret, params } => std::iter::once(*ret)
                    .chain(params.iter().map(|p| p.1))
                    .collect(),
                BtfType::Datasec { vars, .. } => vars.iter().map(|v| v.0).collect(),
                _ => vec![],
            };
            if refs.iter().any(|&r| r >= n) {
                return Err(bad(format!("type {id} refers past the type table")));
            }
            let mut names = vec![t.name_off()];
            match t {
                BtfType::Struct { members, .. } => names.extend(members.iter().map(|m| m.name)),
                BtfType::Enum { values, .. } => names.extend(values.iter().map(|v| v.0)),
                BtfType::FuncProto { params, .. } => names.extend(params.iter().map(|p| p.0)),
                _ => {}
            }
            for o in names {
                b.str_at(o)?;
            }
            let name = b.str_at(t.name_off())?;
            if !name.is_empty() {
                by_name.entry(name.to_owned()).or_default().push(id as u32);
            }
        }
        b.by_name = by_name;
        Ok(b)
    }

    fn str_at(&self, off: u32) -> Result<&str, BtfError> {
        if off == 0 {
            return Ok("");
        }
        let s = self
            .strings
            .get(off as usize..)
            .ok_or_else(|| bad("name offset out of range"))?;
        let end = s
            .iter()
            .position(|&c| c == 0)
            .ok_or_else(|| bad("unterminated name"))?;
        std::str::from_utf8(&s[..end]).map_err(|_| bad("non-UTF-8 name"))
    }

    /// A name offset validated at parse time ("" when out of range).
    pub fn name(&self, off: u32) -> &str {
        self.str_at(off).unwrap_or("")
    }

    pub fn get(&self, id: u32) -> Option<&BtfType> {
        self.types.get(id as usize)
    }

    /// Ids of the types named `name`.
    pub fn by_name(&self, name: &str) -> &[u32] {
        self.by_name.get(name).map_or(&[], Vec::as_slice)
    }

    /// Id of the `FUNC` named `name`.
    pub fn func_id(&self, name: &str) -> Option<u32> {
        self.by_name(name)
            .iter()
            .copied()
            .find(|&i| matches!(self.types[i as usize], BtfType::Func { .. }))
    }

    /// Follow typedefs and cv/type-tag modifiers.
    pub fn skip_mods(&self, mut id: u32) -> u32 {
        for _ in 0..64 {
            match self.types.get(id as usize) {
                Some(
                    BtfType::Typedef { ty, .. }
                    | BtfType::Volatile { ty }
                    | BtfType::Const { ty }
                    | BtfType::Restrict { ty }
                    | BtfType::TypeTag { ty, .. },
                ) => id = *ty,
                _ => return id,
            }
        }
        id
    }

    /// Byte size of a type, following modifiers (`None` for void,
    /// functions, forward declarations and cycles).
    pub fn size_of(&self, id: u32) -> Option<u64> {
        self.size_of_depth(id, 0)
    }

    fn size_of_depth(&self, id: u32, depth: u32) -> Option<u64> {
        if depth > 64 {
            return None;
        }
        match self.types.get(self.skip_mods(id) as usize)? {
            BtfType::Int { size, .. }
            | BtfType::Struct { size, .. }
            | BtfType::Enum { size, .. }
            | BtfType::Float { size, .. }
            | BtfType::Datasec { size, .. } => Some(*size as u64),
            BtfType::Ptr { .. } => Some(8),
            BtfType::Array { elem, len, .. } => {
                self.size_of_depth(*elem, depth + 1)?.checked_mul(*len as u64)
            }
            _ => None,
        }
    }

    /// Total number of types including `void`.
    pub fn len(&self) -> usize {
        self.types.len()
    }

    pub fn is_empty(&self) -> bool {
        self.types.len() <= 1
    }
}

/// Builder for emitted BTF.
#[derive(Debug, Default)]
pub struct BtfBuilder {
    types: Vec<u8>,
    strings: Vec<u8>,
    str_index: HashMap<String, u32>,
    count: u32,
}

impl BtfBuilder {
    pub fn new() -> Self {
        let mut b = Self::default();
        b.strings.push(0);
        b
    }

    /// Offset of `s` in the string section (added once).
    pub fn str(&mut self, s: &str) -> u32 {
        if s.is_empty() {
            return 0;
        }
        if let Some(&o) = self.str_index.get(s) {
            return o;
        }
        let o = self.strings.len() as u32;
        self.strings.extend_from_slice(s.as_bytes());
        self.strings.push(0);
        self.str_index.insert(s.to_owned(), o);
        o
    }

    pub fn word(&mut self, v: u32) {
        self.types.extend_from_slice(&v.to_le_bytes());
    }

    /// Append a type header; returns the new type id.
    pub fn header(&mut self, name: u32, kind: u32, vlen: u32, kflag: bool, size_or_type: u32) -> u32 {
        self.word(name);
        self.word((kind << 24) | (vlen & 0xffff) | if kflag { 1 << 31 } else { 0 });
        self.word(size_or_type);
        self.count += 1;
        self.count
    }

    pub fn int(&mut self, name: &str, size: u32, signed: bool) -> u32 {
        let n = self.str(name);
        let t = self.header(n, KIND_INT, 0, false, size);
        self.word((if signed { INT_SIGNED } else { 0 } << 24) | (size * 8));
        t
    }

    pub fn ptr(&mut self, to: u32) -> u32 {
        self.header(0, KIND_PTR, 0, false, to)
    }

    pub fn array(&mut self, elem: u32, index: u32, len: u32) -> u32 {
        let t = self.header(0, KIND_ARRAY, 0, false, 0);
        self.word(elem);
        self.word(index);
        self.word(len);
        t
    }

    /// `members`: (name, type, bit offset).
    pub fn structure(&mut self, name: &str, union: bool, size: u32, members: &[(&str, u32, u32)]) -> u32 {
        let names: Vec<u32> = members.iter().map(|m| self.str(m.0)).collect();
        let n = self.str(name);
        let kind = if union { KIND_UNION } else { KIND_STRUCT };
        let t = self.header(n, kind, members.len() as u32, false, size);
        for (m, nm) in members.iter().zip(names) {
            self.word(nm);
            self.word(m.1);
            self.word(m.2);
        }
        t
    }

    pub fn enumeration(&mut self, name: &str, size: u32, values: &[(&str, i64)]) -> u32 {
        let signed = values.iter().any(|v| v.1 < 0);
        let names: Vec<u32> = values.iter().map(|v| self.str(v.0)).collect();
        let n = self.str(name);
        let wide = values.iter().any(|v| v.1 > u32::MAX as i64 || v.1 < i32::MIN as i64);
        if wide {
            let t = self.header(n, KIND_ENUM64, values.len() as u32, signed, size);
            for (v, nm) in values.iter().zip(names) {
                self.word(nm);
                self.word(v.1 as u64 as u32);
                self.word((v.1 as u64 >> 32) as u32);
            }
            t
        } else {
            let t = self.header(n, KIND_ENUM, values.len() as u32, signed, size);
            for (v, nm) in values.iter().zip(names) {
                self.word(nm);
                self.word(v.1 as u32);
            }
            t
        }
    }

    pub fn func_proto(&mut self, ret: u32, params: &[(&str, u32)]) -> u32 {
        let names: Vec<u32> = params.iter().map(|p| self.str(p.0)).collect();
        let t = self.header(0, KIND_FUNC_PROTO, params.len() as u32, false, ret);
        for (p, nm) in params.iter().zip(names) {
            self.word(nm);
            self.word(p.1);
        }
        t
    }

    pub fn func(&mut self, name: &str, proto: u32, linkage: u32) -> u32 {
        let n = self.str(name);
        self.header(n, KIND_FUNC, linkage, false, proto)
    }

    pub fn var(&mut self, name: &str, ty: u32, linkage: u32) -> u32 {
        let n = self.str(name);
        let t = self.header(n, KIND_VAR, 0, false, ty);
        self.word(linkage);
        t
    }

    /// `vars`: (var type id, offset, size).
    pub fn datasec(&mut self, name: &str, size: u32, vars: &[(u32, u32, u32)]) -> u32 {
        let n = self.str(name);
        let t = self.header(n, KIND_DATASEC, vars.len() as u32, false, size);
        for v in vars {
            self.word(v.0);
            self.word(v.1);
            self.word(v.2);
        }
        t
    }

    pub fn finish(self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(&BTF_MAGIC.to_le_bytes());
        out.push(1); // version
        out.push(0); // flags
        out.extend_from_slice(&24u32.to_le_bytes()); // hdr_len
        out.extend_from_slice(&0u32.to_le_bytes()); // type_off
        out.extend_from_slice(&(self.types.len() as u32).to_le_bytes());
        out.extend_from_slice(&(self.types.len() as u32).to_le_bytes()); // str_off
        out.extend_from_slice(&(self.strings.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.types);
        out.extend_from_slice(&self.strings);
        out
    }
}

/// Path of the running kernel's BTF.
pub const VMLINUX_PATH: &str = "/sys/kernel/btf/vmlinux";

/// Parse the running kernel's BTF ([`VMLINUX_PATH`]).
pub fn host_vmlinux() -> Result<Btf, BtfError> {
    let bytes = std::fs::read(VMLINUX_PATH).map_err(|e| bad(format!("{VMLINUX_PATH}: {e}")))?;
    Btf::parse(&bytes)
}

/// Write the BTF id of each kfunc call's target (`FUNC` named
/// [`KfuncRef::name`](crate::program::KfuncRef) in `target`) into its `CALL`
/// immediate, as libbpf does for `__ksym` externs in vmlinux. The result
/// is what `BPF_PROG_LOAD` takes (BTF object fd 0 = vmlinux).
pub fn resolve_kfuncs(prog: &crate::Program, target: &Btf) -> Result<crate::Program, BtfError> {
    let mut out = prog.clone();
    for k in &prog.kfuncs {
        let id = target
            .func_id(&k.name)
            .ok_or_else(|| bad(format!("kfunc {} is not in the target BTF", k.name)))?;
        let slot = out
            .insns
            .get_mut(k.slot)
            .ok_or_else(|| bad(format!("kfunc {} at missing slot {}", k.name, k.slot)))?;
        if slot.code != 0x85 || slot.src != crate::insn::pseudo::KFUNC_CALL {
            return Err(bad(format!("slot {} is not a kfunc call", k.slot)));
        }
        slot.imm = i32::try_from(id).map_err(|_| bad("BTF id out of range"))?;
        slot.off = 0;
    }
    Ok(out)
}

/// A `.BTF.ext` / `BPF_PROG_LOAD` func_info record (`struct
/// bpf_func_info`): the subprogram starting at instruction `insn_off` has
/// BTF type `type_id` (a `FUNC`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FuncInfo {
    pub insn_off: u32,
    pub type_id: u32,
}

impl FuncInfo {
    pub const REC_SIZE: u32 = 8;

    pub fn bytes(infos: &[FuncInfo]) -> Vec<u8> {
        let mut out = Vec::with_capacity(infos.len() * 8);
        for f in infos {
            out.extend_from_slice(&f.insn_off.to_le_bytes());
            out.extend_from_slice(&f.type_id.to_le_bytes());
        }
        out
    }
}

/// Entry slots of every subprogram: 0, the targets of BPF-to-BPF calls and
/// of `BPF_PSEUDO_FUNC` loads, sorted.
pub fn subprog_starts(prog: &crate::Program) -> Vec<usize> {
    use crate::insn::{class, pseudo};
    let mut starts = vec![0usize];
    let n = prog.insns.len() as i64;
    for (pc, i) in prog.insns.iter().enumerate() {
        let local_call = i.code == (class::JMP | 0x80) && i.src == pseudo::CALL;
        let func_load = i.code == 0x18 && i.src == pseudo::FUNC;
        if local_call || func_load {
            let t = pc as i64 + 1 + i.imm as i64;
            if (0..n).contains(&t) {
                starts.push(t as usize);
            }
        }
    }
    starts.sort_unstable();
    starts.dedup();
    starts
}

fn c_identifier(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// The `FUNC` types of a program's subprograms: the main program is
/// `int name(void *ctx)` (global), every other subprogram (BPF-to-BPF
/// callee or `bpf_loop` callback) is `int name(u64 a1, void *a2)`
/// (static, so the kernel verifies it in its caller's context). Returns
/// the builder (to add more types, e.g. for maps) and one func_info record
/// per subprogram.
pub fn add_func_types(b: &mut BtfBuilder, prog: &crate::Program) -> Vec<FuncInfo> {
    let int = b.int("int", 4, true);
    let u64t = b.int("unsigned long long", 8, false);
    let void_ptr = b.ptr(0);
    let main_proto = b.func_proto(int, &[("ctx", void_ptr)]);
    let sub_proto = b.func_proto(int, &[("a1", u64t), ("a2", void_ptr)]);
    let mut infos = Vec::new();
    for start in subprog_starts(prog) {
        let named = prog
            .functions
            .iter()
            .find(|(_, e)| *e == start)
            .map(|(n, _)| n.clone());
        let (name, proto, linkage) = if start == 0 {
            (named.unwrap_or_else(|| prog.name.clone()), main_proto, FUNC_GLOBAL)
        } else {
            (
                named.unwrap_or_else(|| format!("sub_{start}")),
                sub_proto,
                FUNC_STATIC,
            )
        };
        let id = b.func(&c_identifier(&name), proto, linkage);
        infos.push(FuncInfo {
            insn_off: start as u32,
            type_id: id,
        });
    }
    infos
}

/// Program BTF for `BPF_PROG_LOAD` (`prog_btf_fd`) with func_info: the
/// kernel requires it for programs that pass callbacks (`bpf_loop`).
pub fn program_btf(prog: &crate::Program) -> (Vec<u8>, Vec<FuncInfo>) {
    let mut b = BtfBuilder::new();
    let infos = add_func_types(&mut b, prog);
    (b.finish(), infos)
}

