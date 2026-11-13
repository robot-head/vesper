//! BPF ELF emission and read-back (`object` crate).
//!
//! The emitted relocatable object follows the layout libbpf and aya load:
//!
//! * one executable section per program, named after its program type
//!   (`xdp`, `tc`, `tracepoint/<category>/<event>`, `kprobe/<function>`),
//!   holding the instructions with `LD_IMM64` map immediates set
//!   to zero and an `R_BPF_64_64` relocation (in `.relxdp`) to the map's
//!   symbol for each;
//! * `license` with the NUL-terminated license string;
//! * `.maps` with one zero-filled BTF-defined map variable per map
//!   (`__uint(type, ...)`, `__uint(key_size, ...)`, ... members encoded,
//!   as clang does, as pointers to arrays whose length is the value);
//! * `.BTF` with the minimal type information describing `.maps`
//!   (`int`, the array and pointer types, one struct and `VAR` per map and
//!   the `.maps` `DATASEC`). No `.BTF.ext` (line info, CO-RE) is emitted:
//!   `@btf_core` relocations are out of scope.
//!
//! Map definitions are read back from the BTF by [`read_object`], so the
//! round trip exercises the same information an external loader uses.

use crate::insn::{RawInsn, pseudo, slots_from_bytes};
use crate::program::{MapDef, MapKind, Program, ProgramType};
use object::write::{Object, Relocation, SectionId, Symbol, SymbolId, SymbolSection};
use object::{
    Architecture, BinaryFormat, Endianness, Object as _, ObjectSection as _, ObjectSymbol as _,
    RelocationFlags, SectionKind, SymbolFlags, SymbolKind, SymbolScope,
};
use std::collections::HashMap;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ElfError {
    Write(String),
    Read(String),
    Malformed(String),
}

impl fmt::Display for ElfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ElfError::Write(m) => write!(f, "ELF write: {m}"),
            ElfError::Read(m) => write!(f, "ELF read: {m}"),
            ElfError::Malformed(m) => write!(f, "malformed BPF object: {m}"),
        }
    }
}

impl std::error::Error for ElfError {}

/// Section name for a program type (libbpf `SEC()` convention).
pub fn section_name(t: ProgramType) -> &'static str {
    match t {
        ProgramType::Xdp => "xdp",
        ProgramType::SchedCls => "classifier",
        ProgramType::Tracepoint => "tracepoint",
        ProgramType::Kprobe => "kprobe",
    }
}

/// The program section of `prog`: the type's name, then `/` and the
/// attach target when there is one (`kprobe/do_unlinkat`,
/// `tracepoint/syscalls/sys_enter_openat`).
pub fn program_section(prog: &Program) -> String {
    if prog.attach.is_empty() {
        section_name(prog.prog_type).to_owned()
    } else {
        format!("{}/{}", section_name(prog.prog_type), prog.attach)
    }
}

/// Program type and attach target of a program section name (libbpf
/// names, including the `classifier` and `tp` aliases).
pub fn parse_section(name: &str) -> Option<(ProgramType, String)> {
    let (head, attach) = match name.split_once('/') {
        Some((h, a)) => (h, a.to_owned()),
        None => (name, String::new()),
    };
    let t = match head {
        "xdp" => ProgramType::Xdp,
        "tc" | "classifier" => ProgramType::SchedCls,
        "tracepoint" | "tp" => ProgramType::Tracepoint,
        "kprobe" => ProgramType::Kprobe,
        _ => return None,
    };
    if t == ProgramType::Xdp && !attach.is_empty() {
        return None;
    }
    Some((t, attach))
}

// ---------------------------------------------------------------- BTF ---

const BTF_MAGIC: u16 = 0xeb9f;
const KIND_INT: u32 = 1;
const KIND_PTR: u32 = 2;
const KIND_ARRAY: u32 = 3;
const KIND_STRUCT: u32 = 4;
const KIND_VAR: u32 = 14;
const KIND_DATASEC: u32 = 15;
const BTF_INT_SIGNED: u32 = 1;
const VAR_GLOBAL_ALLOCATED: u32 = 1;

/// Fields of one BTF-defined map, in emission order.
fn map_fields(m: &MapDef) -> Vec<(&'static str, u32)> {
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

#[derive(Default)]
struct BtfBuilder {
    types: Vec<u8>,
    strings: Vec<u8>,
    str_index: HashMap<String, u32>,
    count: u32,
}

impl BtfBuilder {
    fn new() -> Self {
        let mut b = Self::default();
        b.strings.push(0);
        b
    }

    fn str(&mut self, s: &str) -> u32 {
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

    fn u32(&mut self, v: u32) {
        self.types.extend_from_slice(&v.to_le_bytes());
    }

    fn header(&mut self, name: u32, kind: u32, vlen: u32, size_or_type: u32) -> u32 {
        self.u32(name);
        self.u32((kind << 24) | (vlen & 0xffff));
        self.u32(size_or_type);
        self.count += 1;
        self.count
    }

    fn finish(self) -> Vec<u8> {
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

/// Minimal `.BTF` for the program's `.maps` section. Returns the blob and
/// each map's byte offset in `.maps` and the section size.
pub fn maps_btf(maps: &[MapDef]) -> (Vec<u8>, Vec<u64>, u64) {
    let mut b = BtfBuilder::new();
    let int_name = b.str("int");
    let int = b.header(int_name, KIND_INT, 0, 4);
    b.u32((BTF_INT_SIGNED << 24) | 32);
    let mut ptr_for: HashMap<u32, u32> = HashMap::new();
    let mut offsets = Vec::new();
    let mut vars = Vec::new();
    let mut at = 0u64;
    for m in maps {
        let fields = map_fields(m);
        let mut member_types = Vec::new();
        for &(_, v) in &fields {
            let t = match ptr_for.get(&v) {
                Some(&t) => t,
                None => {
                    let arr = b.header(0, KIND_ARRAY, 0, 0);
                    b.u32(int);
                    b.u32(int);
                    b.u32(v);
                    let ptr = b.header(0, KIND_PTR, 0, arr);
                    ptr_for.insert(v, ptr);
                    ptr
                }
            };
            member_types.push(t);
        }
        let size = 8 * fields.len() as u32;
        let names: Vec<u32> = fields.iter().map(|(n, _)| b.str(n)).collect();
        let st = b.header(0, KIND_STRUCT, fields.len() as u32, size);
        for (i, (&n, &t)) in names.iter().zip(&member_types).enumerate() {
            b.u32(n);
            b.u32(t);
            b.u32(i as u32 * 64);
        }
        let vname = b.str(&m.name);
        let var = b.header(vname, KIND_VAR, 0, st);
        b.u32(VAR_GLOBAL_ALLOCATED);
        vars.push((var, at, size));
        offsets.push(at);
        at += size as u64;
    }
    if !vars.is_empty() {
        let sname = b.str(".maps");
        b.header(sname, KIND_DATASEC, vars.len() as u32, at as u32);
        for (t, off, size) in vars {
            b.u32(t);
            b.u32(off as u32);
            b.u32(size);
        }
    }
    (b.finish(), offsets, at)
}

/// Parse the `.maps` definitions back out of a `.BTF` blob. Returns
/// `(name, offset in .maps, def)` per map variable.
pub fn parse_maps_btf(btf: &[u8]) -> Result<Vec<(u64, MapDef)>, ElfError> {
    let bad = |m: &str| ElfError::Malformed(format!("BTF: {m}"));
    let rd = |b: &[u8], at: usize| -> Result<u32, ElfError> {
        b.get(at..at + 4)
            .map(|s| u32::from_le_bytes(s.try_into().expect("4 bytes")))
            .ok_or_else(|| bad("truncated"))
    };
    if btf.len() < 24 || u16::from_le_bytes([btf[0], btf[1]]) != BTF_MAGIC {
        return Err(bad("bad header"));
    }
    let hdr_len = rd(btf, 4)? as usize;
    let type_off = rd(btf, 8)? as usize;
    let type_len = rd(btf, 12)? as usize;
    let str_off = rd(btf, 16)? as usize;
    let str_len = rd(btf, 20)? as usize;
    let span = |o: usize, l: usize| -> Result<&[u8], ElfError> {
        let s = hdr_len.checked_add(o).ok_or_else(|| bad("overflow"))?;
        let e = s.checked_add(l).ok_or_else(|| bad("overflow"))?;
        btf.get(s..e).ok_or_else(|| bad("section out of range"))
    };
    let types = span(type_off, type_len)?;
    let strs = span(str_off, str_len)?;
    let name = |o: u32| -> Result<String, ElfError> {
        let s = strs.get(o as usize..).ok_or_else(|| bad("name offset"))?;
        let end = s
            .iter()
            .position(|&c| c == 0)
            .ok_or_else(|| bad("unterminated name"))?;
        String::from_utf8(s[..end].to_vec()).map_err(|_| bad("non-UTF-8 name"))
    };
    // type id -> (kind, name_off, size_or_type, extra words)
    struct T {
        kind: u32,
        name: u32,
        sot: u32,
        extra: Vec<u32>,
        vlen: u32,
    }
    let mut table: Vec<T> = vec![T {
        kind: 0,
        name: 0,
        sot: 0,
        extra: vec![],
        vlen: 0,
    }];
    let mut at = 0;
    while at < types.len() {
        let n = rd(types, at)?;
        let info = rd(types, at + 4)?;
        let sot = rd(types, at + 8)?;
        at += 12;
        let kind = (info >> 24) & 0x1f;
        let vlen = info & 0xffff;
        let words = match kind {
            KIND_INT | KIND_VAR => 1,
            KIND_PTR => 0,
            KIND_ARRAY => 3,
            KIND_STRUCT | KIND_DATASEC => 3 * vlen as usize,
            k => return Err(bad(&format!("unsupported kind {k}"))),
        };
        let mut extra = Vec::with_capacity(words);
        for _ in 0..words {
            extra.push(rd(types, at)?);
            at += 4;
        }
        table.push(T {
            kind,
            name: n,
            sot,
            extra,
            vlen,
        });
    }
    let get = |id: u32| {
        table
            .get(id as usize)
            .ok_or_else(|| bad("type id out of range"))
    };
    let mut out = Vec::new();
    for t in &table {
        if t.kind != KIND_DATASEC || name(t.name)? != ".maps" {
            continue;
        }
        for v in t.extra.chunks(3) {
            let (var_id, off) = (v[0], v[1] as u64);
            let var = get(var_id)?;
            if var.kind != KIND_VAR {
                return Err(bad("datasec entry is not a VAR"));
            }
            let st = get(var.sot)?;
            if st.kind != KIND_STRUCT {
                return Err(bad("map variable is not a struct"));
            }
            let mut def = MapDef::new(&name(var.name)?, MapKind::Hash, 0, 0, 0);
            let mut kind = None;
            for mem in st.extra.chunks(3).take(st.vlen as usize) {
                let ptr = get(mem[1])?;
                if ptr.kind != KIND_PTR {
                    return Err(bad("map member is not __uint()"));
                }
                let arr = get(ptr.sot)?;
                if arr.kind != KIND_ARRAY {
                    return Err(bad("map member is not __uint()"));
                }
                let val = arr.extra[2];
                match name(mem[0])?.as_str() {
                    "type" => {
                        kind = Some(
                            MapKind::from_kernel_type(val)
                                .ok_or_else(|| bad(&format!("unsupported map type {val}")))?,
                        )
                    }
                    "key_size" => def.key_size = val,
                    "value_size" => def.value_size = val,
                    "max_entries" => def.max_entries = val,
                    "map_flags" => def.flags = val,
                    other => return Err(bad(&format!("unsupported map member {other}"))),
                }
            }
            def.kind = kind.ok_or_else(|| bad("map without type"))?;
            out.push((off, def));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------- ELF ---

/// Whether the object needs function BTF and `.BTF.ext`: subprograms,
/// kfunc calls, CO-RE relocations or line info.
fn needs_ext(prog: &Program) -> bool {
    !prog.kfuncs.is_empty()
        || !prog.core_relos.is_empty()
        || !prog.lines.is_empty()
        || crate::btf::subprog_starts(prog).len() > 1
}

/// Emit a relocatable BPF ELF object for `prog`.
pub fn write_object(prog: &Program) -> Result<Vec<u8>, ElfError> {
    let mut obj = Object::new(BinaryFormat::Elf, Architecture::Bpf, Endianness::Little);
    let text = obj.add_section(
        Vec::new(),
        program_section(prog).into_bytes(),
        SectionKind::Text,
    );
    // map immediates are zero in the object; the relocation names the map
    let mut insns: Vec<RawInsn> = prog.insns.clone();
    for &(at, _) in &prog.map_relocs {
        let slot = insns
            .get_mut(at)
            .ok_or_else(|| ElfError::Write(format!("relocation at missing slot {at}")))?;
        slot.imm = 0;
    }
    let code = crate::insn::slots_to_bytes(&insns);
    obj.set_section_data(text, code.clone(), 8);

    let license = obj.add_section(Vec::new(), b"license".to_vec(), SectionKind::Data);
    let mut lic = prog.license.clone().into_bytes();
    lic.push(0);
    obj.set_section_data(license, lic.clone(), 1);
    obj.add_symbol(Symbol {
        name: b"_license".to_vec(),
        value: 0,
        size: lic.len() as u64,
        kind: SymbolKind::Data,
        scope: SymbolScope::Dynamic,
        weak: false,
        section: SymbolSection::Section(license),
        flags: SymbolFlags::None,
    });

    let mut map_syms: Vec<SymbolId> = Vec::new();
    let rich = needs_ext(prog);
    let ext = if rich {
        Some(
            crate::btf_ext::build(prog, &program_section(prog))
                .map_err(|e| ElfError::Write(e.to_string()))?,
        )
    } else {
        None
    };
    if let Some(ext) = &ext {
        let btf_sec = obj.add_section(Vec::new(), b".BTF".to_vec(), SectionKind::Metadata);
        obj.set_section_data(btf_sec, ext.btf.clone(), 4);
        let ext_sec = obj.add_section(Vec::new(), b".BTF.ext".to_vec(), SectionKind::Metadata);
        obj.set_section_data(ext_sec, ext.ext.clone(), 4);
    }
    if !prog.maps.is_empty() {
        let (btf, offsets, size) = match &ext {
            Some(e) => (Vec::new(), e.map_offsets.clone(), e.maps_size),
            None => maps_btf(&prog.maps),
        };
        let maps: SectionId = obj.add_section(Vec::new(), b".maps".to_vec(), SectionKind::Data);
        obj.set_section_data(maps, vec![0u8; size as usize], 8);
        for (m, off) in prog.maps.iter().zip(&offsets) {
            let size = 8 * map_fields(m).len() as u64;
            map_syms.push(obj.add_symbol(Symbol {
                name: m.name.as_bytes().to_vec(),
                value: *off,
                size,
                kind: SymbolKind::Data,
                scope: SymbolScope::Dynamic,
                weak: false,
                section: SymbolSection::Section(maps),
                flags: SymbolFlags::None,
            }));
        }
        if ext.is_none() {
            let btf_sec = obj.add_section(Vec::new(), b".BTF".to_vec(), SectionKind::Metadata);
            obj.set_section_data(btf_sec, btf, 4);
        }
    }

    // the entry function and named subprograms
    let mut entries: Vec<(String, usize)> = vec![(prog.name.clone(), 0)];
    for (n, e) in &prog.functions {
        if *e != 0 {
            entries.push((n.clone(), *e));
        }
    }
    entries.sort_by_key(|(_, e)| *e);
    for (i, (n, e)) in entries.iter().enumerate() {
        let end = entries.get(i + 1).map_or(insns.len(), |(_, e)| *e);
        obj.add_symbol(Symbol {
            name: n.as_bytes().to_vec(),
            value: *e as u64 * 8,
            size: (end - e) as u64 * 8,
            kind: SymbolKind::Text,
            scope: if i == 0 {
                SymbolScope::Dynamic
            } else {
                SymbolScope::Compilation
            },
            weak: false,
            section: SymbolSection::Section(text),
            flags: SymbolFlags::None,
        });
    }

    // kfunc calls: R_BPF_64_32 against an undefined global symbol
    let mut kfunc_syms: HashMap<&str, SymbolId> = HashMap::new();
    for k in &prog.kfuncs {
        let sym = *kfunc_syms.entry(k.name.as_str()).or_insert_with(|| {
            obj.add_symbol(Symbol {
                name: k.name.as_bytes().to_vec(),
                value: 0,
                size: 0,
                kind: SymbolKind::Text,
                scope: SymbolScope::Dynamic,
                weak: false,
                section: SymbolSection::Undefined,
                flags: SymbolFlags::None,
            })
        });
        obj.add_relocation(
            text,
            Relocation {
                offset: k.slot as u64 * 8,
                symbol: sym,
                addend: 0,
                flags: RelocationFlags::Elf {
                    r_type: object::elf::R_BPF_64_32,
                },
            },
        )
        .map_err(|e| ElfError::Write(e.to_string()))?;
    }
    for &(at, map) in &prog.map_relocs {
        let sym = *map_syms
            .get(map)
            .ok_or_else(|| ElfError::Write(format!("relocation to missing map {map}")))?;
        obj.add_relocation(
            text,
            Relocation {
                offset: at as u64 * 8,
                symbol: sym,
                addend: 0,
                flags: RelocationFlags::Elf {
                    r_type: object::elf::R_BPF_64_64,
                },
            },
        )
        .map_err(|e| ElfError::Write(e.to_string()))?;
    }
    obj.write().map_err(|e| ElfError::Write(e.to_string()))
}

/// Read a BPF object written by [`write_object`] (or by clang for the same
/// subset) back into a [`Program`]: the `xdp` section, its map relocations
/// resolved against the BTF `.maps` definitions, and the license.
pub fn read_object(bytes: &[u8]) -> Result<Program, ElfError> {
    let file = object::File::parse(bytes).map_err(|e| ElfError::Read(e.to_string()))?;
    if file.architecture() != Architecture::Bpf {
        return Err(ElfError::Malformed("not a BPF object".into()));
    }
    let sec = |n: &str| file.section_by_name(n);
    let mut found = None;
    for s in file.sections() {
        if s.kind() != SectionKind::Text {
            continue;
        }
        let n = s.name().map_err(|e| ElfError::Read(e.to_string()))?;
        if let Some(t) = parse_section(n) {
            if found.is_some() {
                return Err(ElfError::Malformed(
                    "more than one program section".into(),
                ));
            }
            found = Some((s, t));
        }
    }
    let (text, (prog_type, attach)) =
        found.ok_or_else(|| ElfError::Malformed("no program section".into()))?;
    let code = text.data().map_err(|e| ElfError::Read(e.to_string()))?;
    let mut insns = slots_from_bytes(code).map_err(|e| ElfError::Malformed(e.to_string()))?;
    let license = match sec("license") {
        Some(s) => {
            let d = s.data().map_err(|e| ElfError::Read(e.to_string()))?;
            let end = d.iter().position(|&c| c == 0).unwrap_or(d.len());
            String::from_utf8(d[..end].to_vec())
                .map_err(|_| ElfError::Malformed("license is not UTF-8".into()))?
        }
        None => return Err(ElfError::Malformed("no license section".into())),
    };
    let mut maps: Vec<MapDef> = Vec::new();
    let mut map_at: Vec<u64> = Vec::new();
    let maps_index = sec(".maps").map(|s| s.index());
    let text_name = text
        .name()
        .map_err(|e| ElfError::Read(e.to_string()))?
        .to_owned();
    let mut ext = crate::btf_ext::ObjectExt::default();
    let malformed = |e: crate::btf::BtfError| ElfError::Malformed(e.to_string());
    match (sec(".BTF"), sec(".BTF.ext")) {
        (Some(b), Some(x)) => {
            let d = b.data().map_err(|e| ElfError::Read(e.to_string()))?;
            let btf = crate::btf::Btf::parse(d).map_err(malformed)?;
            for (off, def) in crate::btf_ext::maps_from_btf(&btf).map_err(malformed)? {
                map_at.push(off);
                maps.push(def);
            }
            let xd = x.data().map_err(|e| ElfError::Read(e.to_string()))?;
            ext = crate::btf_ext::parse_ext(&btf, xd, &text_name).map_err(malformed)?;
        }
        (Some(b), None) => {
            let d = b.data().map_err(|e| ElfError::Read(e.to_string()))?;
            for (off, def) in parse_maps_btf(d)? {
                map_at.push(off);
                maps.push(def);
            }
        }
        (None, Some(_)) => return Err(ElfError::Malformed(".BTF.ext without .BTF".into())),
        (None, None) => {}
    }
    let mut kfuncs = Vec::new();
    let mut map_relocs = Vec::new();
    for (off, rel) in text.relocations() {
        if rel.flags()
            == (RelocationFlags::Elf {
                r_type: object::elf::R_BPF_64_32,
            })
        {
            // a kfunc call: an undefined symbol named after the function
            let object::RelocationTarget::Symbol(si) = rel.target() else {
                return Err(ElfError::Malformed("relocation without symbol".into()));
            };
            let sym = file
                .symbol_by_index(si)
                .map_err(|e| ElfError::Read(e.to_string()))?;
            if !sym.is_undefined() || off % 8 != 0 {
                return Err(ElfError::Malformed(format!(
                    "unsupported call relocation at {off}"
                )));
            }
            let slot = (off / 8) as usize;
            let s = insns
                .get_mut(slot)
                .ok_or_else(|| ElfError::Malformed("relocation past the code".into()))?;
            if s.code != 0x85 {
                return Err(ElfError::Malformed("kfunc relocation on a non-call".into()));
            }
            s.src = pseudo::KFUNC_CALL;
            s.imm = 0;
            kfuncs.push(crate::program::KfuncRef {
                slot,
                name: sym
                    .name()
                    .map_err(|e| ElfError::Read(e.to_string()))?
                    .to_owned(),
            });
            continue;
        }
        if rel.flags()
            != (RelocationFlags::Elf {
                r_type: object::elf::R_BPF_64_64,
            })
        {
            return Err(ElfError::Malformed(format!(
                "unsupported relocation at {off}"
            )));
        }
        let object::RelocationTarget::Symbol(si) = rel.target() else {
            return Err(ElfError::Malformed("relocation without symbol".into()));
        };
        let sym = file
            .symbol_by_index(si)
            .map_err(|e| ElfError::Read(e.to_string()))?;
        if sym.section_index() != maps_index || maps_index.is_none() {
            return Err(ElfError::Malformed("relocation to a non-map symbol".into()));
        }
        let m = map_at
            .iter()
            .position(|&a| a == sym.address())
            .ok_or_else(|| ElfError::Malformed("relocation to unknown map".into()))?;
        if off % 8 != 0 {
            return Err(ElfError::Malformed("misaligned relocation".into()));
        }
        let slot = (off / 8) as usize;
        let s = insns
            .get_mut(slot)
            .ok_or_else(|| ElfError::Malformed("relocation past the code".into()))?;
        if s.code != 0x18 {
            return Err(ElfError::Malformed(
                "map relocation on a non-ld_imm64".into(),
            ));
        }
        s.src = pseudo::MAP_FD;
        s.imm = m as i32;
        map_relocs.push((slot, m));
    }
    map_relocs.sort_unstable();
    let mut name = String::new();
    let mut functions = Vec::new();
    for s in file.symbols() {
        if s.section_index() == Some(text.index()) && s.kind() == SymbolKind::Text {
            let n = s
                .name()
                .map_err(|e| ElfError::Read(e.to_string()))?
                .to_owned();
            if s.address() == 0 {
                name = n;
            } else {
                functions.push((n, (s.address() / 8) as usize));
            }
        }
    }
    functions.sort_by_key(|(_, e)| *e);
    kfuncs.sort_by_key(|k| k.slot);
    Ok(Program {
        name,
        prog_type,
        insns,
        maps,
        map_relocs,
        functions,
        license,
        attach,
        kfuncs,
        core_relos: ext.core_relos,
        lines: ext.lines,
    })
}
