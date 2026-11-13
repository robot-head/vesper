//! Object BTF for programs that need more than map definitions: the
//! `.BTF` of an emitted object (maps, one `FUNC` per subprogram, extern
//! `FUNC`s for kfuncs in a `.ksyms` `DATASEC`, the local types of CO-RE
//! relocations) and its `.BTF.ext` (func info, line info and CO-RE
//! relocation records), plus the reader for both.
//!
//! Layout of `.BTF.ext` (`struct btf_ext_header`, version 1): a 32-byte
//! header with the offset and length of the func-info, line-info and
//! CO-RE-relocation subsections (relative to the end of the header); each
//! subsection is a record size followed by one block per program section
//! (section name offset into the `.BTF` strings, record count, records).
//! Instruction offsets in the records are in bytes.

use crate::btf::{
    BTF_MAGIC, Btf, BtfBuilder, BtfError, BtfType, FUNC_EXTERN, FuncInfo, INT_SIGNED,
    VAR_GLOBAL_ALLOCATED, add_func_types,
};
use crate::core::{CoreKind, CoreRelo, CoreSpec, LocalMember, LocalType};
use crate::program::{LineInfo, MapDef, MapKind, Program};
use std::collections::HashMap;

const EXT_HDR_LEN: u32 = 32;
const LINE_REC_SIZE: u32 = 16;
const CORE_REC_SIZE: u32 = 16;

fn bad(m: impl Into<String>) -> BtfError {
    BtfError(m.into())
}

/// `.BTF` and `.BTF.ext` of one program section.
#[derive(Clone, Debug)]
pub struct ObjectBtf {
    pub btf: Vec<u8>,
    pub ext: Vec<u8>,
    /// Byte offset of each map in `.maps`.
    pub map_offsets: Vec<u64>,
    pub maps_size: u64,
    /// The func-info records (instruction offsets in slots).
    pub func_info: Vec<FuncInfo>,
    /// Line-info records `[insn byte offset, file, line text, line << 10 | col]`.
    pub line_records: Vec<[u32; 4]>,
    /// CO-RE records `[insn byte offset, local type id, access string, kind]`.
    pub core_records: Vec<[u32; 4]>,
}

/// The `__uint()` members of a BTF-defined map.
pub(crate) fn map_fields(m: &MapDef) -> Vec<(&'static str, u32)> {
    let mut f = vec![("type", m.kind.kernel_type())];
    if m.kind != MapKind::RingBuf {
        f.push(("key_size", m.key_size));
        f.push(("value_size", m.value_size));
    }
    f.push(("max_entries", m.max_entries));
    if m.flags != 0 {
        f.push(("map_flags", m.flags));
    }
    f
}

struct Types<'a> {
    b: &'a mut BtfBuilder,
    cache: HashMap<LocalType, u32>,
    u32_index: Option<u32>,
}

impl Types<'_> {
    fn index_type(&mut self) -> u32 {
        if let Some(t) = self.u32_index {
            return t;
        }
        let t = self.b.int("__ARRAY_SIZE_TYPE__", 4, false);
        self.u32_index = Some(t);
        t
    }

    fn local(&mut self, t: &LocalType) -> u32 {
        if let Some(&id) = self.cache.get(t) {
            return id;
        }
        let id = match t {
            LocalType::Void => 0,
            LocalType::Int { name, size, signed } => self.b.int(name, *size, *signed),
            LocalType::Ptr(to) => {
                let to = self.local(to);
                self.b.ptr(to)
            }
            LocalType::Array { elem, len } => {
                let e = self.local(elem);
                let i = self.index_type();
                self.b.array(e, i, *len)
            }
            LocalType::Struct {
                name,
                union,
                size,
                members,
            } => {
                let tys: Vec<u32> = members.iter().map(|m| self.local(&m.ty)).collect();
                let ms: Vec<(&str, u32, u32)> = members
                    .iter()
                    .zip(&tys)
                    .map(|(m, &t)| (m.name.as_str(), t, m.offset * 8))
                    .collect();
                self.b.structure(name, *union, *size, &ms)
            }
            LocalType::Enum { name, size, values } => {
                let vs: Vec<(&str, i64)> = values.iter().map(|(n, v)| (n.as_str(), *v)).collect();
                self.b.enumeration(name, *size, &vs)
            }
        };
        self.cache.insert(t.clone(), id);
        id
    }
}

fn subsection(rec_size: u32, sec_name: u32, records: &[[u32; 4]], words: usize) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&rec_size.to_le_bytes());
    if !records.is_empty() {
        out.extend_from_slice(&sec_name.to_le_bytes());
        out.extend_from_slice(&(records.len() as u32).to_le_bytes());
        for r in records {
            for w in &r[..words] {
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
    }
    out
}

/// Build the object BTF of `prog`, whose code lives in section `section`.
pub fn build(prog: &Program, section: &str) -> Result<ObjectBtf, BtfError> {
    let mut b = BtfBuilder::new();
    // maps, encoded as clang encodes `__uint(name, value)` members
    let int = b.int("int", 4, true);
    let mut ptr_for: HashMap<u32, u32> = HashMap::new();
    let mut map_offsets = Vec::new();
    let mut vars = Vec::new();
    let mut at = 0u64;
    for m in &prog.maps {
        let fields = map_fields(m);
        let mut members = Vec::new();
        for (i, &(n, v)) in fields.iter().enumerate() {
            let t = match ptr_for.get(&v) {
                Some(&t) => t,
                None => {
                    let arr = b.array(int, int, v);
                    let p = b.ptr(arr);
                    ptr_for.insert(v, p);
                    p
                }
            };
            members.push((n, t, i as u32 * 64));
        }
        let size = 8 * fields.len() as u32;
        let st = b.structure("", false, size, &members);
        let var = b.var(&m.name, st, VAR_GLOBAL_ALLOCATED);
        vars.push((var, at as u32, size));
        map_offsets.push(at);
        at += size as u64;
    }
    if !vars.is_empty() {
        b.datasec(".maps", at as u32, &vars);
    }
    let func_info = add_func_types(&mut b, prog);
    // kfunc externs: `extern void name(void) __ksym;`
    let mut kfunc_names: Vec<&str> = prog.kfuncs.iter().map(|k| k.name.as_str()).collect();
    kfunc_names.sort_unstable();
    kfunc_names.dedup();
    if !kfunc_names.is_empty() {
        let proto = b.func_proto(0, &[]);
        let ks: Vec<(u32, u32, u32)> = kfunc_names
            .iter()
            .map(|n| (b.func(n, proto, FUNC_EXTERN), 0, 0))
            .collect();
        b.datasec(".ksyms", 0, &ks);
    }
    // CO-RE local types and records
    let mut core = Vec::new();
    {
        let mut t = Types {
            b: &mut b,
            cache: HashMap::new(),
            u32_index: None,
        };
        for r in &prog.core_relos {
            let root = t.local(&r.spec.root);
            let access = t.b.str(&r.spec.access_string());
            core.push([(r.slot * 8) as u32, root, access, r.spec.kind.code()]);
        }
    }
    let mut lines = Vec::new();
    for l in &prog.lines {
        if l.col >= 1 << 10 || l.line >= 1 << 22 {
            return Err(bad(format!("line info {}:{} out of range", l.line, l.col)));
        }
        lines.push([
            (l.slot * 8) as u32,
            b.str(&l.file),
            b.str(&l.text),
            (l.line << 10) | l.col,
        ]);
    }
    let sec = b.str(section);
    let fi: Vec<[u32; 4]> = func_info
        .iter()
        .map(|f| [f.insn_off * 8, f.type_id, 0, 0])
        .collect();
    let f = subsection(FuncInfo::REC_SIZE, sec, &fi, 2);
    let l = subsection(LINE_REC_SIZE, sec, &lines, 4);
    let c = subsection(CORE_REC_SIZE, sec, &core, 4);
    let mut ext = Vec::new();
    ext.extend_from_slice(&BTF_MAGIC.to_le_bytes());
    ext.push(1);
    ext.push(0);
    let mut off = 0u32;
    ext.extend_from_slice(&EXT_HDR_LEN.to_le_bytes());
    for part in [&f, &l, &c] {
        ext.extend_from_slice(&off.to_le_bytes());
        ext.extend_from_slice(&(part.len() as u32).to_le_bytes());
        off += part.len() as u32;
    }
    for part in [f, l, c] {
        ext.extend_from_slice(&part);
    }
    Ok(ObjectBtf {
        btf: b.finish(),
        ext,
        map_offsets,
        maps_size: at,
        func_info,
        line_records: lines,
        core_records: core,
    })
}

/// What [`parse_ext`] reads back from `.BTF.ext` for one section.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ObjectExt {
    /// (BTF function name, entry slot), sorted by slot.
    pub functions: Vec<(String, usize)>,
    pub lines: Vec<LineInfo>,
    pub core_relos: Vec<CoreRelo>,
}

fn rd(b: &[u8], at: usize) -> Result<u32, BtfError> {
    at.checked_add(4)
        .and_then(|e| b.get(at..e))
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| bad(".BTF.ext truncated"))
}

/// The records of `section` in one subsection: (record size, records).
fn records<'a>(
    sub: &'a [u8],
    btf: &Btf,
    section: &str,
    min_rec: u32,
) -> Result<Vec<&'a [u8]>, BtfError> {
    if sub.is_empty() {
        return Ok(Vec::new());
    }
    let rec = rd(sub, 0)?;
    if rec < min_rec || rec % 4 != 0 || rec > 256 {
        return Err(bad(format!(".BTF.ext record size {rec}")));
    }
    let mut out = Vec::new();
    let mut at = 4usize;
    while at < sub.len() {
        let name = rd(sub, at)?;
        let n = rd(sub, at + 4)? as usize;
        at += 8;
        let len = n
            .checked_mul(rec as usize)
            .ok_or_else(|| bad(".BTF.ext count overflow"))?;
        let end = at.checked_add(len).ok_or_else(|| bad(".BTF.ext overflow"))?;
        let body = sub
            .get(at..end)
            .ok_or_else(|| bad(".BTF.ext records past the end"))?;
        if btf.name(name) == section {
            out.extend(body.chunks_exact(rec as usize));
        }
        at = end;
    }
    Ok(out)
}

fn slot_of(byte_off: u32) -> Result<usize, BtfError> {
    if byte_off % 8 != 0 {
        return Err(bad(format!("instruction offset {byte_off} is not a slot")));
    }
    Ok((byte_off / 8) as usize)
}

/// Parse the `.BTF.ext` records of `section` against the object's `.BTF`.
pub fn parse_ext(btf: &Btf, ext: &[u8], section: &str) -> Result<ObjectExt, BtfError> {
    if ext.len() < 8 || u16::from_le_bytes([ext[0], ext[1]]) != BTF_MAGIC || ext[2] != 1 {
        return Err(bad(".BTF.ext: bad header"));
    }
    let hdr = rd(ext, 4)? as usize;
    if hdr < 24 {
        return Err(bad(".BTF.ext header too short"));
    }
    let part = |i: usize| -> Result<&[u8], BtfError> {
        if 8 + i * 8 + 8 > hdr {
            return Ok(&[]);
        }
        let off = rd(ext, 8 + i * 8)? as usize;
        let len = rd(ext, 12 + i * 8)? as usize;
        let s = hdr.checked_add(off).ok_or_else(|| bad("overflow"))?;
        let e = s.checked_add(len).ok_or_else(|| bad("overflow"))?;
        ext.get(s..e)
            .ok_or_else(|| bad(".BTF.ext subsection out of range"))
    };
    let mut out = ObjectExt::default();
    for r in records(part(0)?, btf, section, 8)? {
        let (slot, ty) = (slot_of(rd(r, 0)?)?, rd(r, 4)?);
        let Some(BtfType::Func { name, .. }) = btf.get(ty) else {
            return Err(bad(format!("func info type {ty} is not a FUNC")));
        };
        out.functions.push((btf.name(*name).to_owned(), slot));
    }
    for r in records(part(1)?, btf, section, 16)? {
        let lc = rd(r, 12)?;
        out.lines.push(LineInfo {
            slot: slot_of(rd(r, 0)?)?,
            file: btf.name(rd(r, 4)?).to_owned(),
            text: btf.name(rd(r, 8)?).to_owned(),
            line: lc >> 10,
            col: lc & 0x3ff,
        });
    }
    for r in records(part(2)?, btf, section, 16)? {
        let kind = rd(r, 12)?;
        let kind = CoreKind::from_code(kind)
            .ok_or_else(|| bad(format!("unsupported CO-RE relocation kind {kind}")))?;
        let access: Vec<u32> = btf
            .name(rd(r, 8)?)
            .split(':')
            .map(|p| p.parse().map_err(|_| bad(format!("CO-RE access string {p:?}"))))
            .collect::<Result<_, _>>()?;
        if access.is_empty() {
            return Err(bad("empty CO-RE access string"));
        }
        out.core_relos.push(CoreRelo {
            slot: slot_of(rd(r, 0)?)?,
            spec: CoreSpec {
                kind,
                root: local_type(btf, rd(r, 4)?)?,
                access,
            },
        });
    }
    out.functions.sort_by_key(|(_, s)| *s);
    Ok(out)
}

/// Convert an object BTF type back into a [`LocalType`].
pub fn local_type(btf: &Btf, id: u32) -> Result<LocalType, BtfError> {
    local_depth(btf, id, 0)
}

fn local_depth(btf: &Btf, id: u32, depth: u32) -> Result<LocalType, BtfError> {
    if depth > 16 {
        return Err(bad("CO-RE local type nests too deeply"));
    }
    let t = btf
        .get(btf.skip_mods(id))
        .ok_or_else(|| bad(format!("no type {id}")))?;
    Ok(match t {
        BtfType::Void => LocalType::Void,
        BtfType::Int {
            name,
            size,
            encoding,
            ..
        } => LocalType::Int {
            name: btf.name(*name).to_owned(),
            size: *size,
            signed: encoding & INT_SIGNED != 0,
        },
        BtfType::Ptr { ty } => LocalType::Ptr(Box::new(local_depth(btf, *ty, depth + 1)?)),
        BtfType::Array { elem, len, .. } => LocalType::Array {
            elem: Box::new(local_depth(btf, *elem, depth + 1)?),
            len: *len,
        },
        BtfType::Struct {
            name,
            size,
            union,
            members,
        } => {
            let mut ms = Vec::new();
            for m in members {
                if m.bitfield_size != 0 || m.bit_offset % 8 != 0 {
                    return Err(bad("bitfield member in a CO-RE local type"));
                }
                ms.push(LocalMember {
                    name: btf.name(m.name).to_owned(),
                    ty: local_depth(btf, m.ty, depth + 1)?,
                    offset: m.bit_offset / 8,
                });
            }
            LocalType::Struct {
                name: btf.name(*name).to_owned(),
                union: *union,
                size: *size,
                members: ms,
            }
        }
        BtfType::Enum {
            name, size, values, ..
        } => LocalType::Enum {
            name: btf.name(*name).to_owned(),
            size: *size,
            values: values
                .iter()
                .map(|(n, v)| (btf.name(*n).to_owned(), *v))
                .collect(),
        },
        other => return Err(bad(format!("unsupported CO-RE local type {other:?}"))),
    })
}

/// Map definitions (offset in `.maps`, definition) from an object's BTF.
pub fn maps_from_btf(btf: &Btf) -> Result<Vec<(u64, MapDef)>, BtfError> {
    let mut out = Vec::new();
    for t in &btf.types {
        let BtfType::Datasec { name, vars, .. } = t else {
            continue;
        };
        if btf.name(*name) != ".maps" {
            continue;
        }
        for &(var, off, _) in vars {
            let Some(BtfType::Var { name, ty, .. }) = btf.get(var) else {
                return Err(bad(".maps entry is not a VAR"));
            };
            let Some(BtfType::Struct { members, .. }) = btf.get(btf.skip_mods(*ty)) else {
                return Err(bad("map is not a struct"));
            };
            let mut def = MapDef::new(btf.name(*name), MapKind::Array, 0, 0, 0);
            let mut kind = None;
            for m in members {
                let Some(BtfType::Ptr { ty: arr }) = btf.get(m.ty) else {
                    return Err(bad("map member is not __uint()"));
                };
                let Some(BtfType::Array { len, .. }) = btf.get(*arr) else {
                    return Err(bad("map member is not __uint()"));
                };
                match btf.name(m.name) {
                    "type" => {
                        kind = Some(
                            MapKind::from_kernel_type(*len)
                                .ok_or_else(|| bad(format!("unsupported map type {len}")))?,
                        )
                    }
                    "key_size" => def.key_size = *len,
                    "value_size" => def.value_size = *len,
                    "max_entries" => def.max_entries = *len,
                    "map_flags" => def.flags = *len,
                    other => return Err(bad(format!("unsupported map member {other}"))),
                }
            }
            def.kind = kind.ok_or_else(|| bad("map without type"))?;
            out.push((off as u64, def));
        }
    }
    Ok(out)
}
