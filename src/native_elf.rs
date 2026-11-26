//! ELF serializer swap for Linux x86-64 code and read-only data.
use cranelift_codegen::{
    Context, binemit::Reloc, control::ControlPlane, entity::SecondaryMap, ir, isa::TargetIsa,
};
use cranelift_module::{
    DataDescription, DataId, FuncId, Init, Linkage, Module, ModuleDeclarations, ModuleReloc,
    ModuleRelocTarget, ModuleResult,
};
use cranelift_object::{ObjectBuilder, ObjectModule as CraneliftObject, ObjectProduct};
use object::{FileFlags, SymbolFlags, SymbolKind, SymbolScope, write::SymbolSection};
use std::sync::OnceLock;

type Encoder = fn(&[u64], &[u8]) -> Option<Vec<u8>>;
static ENCODER: OnceLock<Encoder> = OnceLock::new();

pub(super) const MODULE_NAME: &str = "vesper";

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
    ReadOnlyLocal = 5,
    ReadOnlyExport = 6,
    ReadOnlyHidden = 7,
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

const SYMBOL_COUNT_WORD: usize = 2;
const FUNCTION_NAMESPACE: u32 = 0;
const DATA_NAMESPACE: u32 = 1;

enum Target {
    Function(FuncId),
    Data(DataId),
}

struct TextRelocation {
    from: FuncId,
    target: Target,
    offset: u32,
    kind: RelocationType,
    addend: i64,
}

struct ElfObject {
    text_alignment: u64,
    rodata_alignment: u64,
    relocations: Vec<TextRelocation>,
}

impl ElfObject {
    fn request(&self, product: &ObjectProduct) -> Option<(Vec<u64>, Vec<u8>)> {
        if product.object.flags != FileFlags::None {
            return None;
        }
        let section_id = product.functions.values().find_map(|entry| {
            let (symbol, defined) = (*entry)?;
            if !defined {
                return None;
            }
            product.object.symbol(symbol).section.id()
        })?;
        let section = product.object.section(section_id);
        if section.name() != Some(".text") {
            return None;
        }
        let text = section.data();
        let rodata_id = product.data_objects.values().find_map(|entry| {
            let (symbol, defined) = (*entry)?;
            if !defined {
                return None;
            }
            product.object.symbol(symbol).section.id()
        });
        let rodata = if let Some(id) = rodata_id {
            let section = product.object.section(id);
            if section.name() != Some(".rodata") || self.rodata_alignment == 0 {
                return None;
            }
            section.data()
        } else {
            &[]
        };
        let mut bytes = Vec::with_capacity(text.len() + rodata.len());
        bytes.extend_from_slice(text);
        bytes.extend_from_slice(rodata);
        let mut words = vec![
            text.len() as u64,
            self.text_alignment,
            0,
            rodata.len() as u64,
            self.rodata_alignment,
        ];
        let mut functions = SecondaryMap::<FuncId, Option<u64>>::new();
        let mut data = SecondaryMap::<DataId, Option<u64>>::new();
        append_symbol(
            &mut words,
            &mut bytes,
            MODULE_NAME.as_bytes(),
            0,
            0,
            SymbolType::File,
        );
        for (id, entry) in product.functions.iter() {
            let Some((symbol, defined)) = *entry else {
                continue;
            };
            let symbol = product.object.symbol(symbol);
            if symbol.kind != SymbolKind::Text || symbol.weak || symbol.flags != SymbolFlags::None {
                return None;
            }
            let kind = match (defined, symbol.section, symbol.scope) {
                (false, SymbolSection::Undefined, SymbolScope::Unknown) => SymbolType::Undefined,
                (true, SymbolSection::Section(section), scope) if section == section_id => {
                    match scope {
                        SymbolScope::Compilation => SymbolType::Local,
                        SymbolScope::Dynamic => SymbolType::Export,
                        SymbolScope::Linkage => SymbolType::Hidden,
                        _ => return None,
                    }
                }
                _ => return None,
            };
            functions[id] = Some(words[SYMBOL_COUNT_WORD]);
            append_symbol(
                &mut words,
                &mut bytes,
                &symbol.name,
                symbol.value,
                symbol.size,
                kind,
            );
        }
        for (id, entry) in product.data_objects.iter() {
            let Some((symbol, true)) = *entry else {
                return None;
            };
            let symbol = product.object.symbol(symbol);
            if rodata_id.is_none()
                || symbol.section.id() != rodata_id
                || symbol.kind != SymbolKind::Data
                || symbol.weak
                || symbol.flags != SymbolFlags::None
            {
                return None;
            }
            let kind = match symbol.scope {
                SymbolScope::Compilation => SymbolType::ReadOnlyLocal,
                SymbolScope::Dynamic => SymbolType::ReadOnlyExport,
                SymbolScope::Linkage => SymbolType::ReadOnlyHidden,
                _ => return None,
            };
            data[id] = Some(words[SYMBOL_COUNT_WORD]);
            append_symbol(
                &mut words,
                &mut bytes,
                &symbol.name,
                symbol.value,
                symbol.size,
                kind,
            );
        }
        for reloc in &self.relocations {
            let (source, true) = product.functions[reloc.from]? else {
                return None;
            };
            let source = product.object.symbol(source);
            let offset = u64::from(reloc.offset);
            if offset.checked_add(reloc.kind.field_bytes())? > source.size {
                return None;
            }
            let at = source.value.checked_add(offset)?;
            let target = match reloc.target {
                Target::Function(id) => functions[id]?,
                Target::Data(id) => data[id]?,
            };
            words.extend([at, target, reloc.kind as u64, reloc.addend as u64]);
        }
        Some((words, bytes))
    }
}

fn append_symbol(
    words: &mut Vec<u64>,
    bytes: &mut Vec<u8>,
    name: &[u8],
    value: u64,
    size: u64,
    kind: SymbolType,
) {
    words.extend([
        bytes.len() as u64,
        name.len() as u64,
        value,
        size,
        kind as u64,
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
    pub(super) fn new(builder: ObjectBuilder, enabled: bool) -> Self {
        Self {
            inner: CraneliftObject::new(builder),
            capture: enabled.then(|| ElfObject {
                text_alignment: 1,
                rodata_alignment: 0,
                relocations: Vec::new(),
            }),
        }
    }

    fn record(
        &mut self,
        from: FuncId,
        alignment: u64,
        relocs: impl IntoIterator<Item = ModuleReloc>,
    ) {
        if let Some(capture) = &mut self.capture {
            capture.text_alignment = capture
                .text_alignment
                .max(alignment)
                .max(self.inner.isa().symbol_alignment());
            for reloc in relocs {
                let target = match reloc.name {
                    ModuleRelocTarget::User {
                        namespace: FUNCTION_NAMESPACE,
                        index,
                    } => Target::Function(FuncId::from_u32(index)),
                    ModuleRelocTarget::User {
                        namespace: DATA_NAMESPACE,
                        index,
                    } => Target::Data(DataId::from_u32(index)),
                    _ => {
                        self.capture = None;
                        return;
                    }
                };
                let kind = match reloc.kind {
                    Reloc::Abs8 => RelocationType::Absolute64,
                    Reloc::X86PCRel4 => RelocationType::Relative32,
                    Reloc::X86CallPCRel4 | Reloc::X86CallPLTRel4 => RelocationType::PltRelative32,
                    Reloc::X86GOTPCRel4 => RelocationType::GotRelative32,
                    _ => {
                        self.capture = None;
                        return;
                    }
                };
                capture.relocations.push(TextRelocation {
                    from,
                    target,
                    offset: reloc.offset,
                    kind,
                    addend: reloc.addend,
                });
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
        if writable || tls {
            self.capture = None;
        }
        self.inner.declare_data(name, linkage, writable, tls)
    }

    fn declare_anonymous_data(&mut self, writable: bool, tls: bool) -> ModuleResult<DataId> {
        if writable || tls {
            self.capture = None;
        }
        self.inner.declare_anonymous_data(writable, tls)
    }

    fn define_data(&mut self, id: DataId, data: &DataDescription) -> ModuleResult<()> {
        self.inner.define_data(id, data)?;
        if let Some(capture) = &mut self.capture {
            if !matches!(data.init, Init::Bytes { .. })
                || !data.function_relocs.is_empty()
                || !data.data_relocs.is_empty()
                || data.custom_segment_section.is_some()
                || data.used
            {
                self.capture = None;
            } else {
                capture.rodata_alignment = capture
                    .rodata_alignment
                    .max(data.align.unwrap_or(1))
                    .max(self.inner.isa().symbol_alignment());
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
    fn writable_data_uses_the_original_artifact() {
        let isa = cranelift_native::builder()
            .unwrap()
            .finish(settings::Flags::new(settings::builder()))
            .unwrap();
        let object = |capture| {
            let builder = ObjectBuilder::new(
                isa.clone(),
                MODULE_NAME,
                cranelift_module::default_libcall_names(),
            )
            .unwrap();
            let mut module = ObjectModule::new(builder, capture);
            let signature = module.make_signature();
            let entry = module
                .declare_function("sh7_entry", Linkage::Export, &signature)
                .unwrap();
            module
                .define_function_bytes(entry, 1, &[0xc3], &[])
                .unwrap();
            let data = module
                .declare_data("sh7_writable", Linkage::Local, true, false)
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
