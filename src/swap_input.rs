//! Swap inputs that cross once per compilation (self-hosting SH4 lane 2,
//! #1276).
//!
//! A swapped Vesper module cannot read the compiler's Rust data, so every
//! swap encodes its input (the checked [`World`], the Place IR) into words.
//! Encoding per call is fine for a hook that runs once per compilation, and
//! ruinous for one that runs per type or per function: the whole layout
//! (`Layout::of`) is called thousands of times per build, each with the
//! same world.
//!
//! This module gives every installed hook a place to keep what it has
//! already sent: a [`Scope`] borrows the world for one compilation (the
//! native path opens one around each of its pipelines), and [`with`] hands a
//! hook its own per-scope slot, keyed by the slot's Rust type. A hook stores
//! there whatever it wants to keep (its encoder's interning tables, the
//! Vesper value it decoded once); the slots are dropped, in order, when the
//! outermost scope of that world closes.
//!
//! Soundness: a slot is found by the world's address. The [`Scope`] holds a
//! shared borrow of the world for as long as it lives, so while a scope is
//! open, the world at that address cannot be mutated, moved or freed (it has
//! no interior mutability), and the slot's data stays a description of it.
//! The address is only compared, never dereferenced. Outside a scope (the
//! checker, the evaluator, tests calling a pass directly) [`with`] returns
//! `None` and the hook encodes per call as before.
//!
//! The default compiler installs no hook and never calls [`enable`], so
//! [`open`] does nothing but read one atomic flag, and the default
//! compiler's objects are unchanged. The verified backend never opens a
//! scope (the Stage 1 guard).
use crate::check::World;
use std::any::{Any, TypeId};
use std::cell::RefCell;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};

static ENABLED: AtomicBool = AtomicBool::new(false);

/// Turns scopes on for the rest of the process. The hybrid build calls it
/// when it installs its hooks; nothing in the default compiler does.
pub fn enable() {
    ENABLED.store(true, Relaxed);
}

/// Whether [`enable`] was called.
pub fn enabled() -> bool {
    ENABLED.load(Relaxed)
}

/// One open world: its address, its own id, how many [`Scope`]s hold it,
/// and the slots.
struct Frame {
    world: *const World,
    id: u64,
    depth: usize,
    slots: Vec<(TypeId, Option<Box<dyn Any>>)>,
}

thread_local! {
    static FRAMES: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
    static NEXT_ID: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// One compilation's borrow of its world. While it lives, [`with`] finds
/// the world's slots; the outermost scope of a world drops them.
#[must_use = "the scope closes when it is dropped"]
pub struct Scope<'w> {
    /// The id of the frame this scope holds, `None` when scopes are off.
    frame: Option<u64>,
    _world: PhantomData<&'w World>,
}

/// Opens a scope for `world` (nested scopes of the same world share its
/// slots). Does nothing unless [`enable`] was called.
pub fn open(world: &World) -> Scope<'_> {
    if !enabled() {
        return Scope {
            frame: None,
            _world: PhantomData,
        };
    }
    let key = world as *const World;
    let id = FRAMES.with_borrow_mut(|frames| match frames.last_mut() {
        Some(top) if top.world == key => {
            top.depth += 1;
            top.id
        }
        _ => {
            let id = NEXT_ID.get();
            NEXT_ID.set(id + 1);
            frames.push(Frame {
                world: key,
                id,
                depth: 1,
                slots: vec![],
            });
            id
        }
    });
    Scope {
        frame: Some(id),
        _world: PhantomData,
    }
}

impl Drop for Scope<'_> {
    fn drop(&mut self) {
        let Some(id) = self.frame else {
            return;
        };
        // Close this scope's own frame, wherever it is: scopes of different
        // worlds may be dropped out of order, and closing the top frame
        // instead would leave this world's slots findable after its borrow
        // ended. Take the closed frame out before dropping its slots: a
        // slot's drop (freeing a Vesper value) must not find the frame list
        // borrowed.
        let closed = FRAMES.with_borrow_mut(|frames| {
            let at = frames.iter().rposition(|f| f.id == id)?;
            frames[at].depth -= 1;
            if frames[at].depth == 0 {
                Some(frames.remove(at))
            } else {
                None
            }
        });
        if let Some(frame) = closed {
            for (_, slot) in frame.slots {
                drop(slot);
            }
        }
    }
}

/// Whether a scope of `world` is open on this thread.
pub fn is_open(world: &World) -> bool {
    let key = world as *const World;
    enabled() && FRAMES.with_borrow(|frames| frames.iter().any(|f| f.world == key))
}

/// Runs `f` on this scope's slot of type `T` for `world`, made by `init` on
/// first use. `None` when no scope of `world` is open (the caller encodes
/// per call), or when the slot is already in use further up this thread's
/// stack (a re-entrant call; it also falls back).
pub fn with<T: Any, R>(
    world: &World,
    init: impl FnOnce() -> T,
    f: impl FnOnce(&mut T) -> R,
) -> Option<R> {
    if !enabled() {
        return None;
    }
    let key = world as *const World;
    let id = TypeId::of::<T>();
    // Take the slot out while `f` runs (an entry left `None` marks it busy),
    // so a re-entrant call falls back instead of aliasing it.
    let taken: Option<Box<dyn Any>> = FRAMES.with_borrow_mut(|frames| {
        let frame = frames.iter_mut().rev().find(|f| f.world == key)?;
        match frame.slots.iter_mut().find(|(t, _)| *t == id) {
            Some((_, slot)) => slot.take().map(Some),
            None => {
                frame.slots.push((id, None));
                Some(None)
            }
        }
    })?;
    let mut value = taken.unwrap_or_else(|| Box::new(init()));
    let result = f(value.downcast_mut::<T>().expect("slot of its own type"));
    // Put it back. The frame outlives `f`: its scope borrows the world `f`
    // was given, so it closes only after this call returns.
    FRAMES.with_borrow_mut(|frames| {
        if let Some(frame) = frames.iter_mut().rev().find(|f| f.world == key)
            && let Some((_, slot)) = frame.slots.iter_mut().find(|(t, _)| *t == id)
        {
            *slot = Some(value);
        }
    });
    Some(result)
}
