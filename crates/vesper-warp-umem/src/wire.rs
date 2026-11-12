//! `XdpDescWire`: the exact 16-byte Linux `struct xdp_desc` layout.
//!
//! The layout is declared through `vesper-ffi`'s checked wire registry (the
//! same mechanism Bridge uses for every other foreign layout) and the codec
//! encodes/decodes through the resulting `WireView`s, so the byte positions are
//! the registry's, not hand-computed offsets. The rings themselves carry the
//! decoded [`XdpDesc`] value; the Linux backend reads `libc::xdp_desc`, whose
//! layout a test checks against this declaration.

use vesper_ffi::{
    Declaration, Endian, Field, Integer, Registry, ViewError, WireLayout, WireType,
    initialize_wire, view_wire,
};

/// Name of the declared wire struct (design §35.2).
pub const XDP_DESC_WIRE: &str = "XdpDescWire";
/// Size of one encoded descriptor.
pub const XDP_DESC_BYTES: usize = 16;

/// A decoded AF_XDP descriptor (`struct xdp_desc { __u64 addr; __u32 len; __u32 options; }`).
///
/// `addr` is a byte offset into the UMEM (aligned-chunk mode: chunk base plus
/// data offset), not a pointer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct XdpDesc {
    pub addr: u64,
    pub len: u32,
    pub options: u32,
}

/// Storage for one encoded descriptor, aligned like the kernel's
/// `struct xdp_desc` (the wire layout itself is byte-aligned).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(C, align(8))]
pub struct XdpDescBytes(pub [u8; XDP_DESC_BYTES]);

/// The checked `XdpDescWire` layout and its codec.
#[derive(Debug, Clone)]
pub struct XdpDescWire {
    layout: WireLayout,
}

impl XdpDescWire {
    /// Declares `wire struct XdpDescWire { umem_addr: u64le, pkt_byte_len: u32le, options: u32le }`.
    pub fn new() -> Self {
        let le = |bits| WireType::Integer(Integer::unsigned(bits, Endian::Little));
        let registry = Registry::new([Declaration::new(
            XDP_DESC_WIRE,
            vec![
                Field::new("umem_addr", le(64)),
                Field::new("pkt_byte_len", le(32)),
                Field::new("options", le(32)),
            ],
        )])
        .expect("XdpDescWire declaration is well formed");
        let layout = registry
            .compile(XDP_DESC_WIRE)
            .expect("XdpDescWire compiles");
        assert_eq!(layout.size(), XDP_DESC_BYTES, "struct xdp_desc is 16 bytes");
        Self { layout }
    }

    pub fn layout(&self) -> &WireLayout {
        &self.layout
    }

    pub fn encode(&self, desc: XdpDesc) -> Result<XdpDescBytes, ViewError> {
        let mut out = XdpDescBytes::default();
        let (mut view, _) = initialize_wire(&self.layout, &mut out.0)?;
        view.write_u64(&["umem_addr"], desc.addr)?;
        view.write_u64(&["pkt_byte_len"], u64::from(desc.len))?;
        view.write_u64(&["options"], u64::from(desc.options))?;
        Ok(out)
    }

    pub fn decode(&self, bytes: &XdpDescBytes) -> Result<XdpDesc, ViewError> {
        let view = view_wire(&self.layout, &bytes.0)?;
        view.validate_padding()?;
        // The declared widths make these conversions lossless; a failure would
        // mean the registry returned a wider value than declared.
        let narrow = |v: u64| u32::try_from(v).map_err(|_| ViewError::OutOfRange);
        Ok(XdpDesc {
            addr: view.read_u64(&["umem_addr"])?,
            len: narrow(view.read_u64(&["pkt_byte_len"])?)?,
            options: narrow(view.read_u64(&["options"])?)?,
        })
    }
}

impl Default for XdpDescWire {
    fn default() -> Self {
        Self::new()
    }
}
