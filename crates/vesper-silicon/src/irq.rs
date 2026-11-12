//! Linear `IrqGuard`, a modelled local-CPU interrupt mask, and spinlocks
//! shared with interrupt service routines (§26.1 item 3, §26.2 `CpuIrq`).
//!
//! The CPU here is a software model of one core's interrupt-enable state:
//! DAIF.I on AArch64, RFLAGS.IF on x86_64 and mstatus.MIE on RISC-V. No real
//! interrupt mask is touched. What the model fixes is the protocol:
//!
//! * [`Cpu::disable_local_save`] returns a linear [`IrqGuard`] that holds the
//!   saved flag word; [`Cpu::restore_local`] is its only consuming exit, it
//!   restores exactly that word, and nested guards must be restored in LIFO
//!   order (a wrong-order restore returns the guard with an error).
//! * [`IsrSpinLock::lock`] needs `&IrqGuard`, and the lock guard borrows it,
//!   so the lock is released before interrupts can be re-enabled.
//! * An interrupt raised while masked stays pending and is delivered on the
//!   restore that unmasks, so an ISR never contends with a lock its own core
//!   holds; ISRs lock through [`IsrCtx`], which only exists inside delivery.
//!
//! Re-enabling interrupts while the lock is held does not compile:
//!
//! ```compile_fail
//! # use vesper_silicon::{Arch, irq::*, ledger::Ledger};
//! let mut cpu = Cpu::new(Arch::Aarch64, &Ledger::new());
//! let lock = IsrSpinLock::new(&cpu, 0u32);
//! let g = cpu.disable_local_save();
//! let held = lock.lock(&g).unwrap();
//! cpu.restore_local(g).unwrap(); // `g` is borrowed by `held`
//! drop(held);
//! ```
//!
//! Guards cannot be duplicated:
//!
//! ```compile_fail
//! # use vesper_silicon::{Arch, irq::*, ledger::Ledger};
//! let mut cpu = Cpu::new(Arch::X86_64, &Ledger::new());
//! let g = cpu.disable_local_save();
//! let g2 = g.clone();
//! ```
//!
//! The accepted shape:
//!
//! ```
//! # use vesper_silicon::{Arch, irq::*, ledger::Ledger};
//! let mut cpu = Cpu::new(Arch::Aarch64, &Ledger::new());
//! let lock = IsrSpinLock::new(&cpu, 0u32);
//! let g = cpu.disable_local_save();
//! *lock.lock(&g).unwrap() += 1;
//! cpu.restore_local(g).unwrap();
//! ```

use std::cell::{RefCell, RefMut};
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::Arch;
use crate::ledger::{Ledger, OwnerKind, Token};

static NEXT_CPU: AtomicU64 = AtomicU64::new(1);

/// Bit of the modelled flags word that enables local interrupts. On AArch64
/// the bit is DAIF.I and *set* means masked; elsewhere set means enabled.
pub fn irq_enable_bit(arch: Arch) -> (u64, bool) {
    match arch {
        // DAIF as read by `mrs x0, daif`: D=9, A=8, I=7, F=6. I set = masked.
        Arch::Aarch64 => (1 << 7, false),
        // RFLAGS.IF = bit 9, set = enabled.
        Arch::X86_64 => (1 << 9, true),
        // mstatus.MIE = bit 3, set = enabled.
        Arch::Riscv64 => (1 << 3, true),
    }
}

/// Linear proof that local interrupts are masked on one CPU.
#[must_use = "an IrqGuard must be passed to Cpu::restore_local"]
#[derive(Debug)]
pub struct IrqGuard {
    saved_psr: u64,
    cpu: u64,
    depth: u32,
    token: Token,
}

impl IrqGuard {
    /// The flags word saved when interrupts were masked.
    pub fn saved_psr(&self) -> u64 {
        self.saved_psr
    }

    pub fn cpu_id(&self) -> u64 {
        self.cpu
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IrqError {
    WrongCpu {
        guard_cpu: u64,
        cpu: u64,
    },
    OutOfOrder {
        guard_depth: u32,
        current_depth: u32,
    },
}

/// Context handed to an ISR body. It cannot be constructed outside
/// interrupt delivery and has no suspending operations.
#[derive(Debug)]
pub struct IsrCtx {
    cpu: u64,
    irq: u32,
}

impl IsrCtx {
    pub fn irq(&self) -> u32 {
        self.irq
    }
}

type Isr = Box<dyn FnMut(&IsrCtx)>;

/// One modelled core.
pub struct Cpu {
    id: u64,
    arch: Arch,
    psr: u64,
    depth: u32,
    pending: VecDeque<u32>,
    handlers: Vec<(u32, Isr)>,
    delivered: Vec<u32>,
    ledger: Ledger,
}

impl std::fmt::Debug for Cpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Cpu")
            .field("id", &self.id)
            .field("arch", &self.arch)
            .field("psr", &self.psr)
            .field("depth", &self.depth)
            .field("pending", &self.pending)
            .finish()
    }
}

impl Cpu {
    /// A core with interrupts enabled.
    pub fn new(arch: Arch, ledger: &Ledger) -> Self {
        let (bit, set_enables) = irq_enable_bit(arch);
        Self {
            id: NEXT_CPU.fetch_add(1, Ordering::Relaxed),
            arch,
            psr: if set_enables { bit } else { 0 },
            depth: 0,
            pending: VecDeque::new(),
            handlers: Vec::new(),
            delivered: Vec::new(),
            ledger: ledger.clone(),
        }
    }

    pub fn id(&self) -> u64 {
        self.id
    }

    pub fn psr(&self) -> u64 {
        self.psr
    }

    pub fn interrupts_enabled(&self) -> bool {
        let (bit, set_enables) = irq_enable_bit(self.arch);
        (self.psr & bit != 0) == set_enables
    }

    fn set_enabled(&mut self, on: bool) {
        let (bit, set_enables) = irq_enable_bit(self.arch);
        if on == set_enables {
            self.psr |= bit;
        } else {
            self.psr &= !bit;
        }
    }

    /// Installs the handler for `irq`.
    pub fn register_isr(&mut self, irq: u32, handler: impl FnMut(&IsrCtx) + 'static) {
        self.handlers.retain(|(n, _)| *n != irq);
        self.handlers.push((irq, Box::new(handler)));
    }

    /// The device raises `irq`. Delivered now if unmasked, else held pending.
    pub fn raise(&mut self, irq: u32) {
        self.pending.push_back(irq);
        if self.interrupts_enabled() {
            self.deliver_pending();
        }
    }

    /// IRQs delivered so far, in order.
    pub fn delivered(&self) -> &[u32] {
        &self.delivered
    }

    pub fn pending(&self) -> usize {
        self.pending.len()
    }

    fn deliver_pending(&mut self) {
        while let Some(irq) = self.pending.pop_front() {
            // Hardware masks interrupts for the duration of the handler.
            let saved = self.psr;
            self.set_enabled(false);
            let ctx = IsrCtx { cpu: self.id, irq };
            if let Some((_, h)) = self.handlers.iter_mut().find(|(n, _)| *n == irq) {
                h(&ctx);
            }
            self.delivered.push(irq);
            self.psr = saved;
        }
    }

    /// `CpuIrq::disable_local_save`: mask local interrupts, saving the word.
    pub fn disable_local_save(&mut self) -> IrqGuard {
        let saved = self.psr;
        self.set_enabled(false);
        self.depth += 1;
        IrqGuard {
            saved_psr: saved,
            cpu: self.id,
            depth: self.depth,
            token: self.ledger.open(OwnerKind::IrqGuard),
        }
    }

    /// `CpuIrq::restore_local(sink guard)`: the guard's only consuming exit.
    /// Restores the saved word and delivers interrupts that became unmasked.
    pub fn restore_local(&mut self, guard: IrqGuard) -> Result<(), (IrqGuard, IrqError)> {
        if guard.cpu != self.id {
            let e = IrqError::WrongCpu {
                guard_cpu: guard.cpu,
                cpu: self.id,
            };
            return Err((guard, e));
        }
        if guard.depth != self.depth {
            let e = IrqError::OutOfOrder {
                guard_depth: guard.depth,
                current_depth: self.depth,
            };
            return Err((guard, e));
        }
        let IrqGuard {
            saved_psr, token, ..
        } = guard;
        token.discharge();
        self.depth -= 1;
        self.psr = saved_psr;
        if self.interrupts_enabled() {
            self.deliver_pending();
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LockError {
    /// The lock is already held on this core: spinning would never end.
    WouldDeadlock,
    /// The proof belongs to a different core than the lock's owner core.
    WrongCpu,
}

/// A spinlock whose data an ISR also touches. Thread context must hold an
/// [`IrqGuard`]; ISR context uses its [`IsrCtx`].
#[derive(Debug)]
pub struct IsrSpinLock<T> {
    cpu: u64,
    data: RefCell<T>,
}

/// Held lock. Borrows the proof, so it ends before the proof can be consumed.
pub struct SpinGuard<'a, T> {
    inner: RefMut<'a, T>,
}

impl<T> std::ops::Deref for SpinGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.inner
    }
}

impl<T> std::ops::DerefMut for SpinGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.inner
    }
}

impl<T> IsrSpinLock<T> {
    /// A lock shared between thread context on `cpu` and its ISRs.
    pub fn new(cpu: &Cpu, value: T) -> Self {
        Self {
            cpu: cpu.id,
            data: RefCell::new(value),
        }
    }

    /// Thread-context acquire: requires the interrupt-mask proof.
    pub fn lock<'a>(&'a self, guard: &'a IrqGuard) -> Result<SpinGuard<'a, T>, LockError> {
        if guard.cpu != self.cpu {
            return Err(LockError::WrongCpu);
        }
        self.acquire()
    }

    /// ISR-context acquire.
    pub fn lock_from_isr<'a>(&'a self, ctx: &'a IsrCtx) -> Result<SpinGuard<'a, T>, LockError> {
        if ctx.cpu != self.cpu {
            return Err(LockError::WrongCpu);
        }
        self.acquire()
    }

    fn acquire(&self) -> Result<SpinGuard<'_, T>, LockError> {
        self.data
            .try_borrow_mut()
            .map(|inner| SpinGuard { inner })
            .map_err(|_| LockError::WouldDeadlock)
    }
}
