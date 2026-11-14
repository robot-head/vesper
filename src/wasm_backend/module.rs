//! Core module assembly: type, function, table, memory, tag, global, export,
//! element and code sections in the order the binary format requires.
use std::{borrow::Cow, collections::BTreeMap};
use wasm_encoder::{
    CodeSection, ConstExpr, ContType, ElementSection, Elements, ExportKind, ExportSection,
    Function, FunctionSection, GlobalSection, GlobalType, MemoryType, Module, RefType,
    TableSection, TableType, TagKind, TagSection, TagType, TypeSection, ValType,
};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum TypeDef {
    Func(Vec<ValType>, Vec<ValType>),
    Cont(u32),
}

struct FuncDef {
    ty: u32,
    body: Option<Function>,
}

/// An append-only builder: every index it returns stays valid.
#[derive(Default)]
pub(crate) struct Builder {
    types: Vec<TypeDef>,
    type_index: BTreeMap<TypeDef, u32>,
    funcs: Vec<FuncDef>,
    pub(crate) func_names: Vec<String>,
    globals: Vec<i64>,
    table: Vec<u32>,
    declared: Vec<u32>,
    tags: Vec<u32>,
    exports: Vec<(String, ExportKind, u32)>,
}

impl Builder {
    fn intern(&mut self, def: TypeDef) -> u32 {
        if let Some(i) = self.type_index.get(&def) {
            return *i;
        }
        let i = self.types.len() as u32;
        self.types.push(def.clone());
        self.type_index.insert(def, i);
        i
    }
    pub(crate) fn func_type(&mut self, params: &[ValType], results: &[ValType]) -> u32 {
        self.intern(TypeDef::Func(params.to_vec(), results.to_vec()))
    }
    pub(crate) fn cont_type(&mut self, func: u32) -> u32 {
        self.intern(TypeDef::Cont(func))
    }
    pub(crate) fn declare(&mut self, name: &str, params: &[ValType], results: &[ValType]) -> u32 {
        let ty = self.func_type(params, results);
        self.declare_typed(name, ty)
    }
    pub(crate) fn declare_typed(&mut self, name: &str, ty: u32) -> u32 {
        self.funcs.push(FuncDef { ty, body: None });
        self.func_names.push(name.to_owned());
        (self.funcs.len() - 1) as u32
    }
    pub(crate) fn define(&mut self, index: u32, body: Function) {
        self.funcs[index as usize].body = Some(body);
    }
    pub(crate) fn function_count(&self) -> usize {
        self.funcs.len()
    }
    /// A mutable `i64` global with the given initial value.
    pub(crate) fn global(&mut self, init: i64) -> u32 {
        self.globals.push(init);
        (self.globals.len() - 1) as u32
    }
    /// Place `func` in the function table and return its slot.
    pub(crate) fn table_slot(&mut self, func: u32) -> u32 {
        self.table.push(func);
        (self.table.len() - 1) as u32
    }
    /// Declare `func` as a `ref.func` target.
    pub(crate) fn declare_ref(&mut self, func: u32) {
        if !self.declared.contains(&func) {
            self.declared.push(func);
        }
    }
    pub(crate) fn tag(&mut self, func_type: u32) -> u32 {
        self.tags.push(func_type);
        (self.tags.len() - 1) as u32
    }
    pub(crate) fn export_func(&mut self, name: &str, func: u32) {
        self.exports.push((name.to_owned(), ExportKind::Func, func));
    }

    pub(crate) fn finish(self, max_pages: u32) -> Result<Vec<u8>, String> {
        let mut module = Module::new();
        let mut types = TypeSection::new();
        for def in &self.types {
            match def {
                TypeDef::Func(params, results) => {
                    types
                        .ty()
                        .function(params.iter().copied(), results.iter().copied());
                }
                TypeDef::Cont(func) => {
                    types.ty().cont(&ContType(*func));
                }
            }
        }
        module.section(&types);
        let mut functions = FunctionSection::new();
        for f in &self.funcs {
            functions.function(f.ty);
        }
        module.section(&functions);
        let mut tables = TableSection::new();
        tables.table(TableType {
            element_type: RefType::FUNCREF,
            table64: false,
            minimum: self.table.len() as u64,
            maximum: Some(self.table.len() as u64),
            shared: false,
        });
        module.section(&tables);
        let mut memories = wasm_encoder::MemorySection::new();
        memories.memory(MemoryType {
            minimum: 1,
            maximum: Some(u64::from(max_pages)),
            memory64: false,
            shared: false,
            page_size_log2: None,
        });
        module.section(&memories);
        if !self.tags.is_empty() {
            let mut tags = TagSection::new();
            for t in &self.tags {
                tags.tag(TagType {
                    kind: TagKind::Exception,
                    func_type_idx: *t,
                });
            }
            module.section(&tags);
        }
        let mut globals = GlobalSection::new();
        for init in &self.globals {
            globals.global(
                GlobalType {
                    val_type: ValType::I64,
                    mutable: true,
                    shared: false,
                },
                &ConstExpr::i64_const(*init),
            );
        }
        module.section(&globals);
        let mut exports = ExportSection::new();
        exports.export("memory", ExportKind::Memory, 0);
        for (name, kind, index) in &self.exports {
            exports.export(name, *kind, *index);
        }
        module.section(&exports);
        let mut elements = ElementSection::new();
        if !self.table.is_empty() {
            elements.active(
                Some(0),
                &ConstExpr::i32_const(0),
                Elements::Functions(Cow::Borrowed(&self.table)),
            );
        }
        if !self.declared.is_empty() {
            elements.declared(Elements::Functions(Cow::Borrowed(&self.declared)));
        }
        if !elements.is_empty() {
            module.section(&elements);
        }
        let mut code = CodeSection::new();
        for (i, f) in self.funcs.iter().enumerate() {
            let body = f.body.as_ref().ok_or_else(|| {
                format!(
                    "function {} was declared but not defined",
                    self.func_names[i]
                )
            })?;
            code.function(body);
        }
        module.section(&code);
        Ok(module.finish())
    }
}
