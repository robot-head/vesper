//! Freestanding linker/stub/script as bytes, both directions (SH4 lane 14, #1288).
//!
//! Used by `tests/self_host_freestanding.rs` and `vesper-hybrid`'s
//! `hybrid_freestanding` bridge (`codegen_swap::freestanding`).
#![allow(dead_code)]

use vesper::native::freestanding::{
    Board, ExitDevice, FreestandingError, FreestandingTarget, link,
};

/// A request: little-endian words and length-prefixed strings.
#[derive(Default)]
pub struct Request(pub Vec<u8>);

impl Request {
    pub fn word(mut self, w: u64) -> Self {
        self.0.extend_from_slice(&w.to_le_bytes());
        self
    }

    pub fn bytes(mut self, b: &[u8]) -> Self {
        self = self.word(b.len() as u64);
        self.0.extend_from_slice(b);
        self
    }
}

/// Reads a result buffer.
pub struct Words<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Words<'a> {
    pub fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }

    pub fn word(&mut self) -> u64 {
        let w = u64::from_le_bytes(self.b[self.at..self.at + 8].try_into().unwrap());
        self.at += 8;
        w
    }

    pub fn bytes(&mut self) -> &'a [u8] {
        let n = self.word() as usize;
        let s = &self.b[self.at..self.at + n];
        self.at += n;
        s
    }

    pub fn string(&mut self) -> String {
        String::from_utf8(self.bytes().to_vec()).unwrap()
    }

    pub fn done(&self) {
        assert_eq!(self.at, self.b.len(), "trailing result bytes");
    }
}

/// `FreestandingError` from a result whose status word is not 0
/// (packages/vesper-codegen-freestanding/src/error.ves).
pub fn decode_error(code: u64, w: &mut Words<'_>) -> FreestandingError {
    match code {
        1 => FreestandingError::UnsupportedTarget(w.string()),
        2 => FreestandingError::UnresolvedSymbol {
            symbol: w.string(),
            object: w.string(),
        },
        3 => FreestandingError::DuplicateSymbol(w.string()),
        4 => FreestandingError::UnsupportedRelocation {
            object: w.string(),
            r_type: w.word() as u32,
        },
        5 => FreestandingError::UnsupportedSection {
            object: w.string(),
            section: w.string(),
        },
        6 => FreestandingError::RelocationOutOfRange {
            object: w.string(),
            symbol: w.string(),
            r_type: w.word() as u32,
        },
        7 => FreestandingError::ImageTooLarge {
            needed: w.word(),
            available: w.word(),
        },
        8 => FreestandingError::Object(w.string()),
        9 => FreestandingError::CodeMismatch {
            function: w.string(),
        },
        other => panic!("unknown error code {other}"),
    }
}

pub fn target_word(t: FreestandingTarget) -> u64 {
    match t {
        FreestandingTarget::Riscv64Imac => 0,
        FreestandingTarget::Aarch64 => 1,
    }
}

pub fn exit_words(e: ExitDevice) -> (u64, u64) {
    match e {
        ExitDevice::SifiveTest { base } => (0, base),
        ExitDevice::Semihosting => (1, 0),
        ExitDevice::Park => (2, 0),
    }
}

pub fn script_request(t: FreestandingTarget, board: &Board, stack_bytes: u32) -> Request {
    Request::default()
        .word(target_word(t))
        .word(board.ram_base)
        .word(board.ram_bytes)
        .word(board.image_base)
        .word(u64::from(stack_bytes))
        .bytes(board.name.as_bytes())
}

pub fn decode_script(out: &[u8]) -> String {
    let mut w = Words::new(out);
    assert_eq!(w.word(), 0);
    let s = w.string();
    w.done();
    s
}

pub fn stub_request(t: FreestandingTarget, board: &Board, e_flags: u32) -> Request {
    let (exit, base) = exit_words(board.exit);
    Request::default()
        .word(target_word(t))
        .word(exit)
        .word(base)
        .word(u64::from(e_flags))
}

pub fn decode_stub(out: &[u8]) -> Result<Vec<u8>, FreestandingError> {
    let mut w = Words::new(out);
    let code = w.word();
    let r = if code == 0 {
        Ok(w.bytes().to_vec())
    } else {
        Err(decode_error(code, &mut w))
    };
    w.done();
    r
}

/// A trap-table entry to resolve: (object, function, offset).
pub type Trap = (String, String, u64);

/// What `freestanding_link` returns.
#[derive(Debug, PartialEq)]
pub struct VesperLinked {
    pub entry: u64,
    pub layout: link::Layout,
    pub symbols: Vec<link::LinkedSymbol>,
    pub pcs: Vec<u64>,
    pub elf: Vec<u8>,
}

pub fn link_request(
    objects: &[(String, Vec<u8>)],
    options: &link::LinkOptions,
    traps: &[Trap],
) -> Request {
    let mut req = Request::default()
        .word(target_word(options.machine))
        .word(options.image_base)
        .word(options.ram_base)
        .word(options.ram_bytes)
        .word(options.stack_bytes)
        .word(objects.len() as u64);
    for (name, bytes) in objects {
        req = req.bytes(name.as_bytes()).bytes(bytes);
    }
    req = req.word(traps.len() as u64);
    for (object, function, offset) in traps {
        req = req
            .bytes(object.as_bytes())
            .bytes(function.as_bytes())
            .word(*offset);
    }
    req
}

pub fn decode_link(out: &[u8]) -> Result<VesperLinked, FreestandingError> {
    let mut w = Words::new(out);
    let code = w.word();
    if code != 0 {
        let e = decode_error(code, &mut w);
        w.done();
        return Err(e);
    }
    let entry = w.word();
    let mut layout = link::Layout::default();
    for name in [".text", ".rodata", ".data", ".bss"] {
        let (a, n) = (w.word(), w.word());
        layout.sections.push((name.to_string(), a, n));
    }
    layout.stack_bottom = w.word();
    layout.stack_top = w.word();
    layout.image_end = w.word();
    for _ in 0..w.word() {
        layout.segments.push(link::Segment {
            address: w.word(),
            file_offset: w.word(),
            file_bytes: w.word(),
            memory_bytes: w.word(),
            flags: w.word() as u32,
        });
    }
    let mut symbols = Vec::new();
    for _ in 0..w.word() {
        symbols.push(link::LinkedSymbol {
            object: w.string(),
            name: w.string(),
            address: w.word(),
            size: w.word(),
            global: w.word() != 0,
        });
    }
    let pcs = (0..w.word()).map(|_| w.word()).collect();
    let elf = w.bytes().to_vec();
    w.done();
    Ok(VesperLinked {
        entry,
        layout,
        symbols,
        pcs,
        elf,
    })
}
