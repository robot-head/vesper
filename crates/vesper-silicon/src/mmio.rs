//! Access-typed registers and typestate peripheral singletons bound at run
//! time from a [`DeviceModel`] (§26.1 item 4, §26.2).
//!
//! Registers live on a [`SimBus`], a software model of the device's register
//! file that applies each field's SVD semantics on CPU reads and writes
//! (write-1-to-clear, read-to-clear, read-only bits ignored, ...). No real
//! memory-mapped I/O is performed: this crate has no `unsafe` and validates
//! the modelled protocol only.
//!
//! [`Reg<A>`] is bound from a register path and refuses to bind when the
//! model's [`RegisterClass`] disagrees with the marker `A`. Each marker
//! exposes only the operations that are hazard-free for its class: there is
//! no `modify` on [`Write1ToClear`] registers and no plain `read` on
//! [`ReadToClear`] ones.
//!
//! ```compile_fail
//! # use vesper_silicon::mmio::*;
//! fn rmw(r: &Reg<Write1ToClear>) { r.modify(0, 1); } // no read-modify-write on W1C
//! ```
//!
//! ```compile_fail
//! # use vesper_silicon::mmio::*;
//! fn peek(r: &Reg<ReadToClear>) -> u64 { r.read() } // reads must be consumes
//! ```
//!
//! ```compile_fail
//! # use vesper_silicon::mmio::*;
//! fn pop(r: &Reg<ConsumeWrite>) -> u64 { r.read() } // FIFO reads must be consumes
//! ```
//!
//! ```compile_fail
//! # use vesper_silicon::mmio::*;
//! fn poke(r: &Reg<ReadOnly>) { r.write(1); } // read-only registers have no write
//! ```
//!
//! Peripheral typestate ([`Spi`]) makes illegal transitions unrepresentable:
//!
//! ```compile_fail
//! # use vesper_silicon::mmio::*;
//! fn send(spi: &mut Spi<Disabled>) { let _ = spi.transfer(0x55); } // must configure first
//! ```

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;
use std::rc::Rc;

use crate::svd::{DeviceModel, FieldAccess, RegisterClass, RegisterModel};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BusError {
    UnknownRegister(String),
    AlreadyTaken(String),
    MissingField {
        register: String,
        field: String,
    },
    ClassMismatch {
        register: String,
        model: RegisterClass,
        requested: RegisterClass,
    },
    ValueOutOfRange {
        register: String,
        value: u64,
    },
    MaskNotWriteClear {
        register: String,
        mask: u64,
    },
}

#[derive(Debug)]
struct Cell {
    model: RegisterModel,
    value: u64,
}

/// Software register file for one device.
#[derive(Debug)]
pub struct SimBus {
    by_path: BTreeMap<String, u64>,
    cells: BTreeMap<u64, Cell>,
    taken: BTreeSet<String>,
    /// `(address, value, is_write)` of every CPU access, in order.
    log: Vec<(u64, u64, bool)>,
}

/// Shared handle to a bus.
pub type Bus = Rc<RefCell<SimBus>>;

impl SimBus {
    pub fn new(model: &DeviceModel) -> Bus {
        let mut by_path = BTreeMap::new();
        let mut cells = BTreeMap::new();
        for p in &model.peripherals {
            for r in &p.registers {
                by_path.insert(r.path.clone(), r.address);
                cells.insert(
                    r.address,
                    Cell {
                        model: r.clone(),
                        value: r.reset_value & r.width_mask(),
                    },
                );
            }
        }
        Rc::new(RefCell::new(Self {
            by_path,
            cells,
            taken: BTreeSet::new(),
            log: Vec::new(),
        }))
    }

    fn cell(&mut self, path: &str) -> Result<&mut Cell, BusError> {
        let addr = *self
            .by_path
            .get(path)
            .ok_or_else(|| BusError::UnknownRegister(path.to_string()))?;
        Ok(self.cells.get_mut(&addr).expect("path index is consistent"))
    }

    /// Value without side effects (a debugger view, not a CPU read).
    pub fn peek(&mut self, path: &str) -> Result<u64, BusError> {
        Ok(self.cell(path)?.value)
    }

    /// The device hardware sets bits (e.g. a pending interrupt or a status).
    pub fn hw_set(&mut self, path: &str, bits: u64) -> Result<(), BusError> {
        let c = self.cell(path)?;
        if bits & !c.model.width_mask() != 0 {
            return Err(BusError::ValueOutOfRange {
                register: path.to_string(),
                value: bits,
            });
        }
        c.value |= bits;
        Ok(())
    }

    /// The device hardware clears bits.
    pub fn hw_clear(&mut self, path: &str, bits: u64) -> Result<(), BusError> {
        let c = self.cell(path)?;
        c.value &= !bits;
        Ok(())
    }

    pub fn log(&self) -> &[(u64, u64, bool)] {
        &self.log
    }

    fn cpu_read(&mut self, addr: u64) -> u64 {
        let c = self.cells.get_mut(&addr).expect("bound register exists");
        let v = c.value & c.model.mask_where(FieldAccess::readable);
        c.value &= !c.model.mask_where(|a| a == FieldAccess::ReadToClear);
        self.log.push((addr, v, false));
        v
    }

    fn cpu_write(&mut self, addr: u64, w: u64) {
        let c = self.cells.get_mut(&addr).expect("bound register exists");
        let mut v = c.value;
        for f in &c.model.fields {
            let m = f.mask();
            let wf = w & m;
            v = match f.access {
                FieldAccess::ReadWrite
                | FieldAccess::WriteOnly
                | FieldAccess::ReadSideEffectWrite => (v & !m) | wf,
                FieldAccess::ReadOnly | FieldAccess::ReadToClear | FieldAccess::ReadSideEffect => v,
                FieldAccess::Write1ToClear => v & !wf,
                FieldAccess::Write1ToSet => v | wf,
                FieldAccess::Write1ToToggle => v ^ wf,
                FieldAccess::Write0ToClear => v & !(m & !wf),
            };
        }
        c.value = v;
        self.log.push((addr, w, true));
    }
}

mod sealed {
    pub trait Sealed {}
}

/// Access-policy marker of a typed register.
pub trait AccessMarker: sealed::Sealed {
    const CLASS: RegisterClass;
}

macro_rules! marker {
    ($($name:ident => $class:ident),* $(,)?) => {$(
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct $name;
        impl sealed::Sealed for $name {}
        impl AccessMarker for $name {
            const CLASS: RegisterClass = RegisterClass::$class;
        }
    )*};
}

marker!(
    ReadOnly => ReadOnly,
    ReadWrite => ReadWrite,
    WriteOnly => WriteOnly,
    Write1ToClear => WriteClear,
    ReadToClear => ReadToClear,
    ConsumeWrite => ConsumeWrite,
);

/// A register bound to a bus with access policy `A`.
#[derive(Debug)]
pub struct Reg<A: AccessMarker> {
    bus: Bus,
    addr: u64,
    model: RegisterModel,
    _a: PhantomData<A>,
}

impl<A: AccessMarker> Reg<A> {
    /// Binds `path`; fails unless the model's class is `A::CLASS`.
    pub fn bind(bus: &Bus, path: &str) -> Result<Self, BusError> {
        let mut b = bus.borrow_mut();
        let c = b.cell(path)?;
        let model = c.model.clone();
        if model.class() != A::CLASS {
            return Err(BusError::ClassMismatch {
                register: path.to_string(),
                model: model.class(),
                requested: A::CLASS,
            });
        }
        Ok(Self {
            bus: bus.clone(),
            addr: model.address,
            model,
            _a: PhantomData,
        })
    }

    pub fn model(&self) -> &RegisterModel {
        &self.model
    }

    /// Mask of one named field.
    pub fn field_mask(&self, field: &str) -> Result<u64, BusError> {
        self.model
            .field(field)
            .map(|f| f.mask())
            .ok_or_else(|| BusError::MissingField {
                register: self.model.path.clone(),
                field: field.to_string(),
            })
    }

    fn raw_read(&self) -> u64 {
        self.bus.borrow_mut().cpu_read(self.addr)
    }

    fn raw_write(&self, v: u64) -> Result<(), BusError> {
        if v & !self.model.width_mask() != 0 {
            return Err(BusError::ValueOutOfRange {
                register: self.model.path.clone(),
                value: v,
            });
        }
        self.bus.borrow_mut().cpu_write(self.addr, v);
        Ok(())
    }
}

impl Reg<ReadOnly> {
    pub fn read(&self) -> u64 {
        self.raw_read()
    }
}

impl Reg<ReadWrite> {
    pub fn read(&self) -> u64 {
        self.raw_read()
    }

    pub fn write(&self, v: u64) -> Result<(), BusError> {
        self.raw_write(v)
    }

    /// `reg = (reg & !clear) | set`. Hazard-free here: the class has no
    /// write-clear or read-to-clear field.
    pub fn modify(&self, clear: u64, set: u64) -> Result<(), BusError> {
        let v = (self.raw_read() & !clear) | set;
        self.raw_write(v & self.model.width_mask())
    }
}

impl Reg<WriteOnly> {
    pub fn write(&self, v: u64) -> Result<(), BusError> {
        self.raw_write(v)
    }
}

impl Reg<Write1ToClear> {
    /// Reading a W1C status register has no side effect.
    pub fn read(&self) -> u64 {
        self.raw_read()
    }

    /// Acknowledges exactly `mask`. Write-clear bits outside `mask` are
    /// written with their no-effect value (0 for W1C/W1T, 1 for W0C) and
    /// plain read/write bits keep their current value, so concurrent pending
    /// bits survive.
    pub fn write_clear_mask(&self, mask: u64) -> Result<(), BusError> {
        let w1c = self.model.mask_where(|a| a == FieldAccess::Write1ToClear);
        if mask & !w1c != 0 || mask == 0 {
            return Err(BusError::MaskNotWriteClear {
                register: self.model.path.clone(),
                mask,
            });
        }
        let rw = self.model.mask_where(|a| a == FieldAccess::ReadWrite);
        let w0c = self.model.mask_where(|a| a == FieldAccess::Write0ToClear);
        let keep = if rw != 0 { self.raw_read() & rw } else { 0 };
        self.raw_write(keep | w0c | mask)
    }
}

impl Reg<ReadToClear> {
    /// The only read: returns the value and the hardware clears the
    /// read-to-clear bits (or performs the read side effect).
    pub fn consume(&self) -> u64 {
        self.raw_read()
    }
}

impl Reg<ConsumeWrite> {
    /// The only read, with its side effect (FIFO pop, spinlock claim). The
    /// software bus does not model the side effect beyond logging the read.
    pub fn consume(&self) -> u64 {
        self.raw_read()
    }

    /// A plain write (FIFO push, spinlock release).
    pub fn write(&self, v: u64) -> Result<(), BusError> {
        self.raw_write(v)
    }
}

/// Claims the peripheral singleton `name` on `bus`; a second claim fails.
/// Generated peripheral structs (see [`crate::pac`]) call this from `take`.
pub fn claim_peripheral(bus: &Bus, name: &str) -> Result<(), BusError> {
    let mut b = bus.borrow_mut();
    if !b.taken.insert(name.to_string()) {
        return Err(BusError::AlreadyTaken(name.to_string()));
    }
    Ok(())
}

/// SPI typestate markers.
#[derive(Debug)]
pub struct Disabled;
#[derive(Debug)]
pub struct MasterReady;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SpiError {
    Bus(BusError),
    InvalidBaudDivider(u8),
    TxNotEmpty,
    RxEmpty,
}

impl From<BusError> for SpiError {
    fn from(e: BusError) -> Self {
        SpiError::Bus(e)
    }
}

/// An SPI peripheral singleton in state `S`, bound from the model's
/// `CR1` (SPE, MSTR, BR), `SR` (TXE, RXNE) and `DR` registers.
#[derive(Debug)]
pub struct Spi<S> {
    regs: Box<SpiRegs>,
    _s: PhantomData<S>,
}

#[derive(Debug)]
struct SpiRegs {
    name: String,
    bus: Bus,
    cr1: Reg<ReadWrite>,
    sr: Reg<Write1ToClear>,
    dr: Reg<ReadWrite>,
    spe: u64,
    mstr: u64,
    br: u64,
    txe: u64,
    rxne: u64,
}

impl Spi<Disabled> {
    /// Takes the singleton `name` (e.g. `SPI1`) from `bus`. A second take of
    /// the same peripheral fails. The peripheral must be disabled at reset.
    pub fn take(bus: &Bus, name: &str) -> Result<Self, BusError> {
        if bus.borrow().taken.contains(name) {
            return Err(BusError::AlreadyTaken(name.to_string()));
        }
        let cr1 = Reg::<ReadWrite>::bind(bus, &format!("{name}.CR1"))?;
        let sr = Reg::<Write1ToClear>::bind(bus, &format!("{name}.SR"))?;
        let dr = Reg::<ReadWrite>::bind(bus, &format!("{name}.DR"))?;
        let regs = SpiRegs {
            name: name.to_string(),
            bus: bus.clone(),
            spe: cr1.field_mask("SPE")?,
            mstr: cr1.field_mask("MSTR")?,
            br: cr1.field_mask("BR")?,
            txe: sr.field_mask("TXE")?,
            rxne: sr.field_mask("RXNE")?,
            cr1,
            sr,
            dr,
        };
        bus.borrow_mut().taken.insert(name.to_string());
        Ok(Spi {
            regs: Box::new(regs),
            _s: PhantomData,
        })
    }

    /// Configures master mode with baud divider `2^(div+1)` and enables the
    /// peripheral. On failure the disabled singleton is returned.
    pub fn into_master(self, div: u8) -> Result<Spi<MasterReady>, (Self, SpiError)> {
        let shift = self.regs.br.trailing_zeros();
        let br = u64::from(div).checked_shl(shift).unwrap_or(u64::MAX);
        if br & !self.regs.br != 0 {
            return Err((self, SpiError::InvalidBaudDivider(div)));
        }
        if let Err(e) = self
            .regs
            .cr1
            .modify(self.regs.br | self.regs.spe, self.regs.mstr | br)
        {
            return Err((self, e.into()));
        }
        if let Err(e) = self.regs.cr1.modify(0, self.regs.spe) {
            return Err((self, e.into()));
        }
        Ok(self.retype())
    }
}

impl Spi<MasterReady> {
    /// Full-duplex byte exchange: requires TXE, writes DR, then requires
    /// RXNE and reads DR.
    pub fn transfer(&mut self, byte: u8) -> Result<u8, SpiError> {
        if self.regs.sr.read() & self.regs.txe == 0 {
            return Err(SpiError::TxNotEmpty);
        }
        self.regs.dr.write(u64::from(byte))?;
        if self.regs.sr.read() & self.regs.rxne == 0 {
            return Err(SpiError::RxEmpty);
        }
        Ok((self.regs.dr.read() & 0xff) as u8)
    }

    pub fn disable(self) -> Spi<Disabled> {
        // Clearing SPE on a ReadWrite register cannot fail: in range.
        let _ = self.regs.cr1.modify(self.regs.spe, 0);
        self.retype()
    }
}

impl<S> Spi<S> {
    pub fn name(&self) -> &str {
        &self.regs.name
    }

    pub fn bus(&self) -> &Bus {
        &self.regs.bus
    }

    fn retype<T>(self) -> Spi<T> {
        Spi {
            regs: self.regs,
            _s: PhantomData,
        }
    }
}
