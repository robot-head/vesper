//! The pilot's export ABI v2 library (`vesper/kit.ves`, port wave P1 lane 1,
//! #898): sequences and slices, owned and linear handles, a recursive enum
//! behind boxes, generic instances and a `.vsys` import, plus the five
//! "Patterns for Rust APIs" of `docs/maintenance/porting-a-crate.md`
//! (in [`patterns`]).
//!
//! As for the rest of the pilot, the API has a Rust build
//! (`src/kit/reference.rs`) and a Vesper build (the generated
//! `port/kit.rs`, under `vesper-port`), and `tests/kit.rs` runs unchanged
//! against both.

/// A plan step that [`patterns::broadcast`] applies to its transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    Wait(u64),
    Send { to: u32, bytes: u64 },
}

/// The errors of [`mean`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KitError {
    Empty,
    TooLarge(u64),
}

impl core::fmt::Display for KitError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            KitError::Empty => f.write_str("no values"),
            KitError::TooLarge(total) => write!(f, "total {total} is too large"),
        }
    }
}

impl std::error::Error for KitError {}

/// A tree read out of a [`Tree`] handle, as a plain Rust value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TreeNode {
    Leaf(u64),
    Node(Box<TreeNode>, Box<TreeNode>),
}

pub mod patterns;
pub mod shim;

#[cfg(not(feature = "vesper-port"))]
mod reference;
#[cfg(not(feature = "vesper-port"))]
pub use reference::*;

// Generated and committed, outside src/ (see the porting recipe).
#[cfg(feature = "vesper-port")]
#[rustfmt::skip]
#[path = "../port/kit.rs"]
mod binding;
#[cfg(feature = "vesper-port")]
pub use binding::*;

/// The tree behind a [`Tree`] handle, read through its borrowed view.
#[cfg(feature = "vesper-port")]
pub fn tree_shape(tree: &Tree) -> TreeNode {
    fn read(view: TreeRef<'_>) -> TreeNode {
        match view.view() {
            TreeView::Leaf(v) => TreeNode::Leaf(v),
            TreeView::Node(l, r) => TreeNode::Node(Box::new(read(l)), Box::new(read(r))),
            TreeView::_Borrow(_) => unreachable!(),
        }
    }
    read(tree.as_ref())
}
