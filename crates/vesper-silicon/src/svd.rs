//! CMSIS-SVD register model (§26.1 item 4).
//!
//! Parsing is done by the `svd-parser` crate (with `derivedFrom`, `dim`
//! arrays and register-property inheritance expanded); this module turns the
//! parsed device into a flat register model whose every field carries one
//! [`FieldAccess`] kind. SVD combinations this model does not represent are
//! rejected with [`SvdError::UnsupportedAccess`] rather than approximated.
//!
//! Supported `readAction`s are `clear` ([`FieldAccess::ReadToClear`]) and
//! `modify` ([`FieldAccess::ReadSideEffect`] / [`FieldAccess::ReadSideEffectWrite`]:
//! the RP2040 SVD marks FIFO pops, latching timer reads and hardware
//! spinlocks this way); `set` and `modifyExternal` are rejected.

use std::collections::BTreeSet;

use svd_parser::svd::{
    self, Access, ModifiedWriteValues, ReadAction, RegisterCluster, RegisterProperties,
};

/// Access kind of one bitfield.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum FieldAccess {
    ReadWrite,
    ReadOnly,
    WriteOnly,
    /// `modifiedWriteValues=oneToClear`: writing 1 clears, writing 0 keeps.
    Write1ToClear,
    /// `readAction=clear`: reading returns the value and clears it.
    ReadToClear,
    /// `modifiedWriteValues=oneToSet`.
    Write1ToSet,
    /// `modifiedWriteValues=oneToToggle`.
    Write1ToToggle,
    /// `modifiedWriteValues=zeroToClear`: writing 0 clears, writing 1 keeps.
    Write0ToClear,
    /// `readAction=modify` on a read-only field: a read has a side effect
    /// (pops a FIFO, latches a counter); it must be an explicit consume.
    ReadSideEffect,
    /// `readAction=modify` on a writable field (a FIFO data register such
    /// as a UART `DR`, or a hardware spinlock): reads are consuming, writes
    /// are plain writes.
    ReadSideEffectWrite,
}

impl FieldAccess {
    /// Writing back a value just read would change state: read-modify-write
    /// is a hazard for these kinds.
    pub fn rmw_hazard(self) -> bool {
        matches!(
            self,
            FieldAccess::Write1ToClear | FieldAccess::Write1ToToggle | FieldAccess::Write0ToClear
        )
    }

    pub fn readable(self) -> bool {
        !matches!(self, FieldAccess::WriteOnly)
    }

    pub fn writable(self) -> bool {
        !matches!(
            self,
            FieldAccess::ReadOnly | FieldAccess::ReadToClear | FieldAccess::ReadSideEffect
        )
    }

    /// A CPU read changes device state (`readAction=clear` or `modify`), so
    /// it may only happen as an explicit consume.
    pub fn consuming_read(self) -> bool {
        matches!(
            self,
            FieldAccess::ReadToClear
                | FieldAccess::ReadSideEffect
                | FieldAccess::ReadSideEffectWrite
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FieldModel {
    pub name: String,
    pub offset: u32,
    pub width: u32,
    pub access: FieldAccess,
}

impl FieldModel {
    pub fn mask(&self) -> u64 {
        let ones = if self.width >= 64 {
            u64::MAX
        } else {
            (1u64 << self.width) - 1
        };
        ones << self.offset
    }
}

/// How a whole register may be bound by the typed MMIO API.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RegisterClass {
    /// Every field read-only.
    ReadOnly,
    /// Every field write-only.
    WriteOnly,
    /// Contains a read-to-clear field, or only read-side-effect and
    /// read-only fields: reads are consuming and there are no writes.
    ReadToClear,
    /// Contains a `readAction=modify` field and writable fields (FIFO data
    /// registers, hardware spinlocks): reads are consuming, writes are plain.
    ConsumeWrite,
    /// Contains a W1C/W1T/W0C field: no read-modify-write.
    WriteClear,
    /// Plain read/write (read-only and write-1-to-set bits may be mixed in).
    ReadWrite,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisterModel {
    /// `PERIPHERAL.REGISTER` (cluster names joined with `.`).
    pub path: String,
    pub name: String,
    pub offset: u32,
    pub address: u64,
    pub size_bits: u32,
    pub reset_value: u64,
    pub fields: Vec<FieldModel>,
}

impl RegisterModel {
    pub fn mask_where(&self, pred: impl Fn(FieldAccess) -> bool) -> u64 {
        self.fields
            .iter()
            .filter(|f| pred(f.access))
            .fold(0, |m, f| m | f.mask())
    }

    pub fn width_mask(&self) -> u64 {
        if self.size_bits >= 64 {
            u64::MAX
        } else {
            (1u64 << self.size_bits) - 1
        }
    }

    pub fn has(&self, a: FieldAccess) -> bool {
        self.fields.iter().any(|f| f.access == a)
    }

    pub fn field(&self, name: &str) -> Option<&FieldModel> {
        self.fields.iter().find(|f| f.name == name)
    }

    pub fn class(&self) -> RegisterClass {
        if self
            .fields
            .iter()
            .all(|f| f.access == FieldAccess::ReadOnly)
        {
            RegisterClass::ReadOnly
        } else if self
            .fields
            .iter()
            .all(|f| f.access == FieldAccess::WriteOnly)
        {
            RegisterClass::WriteOnly
        } else if self.has(FieldAccess::ReadToClear) {
            RegisterClass::ReadToClear
        } else if self.fields.iter().any(|f| f.access.consuming_read()) {
            if self.fields.iter().any(|f| f.access.writable()) {
                RegisterClass::ConsumeWrite
            } else {
                RegisterClass::ReadToClear
            }
        } else if self.fields.iter().any(|f| f.access.rmw_hazard()) {
            RegisterClass::WriteClear
        } else {
            RegisterClass::ReadWrite
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PeripheralModel {
    pub name: String,
    pub base_address: u64,
    pub registers: Vec<RegisterModel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceModel {
    pub name: String,
    pub peripherals: Vec<PeripheralModel>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SvdError {
    Parse(String),
    UnsupportedAccess {
        register: String,
        field: String,
        detail: String,
    },
    MissingSize {
        register: String,
    },
    UnsupportedSize {
        register: String,
        bits: u32,
    },
    FieldOutOfRange {
        register: String,
        field: String,
    },
    FieldOverlap {
        register: String,
        field: String,
    },
    DuplicateRegister {
        path: String,
    },
    AddressOverflow {
        register: String,
    },
}

impl std::fmt::Display for SvdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for SvdError {}

impl DeviceModel {
    /// Parses CMSIS-SVD XML and builds the register model.
    pub fn from_svd(xml: &str) -> Result<Self, SvdError> {
        let config = svd_parser::Config::default()
            .expand(true)
            .expand_properties(true);
        let device = svd_parser::parse_with_config(xml, &config)
            .map_err(|e| SvdError::Parse(format!("{e:#}")))?;
        Self::from_device(&device)
    }

    pub fn from_device(device: &svd::Device) -> Result<Self, SvdError> {
        let mut peripherals = Vec::new();
        let mut paths = BTreeSet::new();
        for p in &device.peripherals {
            let mut registers = Vec::new();
            let props = inherit(
                &p.default_register_properties,
                &device.default_register_properties,
            );
            if let Some(children) = &p.registers {
                collect(&p.name, p.base_address, 0, &props, children, &mut registers)?;
            }
            for r in &registers {
                if !paths.insert(r.path.clone()) {
                    return Err(SvdError::DuplicateRegister {
                        path: r.path.clone(),
                    });
                }
            }
            peripherals.push(PeripheralModel {
                name: p.name.clone(),
                base_address: p.base_address,
                registers,
            });
        }
        Ok(Self {
            name: device.name.clone(),
            peripherals,
        })
    }

    pub fn peripheral(&self, name: &str) -> Option<&PeripheralModel> {
        self.peripherals.iter().find(|p| p.name == name)
    }

    /// Looks up `PERIPHERAL.REGISTER`.
    pub fn register(&self, path: &str) -> Option<&RegisterModel> {
        self.peripherals
            .iter()
            .flat_map(|p| p.registers.iter())
            .find(|r| r.path == path)
    }

    /// The register at an absolute address, if any.
    pub fn register_at(&self, address: u64) -> Option<&RegisterModel> {
        self.peripherals
            .iter()
            .flat_map(|p| p.registers.iter())
            .find(|r| r.address == address)
    }
}

fn collect(
    prefix: &str,
    base: u64,
    offset: u32,
    props: &RegisterProperties,
    children: &[RegisterCluster],
    out: &mut Vec<RegisterModel>,
) -> Result<(), SvdError> {
    for child in children {
        match child {
            RegisterCluster::Register(r) => {
                let path = format!("{prefix}.{}", r.name);
                let rp = inherit(&r.properties, props);
                let size = rp.size.ok_or_else(|| SvdError::MissingSize {
                    register: path.clone(),
                })?;
                if !matches!(size, 8 | 16 | 32 | 64) {
                    return Err(SvdError::UnsupportedSize {
                        register: path,
                        bits: size,
                    });
                }
                let off = offset.checked_add(r.address_offset).ok_or_else(|| {
                    SvdError::AddressOverflow {
                        register: path.clone(),
                    }
                })?;
                let address =
                    base.checked_add(u64::from(off))
                        .ok_or_else(|| SvdError::AddressOverflow {
                            register: path.clone(),
                        })?;
                let mut fields = Vec::new();
                let mut used = 0u64;
                let reg_fields: Vec<&svd::FieldInfo> =
                    r.fields.iter().flatten().map(|f| &**f).collect();
                if reg_fields.is_empty() {
                    let access = classify(
                        &path,
                        &r.name,
                        rp.access,
                        r.modified_write_values,
                        r.read_action,
                    )?;
                    fields.push(FieldModel {
                        name: r.name.clone(),
                        offset: 0,
                        width: size,
                        access,
                    });
                }
                for f in reg_fields {
                    let fend = f.bit_range.offset.checked_add(f.bit_range.width);
                    if f.bit_range.width == 0 || fend.is_none_or(|e| e > size) {
                        return Err(SvdError::FieldOutOfRange {
                            register: path,
                            field: f.name.clone(),
                        });
                    }
                    let access = classify(
                        &path,
                        &f.name,
                        f.access.or(rp.access),
                        f.modified_write_values.or(r.modified_write_values),
                        f.read_action.or(r.read_action),
                    )?;
                    let model = FieldModel {
                        name: f.name.clone(),
                        offset: f.bit_range.offset,
                        width: f.bit_range.width,
                        access,
                    };
                    if used & model.mask() != 0 {
                        return Err(SvdError::FieldOverlap {
                            register: path,
                            field: f.name.clone(),
                        });
                    }
                    used |= model.mask();
                    fields.push(model);
                }
                fields.sort_by_key(|f| f.offset);
                out.push(RegisterModel {
                    path,
                    name: r.name.clone(),
                    offset: off,
                    address,
                    size_bits: size,
                    reset_value: rp.reset_value.unwrap_or(0),
                    fields,
                });
            }
            RegisterCluster::Cluster(c) => {
                let cp = inherit(&c.default_register_properties, props);
                let off = offset.checked_add(c.address_offset).ok_or_else(|| {
                    SvdError::AddressOverflow {
                        register: format!("{prefix}.{}", c.name),
                    }
                })?;
                collect(
                    &format!("{prefix}.{}", c.name),
                    base,
                    off,
                    &cp,
                    &c.children,
                    out,
                )?;
            }
        }
    }
    Ok(())
}

/// Inherit unset register properties from the enclosing scope.
fn inherit(own: &RegisterProperties, parent: &RegisterProperties) -> RegisterProperties {
    let mut p = *own;
    p.size = own.size.or(parent.size);
    p.access = own.access.or(parent.access);
    p.protection = own.protection.or(parent.protection);
    p.reset_value = own.reset_value.or(parent.reset_value);
    p.reset_mask = own.reset_mask.or(parent.reset_mask);
    p
}

fn classify(
    register: &str,
    field: &str,
    access: Option<Access>,
    mwv: Option<ModifiedWriteValues>,
    read_action: Option<ReadAction>,
) -> Result<FieldAccess, SvdError> {
    let unsupported = |detail: String| SvdError::UnsupportedAccess {
        register: register.to_string(),
        field: field.to_string(),
        detail,
    };
    let access = access.unwrap_or(Access::ReadWrite);
    let (read_clears, read_modifies) = match read_action {
        None => (false, false),
        Some(ReadAction::Clear) => (true, false),
        Some(ReadAction::Modify) => (false, true),
        Some(other) => return Err(unsupported(format!("readAction={other:?}"))),
    };
    let kind = match access {
        Access::ReadOnly => {
            if mwv.is_some_and(|m| m != ModifiedWriteValues::Modify) {
                return Err(unsupported(
                    "modifiedWriteValues on a read-only field".to_string(),
                ));
            }
            if read_clears {
                FieldAccess::ReadToClear
            } else if read_modifies {
                FieldAccess::ReadSideEffect
            } else {
                FieldAccess::ReadOnly
            }
        }
        Access::WriteOnly | Access::WriteOnce => {
            if read_clears || read_modifies {
                return Err(unsupported("readAction on a write-only field".to_string()));
            }
            match mwv {
                None | Some(ModifiedWriteValues::Modify) => FieldAccess::WriteOnly,
                Some(m) => {
                    return Err(unsupported(format!(
                        "write-only with modifiedWriteValues={m:?}"
                    )));
                }
            }
        }
        Access::ReadWrite | Access::ReadWriteOnce => {
            let w = match mwv {
                None | Some(ModifiedWriteValues::Modify) => FieldAccess::ReadWrite,
                Some(ModifiedWriteValues::OneToClear) => FieldAccess::Write1ToClear,
                Some(ModifiedWriteValues::OneToSet) => FieldAccess::Write1ToSet,
                Some(ModifiedWriteValues::OneToToggle) => FieldAccess::Write1ToToggle,
                Some(ModifiedWriteValues::ZeroToClear) => FieldAccess::Write0ToClear,
                Some(m) => return Err(unsupported(format!("modifiedWriteValues={m:?}"))),
            };
            if (read_clears || read_modifies) && w != FieldAccess::ReadWrite {
                return Err(unsupported(
                    "readAction with modifiedWriteValues".to_string(),
                ));
            }
            if read_clears {
                FieldAccess::ReadToClear
            } else if read_modifies {
                FieldAccess::ReadSideEffectWrite
            } else {
                w
            }
        }
    };
    Ok(kind)
}
