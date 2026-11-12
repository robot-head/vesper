//! Bounded Aura view-delta runtime core (design §18.2).
//!
//! This is a host-side keyed-tree reconciler and transactional backend contract,
//! not a compiler `View` implementation or a native toolkit. Flat trees are
//! validated for budgets, topology, property types and supported events before
//! planning. Stable keys retain widget, property-slot and subscription identities
//! across reorder/reparent operations; changing a kind replaces that widget.
//! Planning traverses old/new trees and may search child vectors. The emitted
//! patches are sparse; no O(delta), zero-allocation or frame-rate claim is made.
//!
//! [`RecordingBackend`] executes the actual patch contract against retained
//! widget/slot/subscription state. It provides a deterministic test adapter for
//! future retained toolkits and a slot-notification contract for a future Swift
//! host. No GTK/WinUI/SwiftUI, OS event loop or C wire ABI is implemented.
//!
//! [`UiThread`] and owning sessions are !Send/!Sync, confining them to the thread
//! that claimed the token. Toolkit initialization must independently establish
//! that this is its OS main thread. Rust consuming APIs and must-use owners do
//! not establish source-language linear exhaustion: Rust still permits Drop.

mod identity;
mod plan;
mod recording;
mod session;
mod view;

pub use identity::{SlotId, SubscriptionId, UiThread, WidgetId, WrongThread};
pub use plan::{Patch, Transaction};
pub use recording::{
    BackendError, RecordedEvent, RecordingBackend, RecordingSnapshot, WidgetSnapshot,
};
pub use session::{
    Backend, CloseAction, CloseDecision, CloseError, CloseFailure, CloseOutcome, CloseRequestError,
    CommitError, CommitFailure, CommitReport, InitError, InitFailure, PrepareError, PrepareFailure,
    PreparedUpdate, Session, ShutdownError, ShutdownFailure,
};
pub use view::{Event, HandlerKey, Key, Kind, Limits, Node, Property, Value, ViewError, ViewTree};
