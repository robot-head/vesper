//! MicroVM device models: the VirtIO MMIO transport, virtio-blk, virtio-net,
//! virtio-vsock and the VM Generation ID device (§37.1.1, §37.1.4).
//!
//! These are host models tested against the device specifications without
//! KVM: a driver is simulated by writing transport registers and packed-ring
//! descriptors into [`crate::guest_mem::GuestMemory`]. They are not wired to a
//! running KVM guest (no ioeventfd/irqfd), and Rust ownership of
//! [`mmio::Chain`] is not evidence that the Vesper `VirtqDescLease` typing
//! rules are implemented.
pub mod block;
pub mod mmio;
pub mod net;
pub mod vmgenid;
pub mod vsock;
