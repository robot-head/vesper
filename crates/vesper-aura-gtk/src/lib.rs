//! GTK 4 retained backend for the Aura transactional view runtime (design §18).
//!
//! [`GtkBackend`] (feature `gtk`) implements [`vesper_aura::Backend`]: it
//! creates, updates, reorders and tears down real GTK 4 widgets from Aura
//! reconciliation plans and routes GTK signals back to the embedding host as
//! [`UiEvent`]s carrying the subscription's [`vesper_aura::HandlerKey`].
//!
//! Atomicity: GTK has no transactional widget API, so every transaction is
//! first validated in full against a private [`vesper_aura::RecordingBackend`]
//! shadow (the same staged checker the core ships) plus the GTK-specific
//! checks in [`validate_transaction`]. Only a transaction the shadow accepted
//! is replayed against GTK, and none of the GTK calls used can fail on an
//! accepted state. A rejected transaction therefore leaves the native tree
//! untouched. If the native tree ever disagreed with the shadow during replay
//! (a bug, not an input error), the backend poisons itself and rejects every
//! later call with [`GtkError::Poisoned`] instead of continuing.
//!
//! Thread discipline: GTK must only be used from the thread that initialized it
//! and owns the default GLib main context. [`GtkBackend::new`] initializes GTK
//! on the calling thread or, if another thread already did, fails with
//! [`GtkError::NotGtkMainThread`]. Every later call re-checks the current
//! thread and returns [`GtkError::WrongThread`] instead of touching GTK. The
//! backend is `!Send`/`!Sync`, so safe Rust cannot even move it to another
//! thread. On Linux GTK does not require the process's first thread, so this
//! attests "the GTK main thread", not "the OS main thread".
//!
//! The pure helpers in this module are always compiled so the crate builds on
//! hosts without GTK (macOS, Windows, hermetic Bazel); the backend itself needs
//! the `gtk` feature and a system GTK 4.
//!
//! WinUI 3 and SwiftUI backends remain deferred.

#![forbid(unsafe_code)]

use vesper_aura::{BackendError, Kind, Patch, SubscriptionId, Transaction, Value};

#[cfg(feature = "gtk")]
mod backend;
#[cfg(feature = "gtk")]
pub use backend::{
    FOCUS_CONTROLLER_NAME, GtkBackend, GtkCloseFailure, NativeNode, TeardownReport,
    dropped_while_mounted,
};
/// The gtk4-rs version this backend links, for hosts that drive its widgets.
#[cfg(feature = "gtk")]
pub use gtk4;

/// Typed failures of the GTK backend. None of them leaves a partially applied
/// transaction behind except [`GtkError::Desynchronized`], which poisons it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GtkError {
    /// Called from a thread other than the GTK main thread that owns this
    /// backend. Nothing was touched.
    WrongThread,
    /// GTK was already initialized on a different thread.
    NotGtkMainThread,
    /// `gtk_init_check` failed, for example because no display is available.
    Init(String),
    /// The shadow model rejected the transaction; the native tree is unchanged.
    Rejected(BackendError),
    /// A value GTK cannot represent, for example a width above `i32::MAX`.
    Unsupported(&'static str),
    /// The native tree disagreed with the shadow mid-replay. The backend is
    /// poisoned; this indicates a bug in this crate, never an input error.
    Desynchronized(&'static str),
    /// An earlier [`GtkError::Desynchronized`] poisoned this backend.
    Poisoned,
    /// The widget or subscription is not live in this backend.
    StaleWidget,
    /// The backend still holds a mounted tree; unmount through the session.
    StillMounted,
}

impl From<BackendError> for GtkError {
    fn from(error: BackendError) -> Self {
        GtkError::Rejected(error)
    }
}

/// The GTK type used for each Aura kind.
pub fn gtk_type_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Container => "GtkBox",
        Kind::Text => "GtkLabel",
        Kind::Button => "GtkButton",
        Kind::Input => "GtkEntry",
    }
}

/// Convert an Aura `Width` (nonnegative i64) to GTK's `width-request` (i32).
pub fn width_request(width: i64) -> Result<i32, GtkError> {
    if width < 0 {
        return Err(GtkError::Unsupported("negative width"));
    }
    i32::try_from(width).map_err(|_| GtkError::Unsupported("width exceeds i32::MAX"))
}

/// GTK-specific checks that the core contract does not impose. They run before
/// the shadow model so a rejected transaction changes nothing.
pub fn validate_transaction(transaction: &Transaction) -> Result<(), GtkError> {
    for patch in transaction.patches() {
        let value = match patch {
            Patch::BindSlot { value, .. } | Patch::SetSlot { value, .. } => value,
            _ => continue,
        };
        match value {
            // `Width` is the only integer-valued property in the core grammar.
            Value::Int(width) => {
                width_request(*width)?;
            }
            // GTK's string setters take NUL-terminated C strings.
            Value::Text(text) if text.contains('\0') => {
                return Err(GtkError::Unsupported("text contains NUL"));
            }
            _ => {}
        }
    }
    Ok(())
}

/// What a routed GTK signal carries back to the host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Payload {
    None,
    /// The entry text at signal time.
    Text(String),
    /// The entry text exceeded the backend's `max_text_bytes`; only its length
    /// in bytes is reported.
    TextOverBudget(usize),
}

/// One GTK signal routed to the host callback identity of a live subscription.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiEvent {
    pub subscription: SubscriptionId,
    pub handler: vesper_aura::HandlerKey,
    pub event: vesper_aura::Event,
    pub payload: Payload,
}

/// A drained batch of routed signals.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EventBatch {
    pub events: Vec<UiEvent>,
    /// The window's close button (or `gtk_window_close`) was pressed. The
    /// window stays open: the host decides through `Session::request_close`.
    pub close_requested: bool,
    /// Signals dropped because the bounded queue was full.
    pub dropped: u64,
    /// Queued signals whose subscription was disconnected before draining;
    /// they are never delivered.
    pub stale: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_conversion_is_checked() {
        assert_eq!(width_request(0), Ok(0));
        assert_eq!(width_request(i32::MAX as i64), Ok(i32::MAX));
        assert_eq!(
            width_request(i32::MAX as i64 + 1),
            Err(GtkError::Unsupported("width exceeds i32::MAX"))
        );
        assert!(width_request(-1).is_err());
    }

    #[test]
    fn embedded_nul_and_wide_widths_are_rejected_before_any_backend() {
        use vesper_aura::{
            Key, Limits, Node, Property, RecordingBackend, Session, UiThread, ViewTree,
        };
        let thread = UiThread::claim_current();
        let limits = Limits::default();
        let mut session = Session::new(
            &thread,
            RecordingBackend::new(&thread, limits.clone()),
            limits,
        )
        .unwrap();
        let mut check = |value: Value, property: Property| {
            let kind = if property == Property::Width {
                Kind::Container
            } else {
                Kind::Text
            };
            let tree = ViewTree::new(
                Key(0),
                vec![Node::new(Key(0), kind).property(property, value)],
            );
            let update = session.prepare(&thread, Some(tree)).unwrap();
            validate_transaction(update.transaction())
        };
        assert_eq!(check(Value::Text("ok".into()), Property::Text), Ok(()));
        assert_eq!(
            check(Value::Text("a\0b".into()), Property::Text),
            Err(GtkError::Unsupported("text contains NUL"))
        );
        assert_eq!(check(Value::Int(7), Property::Width), Ok(()));
        assert!(check(Value::Int(i64::MAX), Property::Width).is_err());
    }

    #[test]
    fn every_kind_has_a_gtk_type() {
        for kind in [Kind::Container, Kind::Text, Kind::Button, Kind::Input] {
            assert!(gtk_type_name(kind).starts_with("Gtk"));
        }
    }
}
