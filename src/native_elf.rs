//! ELF serializer swap for Linux x86-64 code and data.
use super::machine::{DATA_NAMESPACE, FUNCTION_NAMESPACE};
use cranelift_codegen::{
    Context,
    binemit::Reloc,
    control::ControlPlane,
    ir,
    isa::{OwnedTargetIsa, TargetIsa},
};
use cranelift_module::{
    DataDescription, DataId, FuncId, Init, Linkage, Module, ModuleDeclarations, ModuleReloc,
    ModuleRelocTarget, ModuleResult,
};
use cranelift_object::{ObjectBuilder, ObjectModule as CraneliftObject, ObjectProduct};
use object::{
    FileFlags, SymbolFlags, SymbolKind, SymbolScope,
    write::{SectionId, SymbolSection},
};
use std::{collections::HashMap, sync::OnceLock};

type Encoder = fn(&[u64], &[u8]) -> Option<Vec<u8>>;
static ENCODER: OnceLock<Encoder> = OnceLock::new();

const MODULE_NAME: &str = "vesper";

/// Installs the opt-in ELF encoder. Declined objects use the existing writer.
pub fn install_object_encoder(encode: Encoder) -> Result<(), Encoder> {
    ENCODER.set(encode)
}

pub(super) fn enabled(triple: &target_lexicon::Triple) -> bool {
    ENCODER.get().is_some()
        && triple.architecture == target_lexicon::Architecture::X86_64
        && triple.operating_system == target_lexicon::OperatingSystem::Linux
        && triple.binary_format == target_lexicon::BinaryFormat::Elf
}

// The request tags are shared with modules/native_backend/src/elf.ves.
#[repr(u64)]
enum SymbolType {
    Local = 0,
    Export = 1,
    Undefined = 2,
    Hidden = 3,
    File = 4,
}

#[repr(usize)]
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Text = 0,
    ReadOnly = 1,
    Relro = 2,
    Data = 3,
    Bss = 4,
}

impl Section {
    const ALL: [Self; 5] = [
        Self::Text,
        Self::ReadOnly,
        Self::Relro,
        Self::Data,
        Self::Bss,
    ];

    fn name(self) -> &'static str {
        match self {
            Self::Text => ".text",
            Self::ReadOnly => ".rodata",
            Self::Relro => ".data.rel.ro",
            Self::Data => ".data",
            Self::Bss => ".bss",
        }
    }
}

#[repr(u64)]
#[derive(Clone, Copy)]
enum RelocationType {
    Absolute64 = 1,
    Relative32 = 2,
    PltRelative32 = 3,
    GotRelative32 = 4,
}

impl RelocationType {
    fn field_bytes(&self) -> u64 {
        match self {
            Self::Absolute64 => size_of::<u64>() as u64,
            Self::Relative32 | Self::PltRelative32 | Self::GotRelative32 => size_of::<u32>() as u64,
        }
    }
}

const SYMBOL_COUNT_WORD: usize = 0;
const NO_SECTION: u64 = u64::MAX;

#[derive(Clone, Copy)]
enum Target {
    Function(FuncId),
    Data(DataId),
    LibCall(ir::LibCall),
}

struct Relocation {
    from: Target,
    target: Target,
    offset: u32,
    kind: RelocationType,
    addend: i64,
}

struct ElfObject {
    alignments: [u64; Section::ALL.len()],
    relocations: Vec<Relocation>,
}

impl Target {
    fn symbol(self, product: &ObjectProduct) -> Option<(object::write::SymbolId, bool)> {
        match self {
            Self::Function(id) => product.functions[id],
            Self::Data(id) => product.data_objects[id],
            Self::LibCall(_) => None,
        }
    }

    fn from_relocation(name: ModuleRelocTarget) -> Option<Self> {
        match name {
            ModuleRelocTarget::User {
                namespace: FUNCTION_NAMESPACE,
                index,
            } => Some(Self::Function(FuncId::from_u32(index))),
            ModuleRelocTarget::User {
                namespace: DATA_NAMESPACE,
                index,
            } => Some(Self::Data(DataId::from_u32(index))),
            ModuleRelocTarget::LibCall(call) => Some(Self::LibCall(call)),
            _ => None,
        }
    }
}

impl ElfObject {
    fn request(&self, product: &ObjectProduct) -> Option<(Vec<u64>, Vec<u8>)> {
        if product.object.flags != FileFlags::None {
            return None;
        }
        let mut ids: [Option<SectionId>; Section::ALL.len()] = [None; Section::ALL.len()];
        let mut sizes = [0; Section::ALL.len()];
        let mut symbol_ids = HashMap::new();
        let targets = product
            .functions
            .iter()
            .filter_map(|(id, symbol)| symbol.map(|_| Target::Function(id)))
            .chain(
                product
                    .data_objects
                    .iter()
                    .filter_map(|(id, symbol)| symbol.map(|_| Target::Data(id))),
            );
        let mut symbols = Vec::new();
        for target in targets {
            let (id, defined) = target.symbol(product)?;
            let symbol = product.object.symbol(id);
            let text = matches!(target, Target::Function(_));
            let expected_kind = if text {
                SymbolKind::Text
            } else {
                SymbolKind::Data
            };
            if symbol.kind != expected_kind || symbol.weak || symbol.flags != SymbolFlags::None {
                return None;
            }
            let (kind, section) = match (defined, symbol.section, symbol.scope) {
                (false, SymbolSection::Undefined, SymbolScope::Unknown) => {
                    (SymbolType::Undefined, NO_SECTION)
                }
                (true, SymbolSection::Section(id), scope) => {
                    let name = product.object.section(id).name()?;
                    let section = Section::ALL
                        .into_iter()
                        .find(|section| section.name() == name)?;
                    let index = section as usize;
                    if text != (section == Section::Text)
                        || ids[index].is_some_and(|current| current != id)
                        || self.alignments[index] == 0
                    {
                        return None;
                    }
                    ids[index] = Some(id);
                    sizes[index] = sizes[index].max(symbol.value.checked_add(symbol.size)?);
                    let kind = match scope {
                        SymbolScope::Compilation => SymbolType::Local,
                        SymbolScope::Dynamic => SymbolType::Export,
                        SymbolScope::Linkage => SymbolType::Hidden,
                        _ => return None,
                    };
                    (kind, index as u64)
                }
                _ => return None,
            };
            // The first wire symbol is the compilation-unit name.
            symbol_ids.insert(id, symbols.len() as u64 + 1);
            symbols.push((symbol, kind, section));
        }
        let mut libcalls = HashMap::new();
        let libcall_name = cranelift_module::default_libcall_names();
        for reloc in &self.relocations {
            if let Target::LibCall(call) = reloc.target
                && let std::collections::hash_map::Entry::Vacant(entry) = libcalls.entry(call)
            {
                let name = libcall_name(call);
                let id = product.object.symbol_id(name.as_bytes())?;
                let index = if let Some(&index) = symbol_ids.get(&id) {
                    index
                } else {
                    let symbol = product.object.symbol(id);
                    if symbol.kind != SymbolKind::Text
                        || symbol.scope != SymbolScope::Unknown
                        || symbol.section != SymbolSection::Undefined
                        || symbol.weak
                        || symbol.flags != SymbolFlags::None
                    {
                        return None;
                    }
                    let index = symbols.len() as u64 + 1;
                    symbols.push((symbol, SymbolType::Undefined, NO_SECTION));
                    symbol_ids.insert(id, index);
                    index
                };
                entry.insert(index);
            }
        }

        let mut words = vec![0];
        let mut bytes = Vec::new();
        for section in Section::ALL {
            let index = section as usize;
            let Some(id) = ids[index] else {
                words.extend([0, if section == Section::Text { 1 } else { 0 }]);
                continue;
            };
            if section != Section::Bss {
                let data = product.object.section(id).data();
                sizes[index] = data.len() as u64;
                bytes.extend_from_slice(data);
            }
            words.extend([sizes[index], self.alignments[index]]);
        }
        append_symbol(
            &mut words,
            &mut bytes,
            MODULE_NAME.as_bytes(),
            0,
            0,
            SymbolType::File,
            NO_SECTION,
        );
        for (symbol, kind, section) in symbols {
            append_symbol(
                &mut words,
                &mut bytes,
                &symbol.name,
                symbol.value,
                symbol.size,
                kind,
                section,
            );
        }
        for reloc in &self.relocations {
            let (source, true) = reloc.from.symbol(product)? else {
                return None;
            };
            let source = product.object.symbol(source);
            let section = source.section.id()?;
            let section = ids.iter().position(|&id| id == Some(section))?;
            let offset = u64::from(reloc.offset);
            if section == Section::Bss as usize
                || offset.checked_add(reloc.kind.field_bytes())? > source.size
            {
                return None;
            }
            let at = source.value.checked_add(offset)?;
            let target = match reloc.target {
                Target::LibCall(call) => *libcalls.get(&call)?,
                target => *symbol_ids.get(&target.symbol(product)?.0)?,
            };
            words.extend([
                section as u64,
                at,
                target,
                reloc.kind as u64,
                reloc.addend as u64,
            ]);
        }
        Some((words, bytes))
    }

    fn record(
        &mut self,
        from: Target,
        relocs: impl IntoIterator<Item = ModuleReloc>,
    ) -> Option<()> {
        for reloc in relocs {
            let target = Target::from_relocation(reloc.name)?;
            let kind = match reloc.kind {
                Reloc::Abs8 => RelocationType::Absolute64,
                Reloc::X86PCRel4 => RelocationType::Relative32,
                Reloc::X86CallPCRel4 | Reloc::X86CallPLTRel4 => RelocationType::PltRelative32,
                Reloc::X86GOTPCRel4 => RelocationType::GotRelative32,
                _ => return None,
            };
            self.relocations.push(Relocation {
                from,
                target,
                offset: reloc.offset,
                kind,
                addend: reloc.addend,
            });
        }
        Some(())
    }
}

fn append_symbol(
    words: &mut Vec<u64>,
    bytes: &mut Vec<u8>,
    name: &[u8],
    value: u64,
    size: u64,
    kind: SymbolType,
    section: u64,
) {
    words.extend([
        bytes.len() as u64,
        name.len() as u64,
        value,
        size,
        kind as u64,
        section,
    ]);
    words[SYMBOL_COUNT_WORD] += 1;
    bytes.extend_from_slice(name);
}

/// Retains the metadata the Vesper serializer needs alongside the regular
/// module. The regular artifact is available intact if the writer declines.
pub(super) struct ObjectModule {
    inner: CraneliftObject,
    capture: Option<ElfObject>,
}

impl ObjectModule {
    pub(super) fn new(isa: OwnedTargetIsa, enabled: bool) -> Result<Self, String> {
        let builder =
            ObjectBuilder::new(isa, MODULE_NAME, cranelift_module::default_libcall_names())
                .map_err(|error| error.to_string())?;
        Ok(Self {
            inner: CraneliftObject::new(builder),
            capture: enabled.then(|| ElfObject {
                alignments: [0; Section::ALL.len()],
                relocations: Vec::new(),
            }),
        })
    }

    fn record(
        &mut self,
        from: FuncId,
        alignment: u64,
        relocs: impl IntoIterator<Item = ModuleReloc>,
    ) {
        if let Some(capture) = &mut self.capture {
            let text = &mut capture.alignments[Section::Text as usize];
            *text = (*text)
                .max(alignment)
                .max(self.inner.isa().symbol_alignment());
            if capture.record(Target::Function(from), relocs).is_none() {
                self.capture = None;
            }
        }
    }

    pub(super) fn emit(self) -> Result<Vec<u8>, object::write::Error> {
        let product = self.inner.finish();
        if let (Some(capture), Some(encode)) = (self.capture, ENCODER.get())
            && let Some((words, bytes)) = capture.request(&product)
            && let Some(object) = encode(&words, &bytes)
        {
            return Ok(object);
        }
        product.emit()
    }
}

impl Module for ObjectModule {
    fn isa(&self) -> &dyn TargetIsa {
        self.inner.isa()
    }

    fn declarations(&self) -> &ModuleDeclarations {
        self.inner.declarations()
    }

    fn declare_function(
        &mut self,
        name: &str,
        linkage: Linkage,
        signature: &ir::Signature,
    ) -> ModuleResult<FuncId> {
        self.inner.declare_function(name, linkage, signature)
    }

    fn declare_anonymous_function(&mut self, signature: &ir::Signature) -> ModuleResult<FuncId> {
        self.inner.declare_anonymous_function(signature)
    }

    fn declare_data(
        &mut self,
        name: &str,
        linkage: Linkage,
        writable: bool,
        tls: bool,
    ) -> ModuleResult<DataId> {
        if tls {
            self.capture = None;
        }
        self.inner.declare_data(name, linkage, writable, tls)
    }

    fn declare_anonymous_data(&mut self, writable: bool, tls: bool) -> ModuleResult<DataId> {
        if tls {
            self.capture = None;
        }
        self.inner.declare_anonymous_data(writable, tls)
    }

    fn define_data(&mut self, id: DataId, data: &DataDescription) -> ModuleResult<()> {
        self.inner.define_data(id, data)?;
        if let Some(capture) = &mut self.capture {
            let declaration = self.inner.declarations().get_data_decl(id);
            let relocs = !data.function_relocs.is_empty() || !data.data_relocs.is_empty();
            if data.custom_segment_section.is_some()
                || data.used
                || (relocs && matches!(data.init, Init::Zeros { .. }))
            {
                self.capture = None;
                return Ok(());
            }
            let section = match data.init {
                Init::Zeros { .. } => Section::Bss,
                _ if declaration.writable => Section::Data,
                _ if relocs => Section::Relro,
                _ => Section::ReadOnly,
            };
            let alignment = &mut capture.alignments[section as usize];
            *alignment = (*alignment)
                .max(data.align.unwrap_or(1))
                .max(self.inner.isa().symbol_alignment());
            if capture
                .record(Target::Data(id), data.all_relocs(Reloc::Abs8))
                .is_none()
            {
                self.capture = None;
            }
        }
        Ok(())
    }

    fn define_function_with_control_plane(
        &mut self,
        id: FuncId,
        context: &mut Context,
        ctrl_plane: &mut ControlPlane,
    ) -> ModuleResult<()> {
        self.inner
            .define_function_with_control_plane(id, context, ctrl_plane)?;
        if self.capture.is_some() {
            let buffer = &context
                .compiled_code()
                .expect("function was compiled")
                .buffer;
            let relocations = buffer
                .relocs()
                .iter()
                .map(|r| ModuleReloc::from_mach_reloc(r, &context.func, id));
            self.record(id, u64::from(buffer.alignment), relocations);
        }
        Ok(())
    }

    fn define_function_bytes(
        &mut self,
        id: FuncId,
        alignment: u64,
        bytes: &[u8],
        relocations: &[ModuleReloc],
    ) -> ModuleResult<()> {
        self.inner
            .define_function_bytes(id, alignment, bytes, relocations)?;
        self.record(id, alignment, relocations.iter().cloned());
        Ok(())
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use cranelift_codegen::settings;

    #[test]
    fn thread_local_data_uses_the_original_artifact() {
        let isa = cranelift_native::builder()
            .unwrap()
            .finish(settings::Flags::new(settings::builder()))
            .unwrap();
        let object = |capture| {
            let mut module = ObjectModule::new(isa.clone(), capture).unwrap();
            let signature = module.make_signature();
            let entry = module
                .declare_function("sh7_entry", Linkage::Export, &signature)
                .unwrap();
            module
                .define_function_bytes(entry, 1, &[0xc3], &[])
                .unwrap();
            let data = module
                .declare_data("sh7_tls", Linkage::Local, true, true)
                .unwrap();
            let mut description = DataDescription::new();
            description.define(vec![31].into_boxed_slice());
            module.define_data(data, &description).unwrap();
            assert!(module.capture.is_none());
            module.emit().unwrap()
        };
        assert_eq!(object(true), object(false));
    }
}
