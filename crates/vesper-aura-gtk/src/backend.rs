use crate::{EventBatch, GtkError, Payload, UiEvent, validate_transaction, width_request};
use gtk4 as gtk;
use gtk4::{glib, prelude::*};
use std::{
    cell::RefCell,
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, ThreadId},
};
use vesper_aura::{
    Backend, Event, HandlerKey, Kind, Limits, Patch, Property, RecordingBackend, SlotId,
    SubscriptionId, Transaction, UiThread, Value, WidgetId,
};

/// The thread that initialized GTK through this crate, if any.
static GTK_THREAD: OnceLock<ThreadId> = OnceLock::new();
static INIT_LOCK: Mutex<()> = Mutex::new(());
static DROPPED_WHILE_MOUNTED: AtomicU64 = AtomicU64::new(0);

/// Name given to the focus controllers this backend installs, so tests and
/// tooling can find them through `gtk_widget_observe_controllers`.
pub const FOCUS_CONTROLLER_NAME: &str = "aura-focus";

/// Number of [`GtkBackend`]s dropped while still holding a mounted tree. A
/// backend owns native widgets and must be released through
/// [`GtkBackend::close`] after the session unmounts; a dropped owner is
/// counted here (and its window destroyed) rather than released silently.
pub fn dropped_while_mounted() -> u64 {
    DROPPED_WHILE_MOUNTED.load(Ordering::Relaxed)
}

/// Copied, read-only view of one native widget, read back from GTK itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NativeNode {
    /// GObject type name, for example `GtkButton`.
    pub type_name: String,
    /// `GtkLabel:label`, `GtkButton:label` or `GtkEntry:placeholder-text`.
    pub text: Option<String>,
    /// `GtkEditable:text` of an entry.
    pub value: Option<String>,
    pub sensitive: bool,
    pub visible: bool,
    pub width_request: i32,
    /// Names of the controllers this backend installed (focus subscriptions).
    pub aura_controllers: usize,
    /// Children in GTK order. Only `GtkBox` children are walked; the internal
    /// children of buttons and entries are GTK implementation details.
    pub children: Vec<NativeNode>,
}

/// Result of [`GtkBackend::close`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeardownReport {
    /// Native widgets this backend ever created (plus its window).
    pub created: usize,
    /// Of those, how many GTK objects are still alive after teardown. Zero
    /// unless the host retained its own strong references.
    pub still_alive: usize,
}

/// A rejected [`GtkBackend::close`]: the still-live backend and why.
#[derive(Debug)]
pub struct GtkCloseFailure {
    pub backend: GtkBackend,
    pub error: GtkError,
}
impl GtkCloseFailure {
    fn new(backend: GtkBackend, error: GtkError) -> Box<Self> {
        Box::new(Self { backend, error })
    }
}

enum Handle {
    Signal(glib::SignalHandlerId),
    Focus(gtk::EventControllerFocus),
}

struct Subscription {
    widget: WidgetId,
    event: Event,
    handle: Handle,
}

struct Native {
    kind: Kind,
    widget: gtk::Widget,
}

#[derive(Default)]
struct Queue {
    events: VecDeque<UiEvent>,
    capacity: usize,
    close_requested: bool,
    dropped: u64,
}
impl Queue {
    fn push(&mut self, event: UiEvent) {
        if self.events.len() >= self.capacity {
            self.dropped = self.dropped.saturating_add(1);
        } else {
            self.events.push_back(event);
        }
    }
}

/// GTK 4 implementation of the Aura [`Backend`] contract.
///
/// The backend owns one `GtkWindow` whose child is the mounted root. It is
/// `!Send` and `!Sync`:
///
/// ```compile_fail
/// fn send<T: Send>() {}
/// send::<vesper_aura_gtk::GtkBackend>();
/// ```
#[must_use = "a GTK backend owns native widgets; release it with GtkBackend::close"]
pub struct GtkBackend {
    thread: ThreadId,
    shadow: RecordingBackend,
    window: Option<gtk::Window>,
    close_handler: Option<glib::SignalHandlerId>,
    widgets: BTreeMap<WidgetId, Native>,
    slots: BTreeMap<SlotId, (WidgetId, Property)>,
    subscriptions: BTreeMap<SubscriptionId, Subscription>,
    queue: Rc<RefCell<Queue>>,
    tracked: Vec<glib::WeakRef<glib::Object>>,
    created: usize,
    max_text_bytes: usize,
    poisoned: bool,
}

impl std::fmt::Debug for GtkBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GtkBackend")
            .field("widgets", &self.widgets.len())
            .field("subscriptions", &self.subscriptions.len())
            .field("poisoned", &self.poisoned)
            .finish_non_exhaustive()
    }
}

/// Initialize GTK on the current thread, or confirm it already was.
fn claim_gtk_thread() -> Result<ThreadId, GtkError> {
    let current = thread::current().id();
    let _guard = INIT_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(owner) = GTK_THREAD.get() {
        return if *owner == current && gtk::is_initialized_main_thread() {
            Ok(current)
        } else {
            Err(GtkError::NotGtkMainThread)
        };
    }
    if gtk::is_initialized() {
        // Initialized outside this crate.
        return if gtk::is_initialized_main_thread() {
            let _ = GTK_THREAD.set(current);
            Ok(current)
        } else {
            Err(GtkError::NotGtkMainThread)
        };
    }
    gtk::init().map_err(|e| GtkError::Init(e.to_string()))?;
    let _ = GTK_THREAD.set(current);
    Ok(current)
}

impl GtkBackend {
    /// Create a backend on the GTK main thread. The first call initializes GTK
    /// (and needs a display: X11, Wayland or Broadway); a call on any other
    /// thread afterwards is [`GtkError::NotGtkMainThread`].
    ///
    /// `limits` bound the shadow model exactly like
    /// [`RecordingBackend::new`]; `max_patches` also bounds the event queue.
    pub fn new(thread: &UiThread, limits: Limits) -> Result<Self, GtkError> {
        let owner = claim_gtk_thread()?;
        let queue = Rc::new(RefCell::new(Queue {
            capacity: limits.max_patches,
            ..Queue::default()
        }));
        let window = gtk::Window::new();
        window.set_title(Some("Vesper Aura"));
        let close_queue = Rc::clone(&queue);
        let close_handler = window.connect_close_request(move |_| {
            close_queue.borrow_mut().close_requested = true;
            glib::Propagation::Stop
        });
        let tracked = vec![window.upcast_ref::<glib::Object>().downgrade()];
        Ok(Self {
            thread: owner,
            max_text_bytes: limits.max_text_bytes,
            shadow: RecordingBackend::new(thread, limits),
            window: Some(window),
            close_handler: Some(close_handler),
            widgets: BTreeMap::new(),
            slots: BTreeMap::new(),
            subscriptions: BTreeMap::new(),
            queue,
            tracked,
            created: 1,
            poisoned: false,
        })
    }

    fn check(&self, _thread: &UiThread) -> Result<(), GtkError> {
        // `&UiThread` is !Send/!Sync, so it was claimed on the current thread;
        // it remains to check that this is the backend's GTK thread.
        if thread::current().id() != self.thread || !gtk::is_initialized_main_thread() {
            return Err(GtkError::WrongThread);
        }
        if self.poisoned {
            return Err(GtkError::Poisoned);
        }
        Ok(())
    }

    fn window(&self) -> &gtk::Window {
        self.window.as_ref().expect("window lives until close/drop")
    }

    /// Show the window (needed for focus and real rendering under Xvfb).
    pub fn present(&self, thread: &UiThread) -> Result<(), GtkError> {
        self.check(thread)?;
        self.window().present();
        Ok(())
    }

    /// Run pending GLib main-context work without blocking.
    pub fn pump(&self, thread: &UiThread) -> Result<(), GtkError> {
        self.check(thread)?;
        let context = glib::MainContext::default();
        while context.iteration(false) {}
        Ok(())
    }

    /// The backend's toplevel window, for embedding or for driving input in
    /// tests. A retained clone keeps the GTK object alive past teardown.
    pub fn gtk_window(&self, thread: &UiThread) -> Result<gtk::Window, GtkError> {
        self.check(thread)?;
        Ok(self.window().clone())
    }

    /// The live native widget for an Aura widget identity. A retained clone
    /// keeps the GTK object alive past teardown.
    pub fn native_widget(
        &self,
        thread: &UiThread,
        widget: WidgetId,
    ) -> Result<gtk::Widget, GtkError> {
        self.check(thread)?;
        self.widgets
            .get(&widget)
            .map(|n| n.widget.clone())
            .ok_or(GtkError::StaleWidget)
    }

    /// The shadow model's accepted state (same data a `RecordingBackend`
    /// exposes). It always equals what was last replayed into GTK.
    pub fn shadow(&self, thread: &UiThread) -> Result<&RecordingBackend, GtkError> {
        self.check(thread)?;
        Ok(&self.shadow)
    }

    /// Reject the next transaction before patch `patch` (or after all
    /// patches), for testing that rejection leaves GTK untouched.
    pub fn reject_next_at(&mut self, thread: &UiThread, patch: usize) -> Result<(), GtkError> {
        self.check(thread)?;
        self.shadow.reject_next_at(thread, patch)?;
        Ok(())
    }

    /// Drain routed signals. Signals whose subscription was disconnected after
    /// queueing are counted in `stale` and never delivered.
    pub fn drain_events(&self, thread: &UiThread) -> Result<EventBatch, GtkError> {
        self.check(thread)?;
        let mut queue = self.queue.borrow_mut();
        let mut batch = EventBatch {
            close_requested: std::mem::take(&mut queue.close_requested),
            dropped: std::mem::take(&mut queue.dropped),
            ..EventBatch::default()
        };
        for event in queue.events.drain(..) {
            if self.subscriptions.contains_key(&event.subscription) {
                batch.events.push(event);
            } else {
                batch.stale = batch.stale.saturating_add(1);
            }
        }
        Ok(batch)
    }

    /// Read the native widget tree back through GTK's own accessors
    /// (`gtk_window_get_child`, `gtk_widget_get_first_child`, ...).
    pub fn native_tree(&self, thread: &UiThread) -> Result<Option<NativeNode>, GtkError> {
        self.check(thread)?;
        Ok(self.window().child().map(|w| read_native(&w)))
    }

    /// The native tree the shadow model says GTK must contain.
    pub fn expected_tree(&self, thread: &UiThread) -> Result<Option<NativeNode>, GtkError> {
        self.check(thread)?;
        let snapshot = self.shadow.snapshot(thread)?;
        Ok(snapshot.root.map(|root| expected(snapshot, root)))
    }

    /// Check that GTK's widget tree equals the shadow model and that every
    /// live native widget is reachable from the window.
    pub fn audit(&self, thread: &UiThread) -> Result<(), String> {
        let native = self.native_tree(thread).map_err(|e| format!("{e:?}"))?;
        let expected = self.expected_tree(thread).map_err(|e| format!("{e:?}"))?;
        if native != expected {
            return Err(format!(
                "native tree differs\nnative:   {native:#?}\nexpected: {expected:#?}"
            ));
        }
        let reachable = native.as_ref().map_or(0, count);
        if reachable != self.widgets.len() {
            return Err(format!(
                "{} live widgets, {reachable} reachable from the window",
                self.widgets.len()
            ));
        }
        Ok(())
    }

    /// Number of GTK objects created by this backend that are still alive.
    pub fn live_native_objects(&self, thread: &UiThread) -> Result<usize, GtkError> {
        self.check(thread)?;
        Ok(self
            .tracked
            .iter()
            .filter(|w| w.upgrade().is_some())
            .count())
    }

    /// Release the backend after its session unmounted (for example the
    /// backend returned by `Session::shutdown`). Destroys the window and
    /// reports how many native objects survive.
    pub fn close(mut self, thread: &UiThread) -> Result<TeardownReport, Box<GtkCloseFailure>> {
        if let Err(e) = self.check(thread) {
            return Err(GtkCloseFailure::new(self, e));
        }
        let mounted = match self.shadow.snapshot(thread) {
            Ok(s) => s.root.is_some() || !s.widgets.is_empty(),
            Err(e) => return Err(GtkCloseFailure::new(self, e.into())),
        };
        if mounted || !self.widgets.is_empty() || !self.subscriptions.is_empty() {
            return Err(GtkCloseFailure::new(self, GtkError::StillMounted));
        }
        self.release_window();
        let context = glib::MainContext::default();
        while context.iteration(false) {}
        let tracked = std::mem::take(&mut self.tracked);
        Ok(TeardownReport {
            created: self.created,
            still_alive: tracked.iter().filter(|w| w.upgrade().is_some()).count(),
        })
    }

    fn release_window(&mut self) {
        if let Some(window) = self.window.take() {
            if let Some(id) = self.close_handler.take() {
                window.disconnect(id);
            }
            window.set_child(None::<&gtk::Widget>);
            window.destroy();
        }
    }

    fn native(&self, widget: &WidgetId) -> Result<&Native, GtkError> {
        self.widgets
            .get(widget)
            .ok_or(GtkError::Desynchronized("unknown widget"))
    }

    fn container(&self, widget: &WidgetId) -> Result<gtk::Box, GtkError> {
        self.native(widget)?
            .widget
            .clone()
            .downcast::<gtk::Box>()
            .map_err(|_| GtkError::Desynchronized("parent is not a GtkBox"))
    }

    fn replay(&mut self, patch: &Patch) -> Result<(), GtkError> {
        match patch {
            Patch::SetRoot(root) => {
                let child = match root {
                    Some(id) => Some(self.native(id)?.widget.clone()),
                    None => None,
                };
                self.window().set_child(child.as_ref());
            }
            Patch::Create { widget, kind } => {
                let native: gtk::Widget = match kind {
                    Kind::Container => gtk::Box::new(gtk::Orientation::Vertical, 0).upcast(),
                    Kind::Text => gtk::Label::new(None).upcast(),
                    Kind::Button => gtk::Button::new().upcast(),
                    Kind::Input => gtk::Entry::new().upcast(),
                };
                // Keep the tracking list bounded by the live widgets.
                self.tracked.retain(|w| w.upgrade().is_some());
                self.created = self.created.saturating_add(1);
                self.tracked
                    .push(native.upcast_ref::<glib::Object>().downgrade());
                self.widgets.insert(
                    *widget,
                    Native {
                        kind: *kind,
                        widget: native,
                    },
                );
            }
            Patch::Destroy { widget } => {
                let native = self
                    .widgets
                    .remove(widget)
                    .ok_or(GtkError::Desynchronized("destroy unknown widget"))?;
                if native.widget.parent().is_some() {
                    return Err(GtkError::Desynchronized("destroy attached widget"));
                }
                // Dropping the last reference finalizes the GTK widget.
                drop(native);
            }
            Patch::BindSlot {
                widget,
                property,
                slot,
                value,
            } => {
                self.slots.insert(*slot, (*widget, *property));
                self.set_property(widget, *property, Some(value))?;
            }
            Patch::SetSlot { slot, value } => {
                let (widget, property) = *self
                    .slots
                    .get(slot)
                    .ok_or(GtkError::Desynchronized("unknown slot"))?;
                self.set_property(&widget, property, Some(value))?;
            }
            Patch::UnbindSlot {
                widget,
                property,
                slot,
            } => {
                self.slots.remove(slot);
                self.set_property(widget, *property, None)?;
            }
            Patch::Subscribe {
                widget,
                event,
                handler,
                subscription,
            } => {
                let handle = self.connect(widget, *event, *handler, *subscription)?;
                self.subscriptions.insert(
                    *subscription,
                    Subscription {
                        widget: *widget,
                        event: *event,
                        handle,
                    },
                );
            }
            Patch::Disconnect { subscription } => {
                let sub = self
                    .subscriptions
                    .remove(subscription)
                    .ok_or(GtkError::Desynchronized("unknown subscription"))?;
                let widget = self.native(&sub.widget)?.widget.clone();
                match sub.handle {
                    Handle::Signal(id) => widget.disconnect(id),
                    Handle::Focus(controller) => widget.remove_controller(&controller),
                }
            }
            Patch::RemoveChild {
                parent,
                child,
                index,
            } => {
                let container = self.container(parent)?;
                let child = &self.native(child)?.widget;
                if nth_child(&container, *index).as_ref() != Some(child) {
                    return Err(GtkError::Desynchronized("remove child position"));
                }
                container.remove(child);
            }
            Patch::InsertChild {
                parent,
                child,
                index,
            } => {
                let container = self.container(parent)?;
                let child = self.native(child)?.widget.clone();
                if child.parent().is_some() {
                    return Err(GtkError::Desynchronized("insert attached child"));
                }
                let after = match index.checked_sub(1) {
                    None => None,
                    Some(i) => Some(
                        nth_child(&container, i)
                            .ok_or(GtkError::Desynchronized("insert child position"))?,
                    ),
                };
                container.insert_child_after(&child, after.as_ref());
            }
            Patch::MoveChild {
                parent,
                child,
                from,
                to,
            } => {
                let container = self.container(parent)?;
                let child = self.native(child)?.widget.clone();
                if nth_child(&container, *from).as_ref() != Some(&child) {
                    return Err(GtkError::Desynchronized("move child position"));
                }
                let others: Vec<gtk::Widget> =
                    children(&container).filter(|w| *w != child).collect();
                let after = match to.checked_sub(1) {
                    None => None,
                    Some(i) => Some(
                        others
                            .get(i)
                            .cloned()
                            .ok_or(GtkError::Desynchronized("move child position"))?,
                    ),
                };
                container.reorder_child_after(&child, after.as_ref());
            }
        }
        Ok(())
    }

    /// Apply (or with `None`, reset to the GTK default) one property.
    fn set_property(
        &self,
        widget: &WidgetId,
        property: Property,
        value: Option<&Value>,
    ) -> Result<(), GtkError> {
        let native = self.native(widget)?;
        let w = &native.widget;
        let text = |v: Option<&Value>| match v {
            None => Ok(String::new()),
            Some(Value::Text(t)) => Ok(t.clone()),
            Some(_) => Err(GtkError::Desynchronized("text property type")),
        };
        let flag = |v: Option<&Value>| match v {
            None => Ok(true),
            Some(Value::Bool(b)) => Ok(*b),
            Some(_) => Err(GtkError::Desynchronized("bool property type")),
        };
        match (native.kind, property) {
            (_, Property::Enabled) => w.set_sensitive(flag(value)?),
            (_, Property::Visible) => w.set_visible(flag(value)?),
            (_, Property::Width) => w.set_width_request(match value {
                None => -1,
                Some(Value::Int(width)) => width_request(*width)?,
                Some(_) => return Err(GtkError::Desynchronized("width property type")),
            }),
            (Kind::Text, Property::Text) => downcast::<gtk::Label>(w)?.set_text(&text(value)?),
            (Kind::Button, Property::Text) => downcast::<gtk::Button>(w)?.set_label(&text(value)?),
            (Kind::Input, Property::Text) => {
                let placeholder = value.map(|v| text(Some(v))).transpose()?;
                downcast::<gtk::Entry>(w)?.set_placeholder_text(placeholder.as_deref());
            }
            (Kind::Input, Property::Value) => {
                let entry = downcast::<gtk::Entry>(w)?;
                let text = text(value)?;
                // A programmatic value is not a user edit: block this widget's
                // Changed registration while setting it.
                let blocked: Vec<&glib::SignalHandlerId> = self
                    .subscriptions
                    .values()
                    .filter(|s| s.widget == *widget && s.event == Event::Changed)
                    .filter_map(|s| match &s.handle {
                        Handle::Signal(id) => Some(id),
                        Handle::Focus(_) => None,
                    })
                    .collect();
                for id in &blocked {
                    entry.block_signal(id);
                }
                entry.set_text(&text);
                for id in &blocked {
                    entry.unblock_signal(id);
                }
            }
            _ => return Err(GtkError::Desynchronized("property not valid for kind")),
        }
        Ok(())
    }

    fn connect(
        &self,
        widget: &WidgetId,
        event: Event,
        handler: HandlerKey,
        subscription: SubscriptionId,
    ) -> Result<Handle, GtkError> {
        let native = self.native(widget)?;
        let queue = Rc::clone(&self.queue);
        let routed = move |payload| UiEvent {
            subscription,
            handler,
            event,
            payload,
        };
        Ok(match (native.kind, event) {
            (Kind::Button, Event::Click) => {
                let button = downcast::<gtk::Button>(&native.widget)?;
                Handle::Signal(button.connect_clicked(move |_| {
                    queue.borrow_mut().push(routed(Payload::None));
                }))
            }
            (Kind::Input, Event::Changed) => {
                let entry = downcast::<gtk::Entry>(&native.widget)?;
                let max = self.max_text_bytes;
                Handle::Signal(entry.connect_changed(move |entry| {
                    let text = entry.text();
                    let payload = if text.len() > max {
                        Payload::TextOverBudget(text.len())
                    } else {
                        Payload::Text(text.to_string())
                    };
                    queue.borrow_mut().push(routed(payload));
                }))
            }
            (Kind::Button | Kind::Input, Event::Focused) => {
                let controller = gtk::EventControllerFocus::new();
                controller.set_name(Some(FOCUS_CONTROLLER_NAME));
                controller.connect_enter(move |_| {
                    queue.borrow_mut().push(routed(Payload::None));
                });
                native.widget.add_controller(controller.clone());
                Handle::Focus(controller)
            }
            _ => return Err(GtkError::Desynchronized("event not valid for kind")),
        })
    }
}

impl Backend for GtkBackend {
    type Error = GtkError;

    fn apply(&mut self, thread: &UiThread, transaction: &Transaction) -> Result<(), GtkError> {
        self.check(thread)?;
        validate_transaction(transaction)?;
        // Staged validation: the shadow applies all-or-nothing.
        self.shadow.apply(thread, transaction)?;
        for patch in transaction.patches() {
            if let Err(error) = self.replay(patch) {
                self.poisoned = true;
                return Err(error);
            }
        }
        Ok(())
    }
}

impl Drop for GtkBackend {
    fn drop(&mut self) {
        if self.window.is_none() {
            return;
        }
        if !self.widgets.is_empty() {
            DROPPED_WHILE_MOUNTED.fetch_add(1, Ordering::Relaxed);
        }
        // A !Send value is dropped on its own thread, so GTK is usable here.
        if thread::current().id() == self.thread {
            for (_, sub) in std::mem::take(&mut self.subscriptions) {
                if let Some(native) = self.widgets.get(&sub.widget) {
                    match sub.handle {
                        Handle::Signal(id) => native.widget.disconnect(id),
                        Handle::Focus(c) => native.widget.remove_controller(&c),
                    }
                }
            }
            self.release_window();
        }
    }
}

fn downcast<T: IsA<gtk::Widget>>(widget: &gtk::Widget) -> Result<&T, GtkError> {
    widget
        .downcast_ref::<T>()
        .ok_or(GtkError::Desynchronized("widget type"))
}

fn children(container: &gtk::Box) -> impl Iterator<Item = gtk::Widget> {
    std::iter::successors(container.first_child(), |w| w.next_sibling())
}

fn nth_child(container: &gtk::Box, index: usize) -> Option<gtk::Widget> {
    children(container).nth(index)
}

fn aura_controllers(widget: &gtk::Widget) -> usize {
    let list = widget.observe_controllers();
    (0..list.n_items())
        .filter_map(|i| list.item(i))
        .filter_map(|o| o.downcast::<gtk::EventController>().ok())
        .filter(|c| c.name().as_deref() == Some(FOCUS_CONTROLLER_NAME))
        .count()
}

fn read_native(widget: &gtk::Widget) -> NativeNode {
    let (text, value) = if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        (Some(label.text().to_string()), None)
    } else if let Some(button) = widget.downcast_ref::<gtk::Button>() {
        (
            Some(button.label().map(|s| s.to_string()).unwrap_or_default()),
            None,
        )
    } else if let Some(entry) = widget.downcast_ref::<gtk::Entry>() {
        (
            Some(
                entry
                    .placeholder_text()
                    .map(|s| s.to_string())
                    .unwrap_or_default(),
            ),
            Some(entry.text().to_string()),
        )
    } else {
        (None, None)
    };
    let children = match widget.downcast_ref::<gtk::Box>() {
        Some(container) => children(container).map(|c| read_native(&c)).collect(),
        None => Vec::new(),
    };
    NativeNode {
        type_name: widget.type_().name().to_string(),
        text,
        value,
        sensitive: widget.get_sensitive(),
        visible: widget.get_visible(),
        width_request: widget.width_request(),
        aura_controllers: aura_controllers(widget),
        children,
    }
}

fn expected(snapshot: &vesper_aura::RecordingSnapshot, widget: WidgetId) -> NativeNode {
    let node = &snapshot.widgets[&widget];
    let value = |p: Property| node.properties.get(&p).map(|s| &snapshot.slots[s]);
    let text = |p: Property| match value(p) {
        Some(Value::Text(t)) => t.clone(),
        _ => String::new(),
    };
    let flag = |p: Property| !matches!(value(p), Some(Value::Bool(false)));
    let (text_prop, value_prop) = match node.kind {
        Kind::Container => (None, None),
        Kind::Text | Kind::Button => (Some(text(Property::Text)), None),
        Kind::Input => (Some(text(Property::Text)), Some(text(Property::Value))),
    };
    NativeNode {
        type_name: crate::gtk_type_name(node.kind).to_string(),
        text: text_prop,
        value: value_prop,
        sensitive: flag(Property::Enabled),
        visible: flag(Property::Visible),
        width_request: match value(Property::Width) {
            Some(Value::Int(w)) => i32::try_from(*w).unwrap_or(i32::MAX),
            _ => -1,
        },
        aura_controllers: usize::from(node.subscriptions.contains_key(&Event::Focused)),
        children: node
            .children
            .iter()
            .map(|c| expected(snapshot, *c))
            .collect(),
    }
}

fn count(node: &NativeNode) -> usize {
    1 + node.children.iter().map(count).sum::<usize>()
}
