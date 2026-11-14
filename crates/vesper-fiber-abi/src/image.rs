//! Byte images of parked fibers (additive to ABI v2).
//!
//! A parked fiber is fully described by its context, its data frame and the
//! stack bytes between its saved stack pointer and the stack top (the saved
//! switch frame sits at the bottom of those bytes). [`StackImage`] is that
//! byte range, [`RegisterFrame`] decodes the switch frame at its start and
//! [`FiberImage`] adds everything else a restore needs.
//!
//! **Same addresses, same process.** Stack bytes hold absolute addresses:
//! return addresses into generated code, the context pointer, pointers into
//! the stack itself, the evidence vector header and (on Windows) the TEB
//! stack limits. A byte image is therefore only restored onto a stack at the
//! *same* reservation address ([`crate::FiberConfig::fixed_stack_base`]),
//! whose context lives inside that reservation so that it too keeps its
//! address, running the *same* code and evidence vector in the *same*
//! process and thread. The data frame may move: generated code reloads its
//! base from the resume signal at every reload point (RFC §4), exactly as
//! for [`crate::Parked::relocate_data`]. An entry that kept any other
//! address live across a park would break the relocation contract already;
//! byte images rely on the same contract and nothing more.
//!
//! **Checks.** [`crate::Fiber::from_image`] restores onto a *created*
//! template fiber and rejects, with a typed [`ImageError`], any template
//! that differs from the captured fiber in entry address, entry argument,
//! code owner, evidence vector, data-frame size, stack size or reservation
//! address. Code and evidence are compared by `Rc` identity through weak
//! anchors this module keeps per thread, so a freed module whose address is
//! reused by different code is not mistaken for the original.
//!
//! **Encoded images.** [`FiberImage::encode`] produces a versioned byte form
//! (for Chrono bundles and diffs) that ends in a seal: a keyed hash (the
//! standard library's randomly keyed `RandomState` hasher, keyed once per
//! thread) over everything before it. [`FiberImage::decode`] accepts only
//! unmodified images sealed by the same thread of the same process, because
//! only those can be restored safely; the seal is a binding and integrity
//! check against foreign or edited bytes, not a cryptographic MAC against an
//! adversary inside the process.
use crate::abi::{ABI_VERSION, EvidenceId, ParkRequest};
use crate::evidence::EvidenceVector;
use crate::switch::SWITCH_FRAME_BYTES;
use std::any::Any;
use std::cell::{Cell, RefCell};
use std::collections::hash_map::RandomState;
use std::fmt;
use std::hash::{BuildHasher, Hasher};
use std::rc::{Rc, Weak};

/// The stack bytes of a parked fiber: `bytes` is the memory in
/// `[saved_sp, top)`, lowest address first.
#[derive(Clone, PartialEq, Eq)]
pub struct StackImage {
    pub saved_sp: u64,
    pub top: u64,
    pub bytes: Vec<u8>,
}

impl fmt::Debug for StackImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StackImage")
            .field("saved_sp", &format_args!("{:#x}", self.saved_sp))
            .field("top", &format_args!("{:#x}", self.top))
            .field("len", &self.bytes.len())
            .finish()
    }
}

impl StackImage {
    /// The 64-bit little-endian word at absolute address `address`.
    pub fn word_at(&self, address: u64) -> Option<u64> {
        let offset = address.checked_sub(self.saved_sp)? as usize;
        let bytes = self.bytes.get(offset..offset.checked_add(8)?)?;
        Some(u64::from_le_bytes(bytes.try_into().ok()?))
    }
    /// Decode the saved switch frame at the bottom of the image.
    pub fn register_frame(&self) -> Option<RegisterFrame> {
        RegisterFrame::parse(self.bytes.get(..SWITCH_FRAME_BYTES)?)
    }
}

/// Which switch-frame layout a [`RegisterFrame`] was decoded from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FrameAbi {
    /// System V x86_64 (Linux), 64 bytes.
    SysVX86_64,
    /// AAPCS64 / Apple arm64 (Linux and macOS AArch64), 192 bytes.
    Aarch64,
    /// Microsoft x64 (Windows), 288 bytes including TEB stack fields.
    WindowsX64,
}

impl FrameAbi {
    /// The layout this build's switch uses.
    pub const HOST: FrameAbi = host_abi();
    fn tag(self) -> u16 {
        match self {
            Self::SysVX86_64 => 1,
            Self::Aarch64 => 2,
            Self::WindowsX64 => 3,
        }
    }
}

const fn host_abi() -> FrameAbi {
    #[cfg(all(target_arch = "x86_64", not(windows)))]
    {
        FrameAbi::SysVX86_64
    }
    #[cfg(target_arch = "aarch64")]
    {
        FrameAbi::Aarch64
    }
    #[cfg(all(target_arch = "x86_64", windows))]
    {
        FrameAbi::WindowsX64
    }
}

/// The callee-saved state a parked context will resume with, decoded from
/// its saved switch frame (ABI v2 layouts, see `switch.rs` and
/// `switch_windows.rs`). Registers are named as the target ABI names them
/// and listed in frame order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterFrame {
    pub abi: FrameAbi,
    /// Callee-saved general-purpose registers.
    pub general: Vec<(&'static str, u64)>,
    /// Callee-saved vector registers: d8–d15 (low 64 bits, AArch64) or
    /// xmm6–xmm15 (all 128 bits, Windows). Empty on System V x86_64.
    pub vector: Vec<(&'static str, u128)>,
    /// Floating-point control/status words: `mxcsr` and `x87_cw`, or `fpcr`
    /// and `fpsr`.
    pub fp_env: Vec<(&'static str, u64)>,
    /// Windows TEB stack fields switched with the context; empty elsewhere.
    pub teb: Vec<(&'static str, u64)>,
    /// Where the switch returns to when the context is resumed.
    pub return_address: u64,
    /// rbp or x29.
    pub frame_pointer: u64,
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}
fn le128(b: &[u8], at: usize) -> u128 {
    u128::from_le_bytes(b[at..at + 16].try_into().unwrap())
}

impl RegisterFrame {
    /// Decode a host switch frame (exactly `SWITCH_FRAME_BYTES` bytes).
    pub fn parse(frame: &[u8]) -> Option<Self> {
        Self::parse_as(FrameAbi::HOST, frame)
    }

    /// Decode a switch frame of any supported layout.
    pub fn parse_as(abi: FrameAbi, b: &[u8]) -> Option<Self> {
        let need = match abi {
            FrameAbi::SysVX86_64 => 64,
            FrameAbi::Aarch64 => 192,
            FrameAbi::WindowsX64 => 288,
        };
        if b.len() != need {
            return None;
        }
        let x86_env = |b: &[u8]| {
            vec![
                (
                    "mxcsr",
                    u64::from(u32::from_le_bytes(b[0..4].try_into().unwrap())),
                ),
                (
                    "x87_cw",
                    u64::from(u16::from_le_bytes(b[4..6].try_into().unwrap())),
                ),
            ]
        };
        Some(match abi {
            FrameAbi::SysVX86_64 => {
                let names = ["r15", "r14", "r13", "r12", "rbx", "rbp"];
                let general: Vec<_> = names
                    .iter()
                    .enumerate()
                    .map(|(i, n)| (*n, le64(b, 8 + 8 * i)))
                    .collect();
                Self {
                    abi,
                    frame_pointer: le64(b, 48),
                    return_address: le64(b, 56),
                    general,
                    vector: vec![],
                    fp_env: x86_env(b),
                    teb: vec![],
                }
            }
            FrameAbi::Aarch64 => {
                const NAMES: [&str; 12] = [
                    "x19", "x20", "x21", "x22", "x23", "x24", "x25", "x26", "x27", "x28", "x29",
                    "x30",
                ];
                const D: [&str; 8] = ["d8", "d9", "d10", "d11", "d12", "d13", "d14", "d15"];
                Self {
                    abi,
                    general: NAMES
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (*n, le64(b, 8 * i)))
                        .collect(),
                    vector: D
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (*n, u128::from(le64(b, 96 + 8 * i))))
                        .collect(),
                    fp_env: vec![("fpcr", le64(b, 160)), ("fpsr", le64(b, 168))],
                    teb: vec![],
                    frame_pointer: le64(b, 80),
                    return_address: le64(b, 88),
                }
            }
            FrameAbi::WindowsX64 => {
                const G: [&str; 8] = ["r15", "r14", "r13", "r12", "rsi", "rdi", "rbx", "rbp"];
                const X: [&str; 10] = [
                    "xmm6", "xmm7", "xmm8", "xmm9", "xmm10", "xmm11", "xmm12", "xmm13", "xmm14",
                    "xmm15",
                ];
                Self {
                    abi,
                    general: G
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (*n, le64(b, 216 + 8 * i)))
                        .collect(),
                    vector: X
                        .iter()
                        .enumerate()
                        .map(|(i, n)| (*n, le128(b, 48 + 16 * i)))
                        .collect(),
                    fp_env: x86_env(b),
                    teb: vec![
                        ("guaranteed_stack_bytes", le64(b, 8)),
                        ("deallocation_stack", le64(b, 16)),
                        ("stack_limit", le64(b, 24)),
                        ("stack_base", le64(b, 32)),
                    ],
                    frame_pointer: le64(b, 272),
                    return_address: le64(b, 280),
                }
            }
        })
    }

    /// The saved value of a named register.
    pub fn get(&self, name: &str) -> Option<u64> {
        self.general
            .iter()
            .chain(&self.fp_env)
            .chain(&self.teb)
            .find(|(n, _)| *n == name)
            .map(|(_, v)| *v)
    }
}

/// Why an image could not be taken, decoded or restored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageError {
    /// The fiber's stack was not created at a fixed address
    /// ([`crate::FiberConfig::fixed_stack_base`]), so its context is not at a
    /// restorable address.
    NotFixedAddress,
    /// The template fiber has already started.
    NotFresh,
    /// The template's reservation is not at the captured address or size.
    AddressMismatch {
        captured: u64,
        template: u64,
    },
    StackSize {
        captured: u64,
        template: u64,
    },
    /// Different generated entry (different code).
    EntryMismatch {
        captured: u64,
        template: u64,
    },
    /// Same entry, different entry argument.
    ArgumentMismatch {
        captured: u64,
        template: u64,
    },
    /// The template's code owner is not the `Rc` the captured fiber ran
    /// (or that code has since been freed).
    CodeMismatch,
    /// The template's evidence vector is not the captured one.
    EvidenceMismatch,
    DataSize {
        captured: usize,
        template: usize,
    },
    /// An encoded image is shorter than its fields need.
    Truncated,
    BadMagic,
    UnsupportedVersion(u16),
    /// Captured on another target layout or ABI version.
    WrongTarget,
    /// The seal does not match: the image was modified, or sealed by another
    /// thread or process.
    BadSeal,
    /// A structurally impossible field (stack range, lengths).
    Malformed(&'static str),
    TrailingBytes(usize),
}

impl fmt::Display for ImageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "fiber image: {self:?}")
    }
}
impl std::error::Error for ImageError {}

struct Anchor {
    id: u64,
    code: Weak<dyn Any>,
    evidence: Weak<EvidenceVector>,
}

thread_local! {
    static ANCHORS: RefCell<Vec<Anchor>> = const { RefCell::new(Vec::new()) };
    static NEXT_ANCHOR: Cell<u64> = const { Cell::new(1) };
    static SEAL_KEY: RandomState = RandomState::new();
}

/// The anchor id naming this `(code, evidence)` pair on this thread. Weak
/// references keep both allocations from being reused while the anchor
/// lives; anchors whose code or evidence is gone are dropped first.
pub(crate) fn anchor(code: &Rc<dyn Any>, evidence: &Rc<EvidenceVector>) -> u64 {
    ANCHORS.with(|a| {
        let mut a = a.borrow_mut();
        a.retain(|x| x.code.strong_count() > 0 && x.evidence.strong_count() > 0);
        let code_w = Rc::downgrade(code);
        let ev_w = Rc::downgrade(evidence);
        if let Some(x) = a
            .iter()
            .find(|x| Weak::ptr_eq(&x.code, &code_w) && Weak::ptr_eq(&x.evidence, &ev_w))
        {
            return x.id;
        }
        let id = NEXT_ANCHOR.with(|n| {
            let id = n.get();
            n.set(id + 1);
            id
        });
        a.push(Anchor {
            id,
            code: code_w,
            evidence: ev_w,
        });
        id
    })
}

/// Whether `id` names exactly this live `(code, evidence)` pair.
pub(crate) fn anchor_matches(
    id: u64,
    code: &Rc<dyn Any>,
    evidence: &Rc<EvidenceVector>,
) -> (bool, bool) {
    ANCHORS.with(|a| {
        let a = a.borrow();
        match a.iter().find(|x| x.id == id) {
            None => (false, false),
            Some(x) => (
                Weak::ptr_eq(&x.code, &Rc::downgrade(code)),
                Weak::ptr_eq(&x.evidence, &Rc::downgrade(evidence)),
            ),
        }
    })
}

fn seal(body: &[u8]) -> u64 {
    SEAL_KEY.with(|k| {
        let mut h = k.build_hasher();
        h.write(body);
        h.finish()
    })
}

/// Image magic and format version.
pub const IMAGE_MAGIC: [u8; 8] = *b"VFIMAGE\x01";
pub const IMAGE_VERSION: u16 = 1;

/// A restorable image of a parked fiber with a fixed-address stack. Taken by
/// [`crate::Parked::image`]; restored by [`crate::Fiber::from_image`].
/// Fields are read-only so that an in-memory image is always one this
/// thread captured.
#[derive(Clone, PartialEq, Eq)]
pub struct FiberImage {
    pub(crate) stack: StackImage,
    pub(crate) reservation_base: u64,
    pub(crate) reservation_len: u64,
    pub(crate) context: u64,
    pub(crate) fiber_id: u64,
    pub(crate) generation: u64,
    pub(crate) park: ParkRequest,
    pub(crate) parks: u64,
    pub(crate) data: Vec<u8>,
    pub(crate) entry: u64,
    pub(crate) entry_arg: u64,
    pub(crate) evidence_header: u64,
    pub(crate) evidence: Vec<EvidenceId>,
    pub(crate) anchor: u64,
}

impl fmt::Debug for FiberImage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FiberImage")
            .field("fiber_id", &self.fiber_id)
            .field("generation", &self.generation)
            .field(
                "reservation_base",
                &format_args!("{:#x}", self.reservation_base),
            )
            .field("stack", &self.stack)
            .field("entry", &format_args!("{:#x}", self.entry))
            .field("entry_arg", &self.entry_arg)
            .finish_non_exhaustive()
    }
}

struct Reader<'a> {
    b: &'a [u8],
    at: usize,
}
impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], ImageError> {
        let end = self.at.checked_add(n).ok_or(ImageError::Truncated)?;
        let s = self.b.get(self.at..end).ok_or(ImageError::Truncated)?;
        self.at = end;
        Ok(s)
    }
    fn u16(&mut self) -> Result<u16, ImageError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn u32(&mut self) -> Result<u32, ImageError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, ImageError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn bytes(&mut self, max: usize) -> Result<Vec<u8>, ImageError> {
        let n = self.u64()?;
        if n > max as u64 {
            return Err(ImageError::Malformed("length exceeds the image"));
        }
        Ok(self.take(n as usize)?.to_vec())
    }
}

impl FiberImage {
    pub fn stack(&self) -> &StackImage {
        &self.stack
    }
    pub fn register_frame(&self) -> Option<RegisterFrame> {
        self.stack.register_frame()
    }
    /// Start and length of the fixed reservation the image restores onto.
    pub fn reservation(&self) -> (u64, u64) {
        (self.reservation_base, self.reservation_len)
    }
    /// Address of the fiber context (inside the reservation).
    pub fn context_address(&self) -> u64 {
        self.context
    }
    /// Fiber id of the captured fiber (a restored fiber gets a new one).
    pub fn fiber_id(&self) -> u64 {
        self.fiber_id
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    pub fn park(&self) -> ParkRequest {
        self.park
    }
    pub fn parks(&self) -> u64 {
        self.parks
    }
    /// The data frame at capture.
    pub fn data(&self) -> &[u8] {
        &self.data
    }
    pub fn entry(&self) -> u64 {
        self.entry
    }
    pub fn entry_arg(&self) -> u64 {
        self.entry_arg
    }
    /// Evidence identities of the captured vector, outermost first.
    pub fn evidence(&self) -> &[EvidenceId] {
        &self.evidence
    }

    /// Versioned little-endian encoding ending in this thread's seal.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(160 + self.stack.bytes.len() + self.data.len());
        out.extend_from_slice(&IMAGE_MAGIC);
        out.extend_from_slice(&IMAGE_VERSION.to_le_bytes());
        out.extend_from_slice(&FrameAbi::HOST.tag().to_le_bytes());
        out.extend_from_slice(&ABI_VERSION.to_le_bytes());
        for w in [
            self.reservation_base,
            self.reservation_len,
            self.context,
            self.fiber_id,
            self.generation,
            self.park.evidence,
        ] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend_from_slice(&self.park.op.to_le_bytes());
        out.extend_from_slice(&self.park.flags.to_le_bytes());
        for w in [
            self.park.arg0,
            self.park.arg1,
            self.parks,
            self.entry,
            self.entry_arg,
            self.evidence_header,
            self.anchor,
            self.stack.saved_sp,
            self.stack.top,
        ] {
            out.extend_from_slice(&w.to_le_bytes());
        }
        out.extend_from_slice(&(self.evidence.len() as u64).to_le_bytes());
        for id in &self.evidence {
            out.extend_from_slice(&id.to_le_bytes());
        }
        for b in [&self.stack.bytes, &self.data] {
            out.extend_from_slice(&(b.len() as u64).to_le_bytes());
            out.extend_from_slice(b);
        }
        let s = seal(&out);
        out.extend_from_slice(&s.to_le_bytes());
        out
    }

    /// Decode an image sealed by this thread. Foreign or modified images are
    /// rejected with [`ImageError::BadSeal`] before any field is trusted.
    pub fn decode(bytes: &[u8]) -> Result<Self, ImageError> {
        if bytes.len() < IMAGE_MAGIC.len() + 8 {
            return Err(ImageError::Truncated);
        }
        if bytes[..8] != IMAGE_MAGIC {
            return Err(ImageError::BadMagic);
        }
        let (body, tag) = bytes.split_at(bytes.len() - 8);
        if seal(body) != u64::from_le_bytes(tag.try_into().unwrap()) {
            return Err(ImageError::BadSeal);
        }
        let mut r = Reader { b: body, at: 8 };
        let version = r.u16()?;
        if version != IMAGE_VERSION {
            return Err(ImageError::UnsupportedVersion(version));
        }
        if r.u16()? != FrameAbi::HOST.tag() || r.u32()? != ABI_VERSION {
            return Err(ImageError::WrongTarget);
        }
        let reservation_base = r.u64()?;
        let reservation_len = r.u64()?;
        let context = r.u64()?;
        let fiber_id = r.u64()?;
        let generation = r.u64()?;
        let park = ParkRequest {
            evidence: r.u64()?,
            op: r.u32()?,
            flags: r.u32()?,
            arg0: r.u64()?,
            arg1: r.u64()?,
        };
        let parks = r.u64()?;
        let entry = r.u64()?;
        let entry_arg = r.u64()?;
        let evidence_header = r.u64()?;
        let anchor = r.u64()?;
        let saved_sp = r.u64()?;
        let top = r.u64()?;
        let n = r.u64()?;
        if n > (body.len() / 8) as u64 {
            return Err(ImageError::Malformed("evidence length exceeds the image"));
        }
        let mut evidence = Vec::with_capacity(n as usize);
        for _ in 0..n {
            evidence.push(r.u64()?);
        }
        let stack = r.bytes(body.len())?;
        let data = r.bytes(body.len())?;
        if r.at != body.len() {
            return Err(ImageError::TrailingBytes(body.len() - r.at));
        }
        let image = Self {
            stack: StackImage {
                saved_sp,
                top,
                bytes: stack,
            },
            reservation_base,
            reservation_len,
            context,
            fiber_id,
            generation,
            park,
            parks,
            data,
            entry,
            entry_arg,
            evidence_header,
            evidence,
            anchor,
        };
        image.validate()?;
        Ok(image)
    }

    /// Structural checks shared by capture and decode.
    pub(crate) fn validate(&self) -> Result<(), ImageError> {
        let end = self
            .reservation_base
            .checked_add(self.reservation_len)
            .ok_or(ImageError::Malformed("reservation overflows"))?;
        let s = &self.stack;
        if s.top != end || s.saved_sp >= s.top || s.saved_sp < self.reservation_base {
            return Err(ImageError::Malformed("stack range outside the reservation"));
        }
        if !s.saved_sp.is_multiple_of(16) || !s.top.is_multiple_of(16) {
            return Err(ImageError::Malformed("misaligned stack pointer"));
        }
        if s.top - s.saved_sp != s.bytes.len() as u64 || s.bytes.len() < SWITCH_FRAME_BYTES {
            return Err(ImageError::Malformed("stack length"));
        }
        if self.context < self.reservation_base || self.context >= s.saved_sp {
            return Err(ImageError::Malformed("context outside the reservation"));
        }
        Ok(())
    }
}
