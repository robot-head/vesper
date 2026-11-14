use crate::{
    EventBatch, GtkError, Payload, UiEvent,
    image::RgbaImage,
    style::{Layout, Style, grid_cell},
    validate_transaction, width_request,
};
use gtk4 as gtk;
use gtk4::{glib, graphene, prelude::*};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, VecDeque},
    rc::Rc,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    thread::{self, ThreadId},
    time::{Duration, Instant},
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

/// Widget name of the scrolled window that hosts a virtual list inside an
/// Aura container.
pub const VIRTUAL_LIST_NAME: &str = "aura-virtual-list";

/// Most rows a virtual list may have. The model holds one small
/// `GtkStringObject` per row; row widgets are created only for visible rows.
pub const MAX_VIRTUAL_ROWS: u32 = 1_000_000;

/// Largest stylesheet accepted by [`GtkBackend::load_css`].
pub const MAX_CSS_BYTES: usize = 64 * 1024;

/// CSS classes GTK itself adds to the widgets this backend creates. They are
/// left out of [`NativeNode::css_classes`], which reports host classes only.
pub const GTK_INTRINSIC_CLASSES: &[&str] =
    &["text-button", "image-button", "vertical", "horizontal"];

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
    /// The layout manager of a container, read back from GTK.
    pub layout: Option<NativeLayout>,
    /// `GtkGridLayoutChild` column and row, when the parent uses a grid.
    pub grid_cell: Option<(i32, i32)>,
    /// Host CSS classes (sorted; GTK's own classes are left out).
    pub css_classes: Vec<String>,
    /// Row count of a virtual list hosted in this container.
    pub virtual_rows: Option<u32>,
    /// Children in GTK order. Only `GtkBox` children are walked; the internal
    /// children of buttons and entries are GTK implementation details, and a
    /// hosted virtual list is reported through `virtual_rows`.
    pub children: Vec<NativeNode>,
}

/// A container's layout manager as GTK reports it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum NativeLayout {
    /// `GtkBoxLayout`.
    Box { horizontal: bool, spacing: i32 },
    /// `GtkGridLayout`.
    Grid {
        row_spacing: i32,
        column_spacing: i32,
    },
    /// Any other layout manager (never installed by this crate).
    Other(String),
}

impl NativeLayout {
    fn of(layout: Layout) -> Self {
        match layout {
            Layout::VStack { spacing } => NativeLayout::Box {
                horizontal: false,
                spacing: i32::from(spacing),
            },
            Layout::HStack { spacing } => NativeLayout::Box {
                horizontal: true,
                spacing: i32::from(spacing),
            },
            Layout::Grid {
                row_spacing,
                column_spacing,
                ..
            } => NativeLayout::Grid {
                row_spacing: i32::from(row_spacing),
                column_spacing: i32::from(column_spacing),
            },
        }
    }
}

/// Counters of one virtual list's row recycling.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct VirtualListStats {
    pub rows: u32,
    /// Row widgets GTK asked the factory to create (`setup`).
    pub row_widgets: u64,
    /// Rows currently bound to a widget.
    pub bound: u64,
    /// Total `bind` calls (each fetches the row's text from the host).
    pub binds: u64,
    /// Largest row index ever bound.
    pub max_bound_index: Option<u32>,
}

/// Rows of a virtual list are fetched from the host at bind time.
pub type RowText = Rc<dyn Fn(u32) -> String>;

struct Island {
    scrolled: gtk::ScrolledWindow,
    list: gtk::ListView,
    model: gtk::StringList,
    stats: Rc<RefCell<VirtualListStats>>,
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
    decor: BTreeMap<WidgetId, Style>,
    islands: BTreeMap<WidgetId, Island>,
    css: Vec<gtk::CssProvider>,
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
            decor: BTreeMap::new(),
            islands: BTreeMap::new(),
            css: Vec::new(),
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
        Ok(snapshot
            .root
            .map(|root| self.expected(snapshot, root, None)))
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

    /// The decorations last applied to `widget` (default if none).
    pub fn style(&self, thread: &UiThread, widget: WidgetId) -> Result<Style, GtkError> {
        self.check(thread)?;
        if !self.widgets.contains_key(&widget) {
            return Err(GtkError::StaleWidget);
        }
        Ok(self.decor.get(&widget).cloned().unwrap_or_default())
    }

    /// Apply a layout and CSS classes to a live widget. A layout is only
    /// valid on a container. Validation happens before any GTK change.
    pub fn set_style(
        &mut self,
        thread: &UiThread,
        widget: WidgetId,
        style: Style,
    ) -> Result<(), GtkError> {
        self.check(thread)?;
        let style = style.validate()?;
        let native = self.widgets.get(&widget).ok_or(GtkError::StaleWidget)?;
        if style.layout.is_some() && native.kind != Kind::Container {
            return Err(GtkError::Unsupported("layout on a non-container"));
        }
        let w = native.widget.clone();
        let old = self.decor.get(&widget).cloned().unwrap_or_default();
        for class in &old.classes {
            if !style.classes.contains(class) {
                w.remove_css_class(class);
            }
        }
        for class in &style.classes {
            w.add_css_class(class);
        }
        if native.kind == Kind::Container {
            let container = downcast::<gtk::Box>(&w)?;
            install_layout(container, style.layout.unwrap_or_default());
        }
        if style == Style::default() {
            self.decor.remove(&widget);
        } else {
            self.decor.insert(widget, style);
        }
        self.relayout(&widget);
        Ok(())
    }

    /// Recompute grid cells of one container (no-op for stacks).
    fn relayout(&self, widget: &WidgetId) {
        let Some(Layout::Grid { columns, .. }) = self.decor.get(widget).and_then(|s| s.layout)
        else {
            return;
        };
        let Some(native) = self.widgets.get(widget) else {
            return;
        };
        let Some(manager) = native.widget.layout_manager() else {
            return;
        };
        let Some(container) = native.widget.downcast_ref::<gtk::Box>() else {
            return;
        };
        for (i, child) in children(container).enumerate() {
            if let (Ok((col, row)), Ok(cell)) = (
                grid_cell(i, columns),
                manager
                    .layout_child(&child)
                    .downcast::<gtk::GridLayoutChild>(),
            ) {
                cell.set_column(col);
                cell.set_row(row);
            }
        }
    }

    /// Load a stylesheet for this backend's display at user priority.
    /// Any parse error rejects the whole stylesheet (nothing stays loaded).
    pub fn load_css(&mut self, thread: &UiThread, css: &str) -> Result<(), GtkError> {
        self.check(thread)?;
        if css.len() > MAX_CSS_BYTES {
            return Err(GtkError::Unsupported("stylesheet too large"));
        }
        if css.contains('\0') {
            return Err(GtkError::Unsupported("text contains NUL"));
        }
        let display = gtk::prelude::WidgetExt::display(self.window());
        let provider = gtk::CssProvider::new();
        let errors = Rc::new(RefCell::new(Vec::<String>::new()));
        let sink = Rc::clone(&errors);
        let handler = provider.connect_parsing_error(move |_, section, error| {
            let start = section.start_location();
            sink.borrow_mut().push(format!(
                "{}:{}: {}",
                start.lines() + 1,
                start.line_chars() + 1,
                error
            ));
        });
        provider.load_from_string(css);
        provider.disconnect(handler);
        let errors = errors.take();
        if !errors.is_empty() {
            return Err(GtkError::Css(errors));
        }
        gtk::style_context_add_provider_for_display(
            &display,
            &provider,
            gtk::STYLE_PROVIDER_PRIORITY_USER,
        );
        self.css.push(provider);
        Ok(())
    }

    /// Remove every stylesheet loaded through [`GtkBackend::load_css`].
    pub fn clear_css(&mut self, thread: &UiThread) -> Result<(), GtkError> {
        self.check(thread)?;
        self.remove_css();
        Ok(())
    }

    fn remove_css(&mut self) {
        if let Some(window) = &self.window {
            let display = gtk::prelude::WidgetExt::display(window);
            for provider in self.css.drain(..) {
                gtk::style_context_remove_provider_for_display(&display, &provider);
            }
        }
    }

    /// Host a virtualized `GtkListView` with `rows` rows inside an Aura
    /// container that has no Aura children. Row widgets are `GtkLabel`s
    /// created only for visible rows and recycled while scrolling; their text
    /// is fetched from `row_text` at bind time. While the list is attached,
    /// a transaction inserting Aura children into that container is rejected
    /// before any change. Destroying the container detaches the list.
    pub fn attach_virtual_list(
        &mut self,
        thread: &UiThread,
        widget: WidgetId,
        rows: u32,
        row_text: RowText,
    ) -> Result<(), GtkError> {
        self.check(thread)?;
        if rows > MAX_VIRTUAL_ROWS {
            return Err(GtkError::Unsupported("too many virtual rows"));
        }
        let native = self.widgets.get(&widget).ok_or(GtkError::StaleWidget)?;
        if native.kind != Kind::Container {
            return Err(GtkError::Unsupported("virtual list outside a container"));
        }
        if self.islands.contains_key(&widget) {
            return Err(GtkError::Unsupported(
                "container already hosts a virtual list",
            ));
        }
        let container = downcast::<gtk::Box>(&native.widget)?.clone();
        if container.first_child().is_some() {
            return Err(GtkError::Unsupported("container has Aura children"));
        }
        let stats = Rc::new(RefCell::new(VirtualListStats {
            rows,
            ..VirtualListStats::default()
        }));
        let factory = gtk::SignalListItemFactory::new();
        let setup_stats = Rc::clone(&stats);
        factory.connect_setup(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let label = gtk::Label::new(None);
            label.set_xalign(0.0);
            item.set_child(Some(&label));
            setup_stats.borrow_mut().row_widgets += 1;
        });
        let bind_stats = Rc::clone(&stats);
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(label) = item.child().and_then(|c| c.downcast::<gtk::Label>().ok()) else {
                return;
            };
            let index = item.position();
            label.set_text(&row_text(index));
            let mut s = bind_stats.borrow_mut();
            s.bound += 1;
            s.binds += 1;
            s.max_bound_index = Some(s.max_bound_index.map_or(index, |m| m.max(index)));
        });
        let unbind_stats = Rc::clone(&stats);
        factory.connect_unbind(move |_, _| {
            let mut s = unbind_stats.borrow_mut();
            s.bound = s.bound.saturating_sub(1);
        });
        let model = gtk::StringList::new(&[]);
        splice_rows(&model, rows);
        let selection = gtk::NoSelection::new(Some(model.clone()));
        let list = gtk::ListView::new(Some(selection), Some(factory));
        let scrolled = gtk::ScrolledWindow::new();
        scrolled.set_widget_name(VIRTUAL_LIST_NAME);
        scrolled.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);
        scrolled.set_vexpand(true);
        scrolled.set_hexpand(true);
        scrolled.set_child(Some(&list));
        container.append(&scrolled);
        for object in [scrolled.upcast_ref::<glib::Object>(), list.upcast_ref()] {
            self.tracked.push(object.downgrade());
        }
        self.created = self.created.saturating_add(2);
        self.islands.insert(
            widget,
            Island {
                scrolled,
                list,
                model,
                stats,
            },
        );
        Ok(())
    }

    /// Change the row count of an attached virtual list.
    pub fn set_virtual_rows(
        &mut self,
        thread: &UiThread,
        widget: WidgetId,
        rows: u32,
    ) -> Result<(), GtkError> {
        self.check(thread)?;
        if rows > MAX_VIRTUAL_ROWS {
            return Err(GtkError::Unsupported("too many virtual rows"));
        }
        let island = self.islands.get(&widget).ok_or(GtkError::StaleWidget)?;
        splice_rows(&island.model, rows);
        island.stats.borrow_mut().rows = rows;
        Ok(())
    }

    /// Scroll an attached virtual list so that `row` is visible.
    pub fn scroll_virtual_list(
        &self,
        thread: &UiThread,
        widget: WidgetId,
        row: u32,
    ) -> Result<(), GtkError> {
        self.check(thread)?;
        let island = self.islands.get(&widget).ok_or(GtkError::StaleWidget)?;
        if row >= island.model.n_items() {
            return Err(GtkError::Unsupported("row out of range"));
        }
        island
            .list
            .scroll_to(row, gtk::ListScrollFlags::NONE, None::<gtk::ScrollInfo>);
        Ok(())
    }

    /// Row recycling counters of an attached virtual list.
    pub fn virtual_list_stats(
        &self,
        thread: &UiThread,
        widget: WidgetId,
    ) -> Result<VirtualListStats, GtkError> {
        self.check(thread)?;
        let island = self.islands.get(&widget).ok_or(GtkError::StaleWidget)?;
        Ok(*island.stats.borrow())
    }

    /// The row labels GTK currently has bound, in row order, read back from
    /// the list view's widget tree.
    pub fn visible_rows(
        &self,
        thread: &UiThread,
        widget: WidgetId,
    ) -> Result<Vec<String>, GtkError> {
        self.check(thread)?;
        let island = self.islands.get(&widget).ok_or(GtkError::StaleWidget)?;
        let mut labels = Vec::new();
        collect_labels(island.list.upcast_ref(), &mut labels);
        Ok(labels)
    }

    /// Present the window and pump GLib until GTK has drawn at least
    /// `frames` new frames, or fail with [`GtkError::Timeout`].
    pub fn settle(
        &self,
        thread: &UiThread,
        frames: i64,
        timeout: Duration,
    ) -> Result<(), GtkError> {
        self.check(thread)?;
        let window = self.window();
        window.present();
        let context = glib::MainContext::default();
        let deadline = Instant::now() + timeout;
        let start = Cell::new(None);
        loop {
            // Request a redraw each round: the frame clock only advances when
            // something asks for a frame.
            window.queue_draw();
            while context.iteration(false) {}
            if let Some(clock) = window.frame_clock() {
                let now = clock.frame_counter();
                let first = *start.get().get_or_insert(now);
                start.set(Some(first));
                if window.is_mapped() && now - first >= frames {
                    return Ok(());
                }
            }
            if Instant::now() >= deadline {
                return Err(GtkError::Timeout("no frame drawn"));
            }
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Render the mounted root widget at its allocated size through the
    /// window's own GSK renderer and download it as unpremultiplied RGBA8.
    pub fn screenshot(&self, thread: &UiThread) -> Result<RgbaImage, GtkError> {
        self.check(thread)?;
        let root = self
            .window()
            .child()
            .ok_or(GtkError::Unsupported("nothing mounted"))?;
        if !root.is_mapped() {
            return Err(GtkError::Unsupported("root is not mapped"));
        }
        let (width, height) = (root.width(), root.height());
        if width <= 0 || height <= 0 {
            return Err(GtkError::Unsupported("root has no allocation"));
        }
        let paintable = gtk::WidgetPaintable::new(Some(&root));
        let snapshot = gtk::Snapshot::new();
        paintable.snapshot(&snapshot, f64::from(width), f64::from(height));
        let node = snapshot
            .to_node()
            .ok_or(GtkError::Unsupported("root rendered nothing"))?;
        let renderer = root
            .native()
            .and_then(|n| n.renderer())
            .ok_or(GtkError::Unsupported("no renderer"))?;
        let texture = renderer.render_texture(
            node,
            Some(&graphene::Rect::new(0.0, 0.0, width as f32, height as f32)),
        );
        crate::image::download(&texture).map_err(|_| GtkError::Unsupported("texture download"))
    }

    fn release_window(&mut self) {
        self.remove_css();
        for (widget, island) in std::mem::take(&mut self.islands) {
            if let Some(native) = self.widgets.get(&widget)
                && let Some(container) = native.widget.downcast_ref::<gtk::Box>()
            {
                container.remove(&island.scrolled);
            }
        }
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
                self.decor.remove(widget);
                if let Some(island) = self.islands.remove(widget)
                    && let Some(container) = native.widget.downcast_ref::<gtk::Box>()
                {
                    container.remove(&island.scrolled);
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
                let entry = downcast::<gtk::Entry>(w)?;
                entry.set_placeholder_text(placeholder.as_deref());
                // The placeholder is the entry's only visible description:
                // expose it as the accessible name (AT-SPI `Name`).
                match placeholder.as_deref() {
                    Some(name) if !name.is_empty() => {
                        entry.update_property(&[gtk::accessible::Property::Label(name)]);
                    }
                    _ => entry.reset_property(gtk::AccessibleProperty::Label),
                }
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

impl GtkBackend {
    fn expected(
        &self,
        snapshot: &vesper_aura::RecordingSnapshot,
        widget: WidgetId,
        grid_cell: Option<(i32, i32)>,
    ) -> NativeNode {
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
        let style = self.decor.get(&widget).cloned().unwrap_or_default();
        let layout = style.layout.unwrap_or_default();
        let cell = |i: usize| match layout {
            Layout::Grid { columns, .. } => crate::style::grid_cell(i, columns).ok(),
            _ => None,
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
            layout: (node.kind == Kind::Container).then(|| NativeLayout::of(layout)),
            grid_cell,
            css_classes: style.classes,
            virtual_rows: self.islands.get(&widget).map(|i| i.stats.borrow().rows),
            children: node
                .children
                .iter()
                .enumerate()
                .map(|(i, c)| self.expected(snapshot, *c, cell(i)))
                .collect(),
        }
    }
}

impl Backend for GtkBackend {
    type Error = GtkError;

    fn apply(&mut self, thread: &UiThread, transaction: &Transaction) -> Result<(), GtkError> {
        self.check(thread)?;
        validate_transaction(transaction)?;
        for patch in transaction.patches() {
            if let Patch::InsertChild { parent, .. } = patch
                && self.islands.contains_key(parent)
            {
                return Err(GtkError::Unsupported("container hosts a virtual list"));
            }
        }
        // Staged validation: the shadow applies all-or-nothing.
        self.shadow.apply(thread, transaction)?;
        let mut touched = Vec::new();
        for patch in transaction.patches() {
            if let Patch::InsertChild { parent, .. }
            | Patch::RemoveChild { parent, .. }
            | Patch::MoveChild { parent, .. } = patch
            {
                touched.push(*parent);
            }
            if let Err(error) = self.replay(patch) {
                self.poisoned = true;
                return Err(error);
            }
        }
        touched.sort();
        touched.dedup();
        for parent in &touched {
            self.relayout(parent);
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
    let manager = widget.layout_manager();
    let layout = widget.downcast_ref::<gtk::Box>().map(|_| match &manager {
        Some(m) if m.is::<gtk::GridLayout>() => {
            let grid = m.downcast_ref::<gtk::GridLayout>().expect("checked type");
            NativeLayout::Grid {
                row_spacing: i32::try_from(grid.row_spacing()).unwrap_or(i32::MAX),
                column_spacing: i32::try_from(grid.column_spacing()).unwrap_or(i32::MAX),
            }
        }
        Some(m) if m.is::<gtk::BoxLayout>() => {
            let boxed = m.downcast_ref::<gtk::BoxLayout>().expect("checked type");
            NativeLayout::Box {
                horizontal: boxed.orientation() == gtk::Orientation::Horizontal,
                spacing: i32::try_from(boxed.spacing()).unwrap_or(i32::MAX),
            }
        }
        Some(m) => NativeLayout::Other(m.type_().name().to_string()),
        None => NativeLayout::Other("none".into()),
    });
    let mut virtual_rows = None;
    let children = match widget.downcast_ref::<gtk::Box>() {
        Some(container) => children(container)
            .filter(|c| {
                if c.widget_name() == VIRTUAL_LIST_NAME {
                    virtual_rows = virtual_list_rows(c);
                    false
                } else {
                    true
                }
            })
            .map(|c| {
                let mut node = read_native(&c);
                if let Some(m) = manager.as_ref().filter(|m| m.is::<gtk::GridLayout>())
                    && let Ok(cell) = m.layout_child(&c).downcast::<gtk::GridLayoutChild>()
                {
                    node.grid_cell = Some((cell.column(), cell.row()));
                }
                node
            })
            .collect(),
        None => Vec::new(),
    };
    let mut css_classes: Vec<String> = widget
        .css_classes()
        .iter()
        .map(|c| c.to_string())
        .filter(|c| !GTK_INTRINSIC_CLASSES.contains(&c.as_str()))
        .collect();
    css_classes.sort();
    NativeNode {
        type_name: widget.type_().name().to_string(),
        text,
        value,
        sensitive: widget.get_sensitive(),
        visible: widget.get_visible(),
        width_request: widget.width_request(),
        aura_controllers: aura_controllers(widget),
        layout,
        grid_cell: None,
        css_classes,
        virtual_rows,
        children,
    }
}

fn virtual_list_rows(scrolled: &gtk::Widget) -> Option<u32> {
    let list = scrolled
        .downcast_ref::<gtk::ScrolledWindow>()?
        .child()?
        .downcast::<gtk::ListView>()
        .ok()?;
    Some(list.model()?.n_items())
}

fn collect_labels(widget: &gtk::Widget, out: &mut Vec<String>) {
    if let Some(label) = widget.downcast_ref::<gtk::Label>() {
        if widget.is_mapped() {
            out.push(label.text().to_string());
        }
        return;
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        collect_labels(&c, out);
        child = c.next_sibling();
    }
}

fn splice_rows(model: &gtk::StringList, rows: u32) {
    let current = model.n_items();
    if rows < current {
        model.splice(rows, current - rows, &[]);
        return;
    }
    // Append in bounded chunks of empty strings; row text is fetched at bind.
    const CHUNK: u32 = 4096;
    let blank = [""; CHUNK as usize];
    let mut n = current;
    while n < rows {
        let take = (rows - n).min(CHUNK);
        model.splice(n, 0, &blank[..take as usize]);
        n += take;
    }
}

/// Install `layout` as the container's layout manager.
fn install_layout(container: &gtk::Box, layout: Layout) {
    match layout {
        Layout::VStack { spacing } | Layout::HStack { spacing } => {
            let orientation = if matches!(layout, Layout::HStack { .. }) {
                gtk::Orientation::Horizontal
            } else {
                gtk::Orientation::Vertical
            };
            let manager = match container
                .layout_manager()
                .and_then(|m| m.downcast::<gtk::BoxLayout>().ok())
            {
                Some(m) => m,
                None => {
                    let m = gtk::BoxLayout::new(orientation);
                    container.set_layout_manager(Some(m.clone()));
                    m
                }
            };
            // GtkBox keeps its own orientation/spacing mirror; set both.
            container.set_orientation(orientation);
            container.set_spacing(i32::from(spacing));
            manager.set_orientation(orientation);
            manager.set_spacing(u32::from(spacing));
        }
        Layout::Grid {
            row_spacing,
            column_spacing,
            ..
        } => {
            let grid = match container
                .layout_manager()
                .and_then(|m| m.downcast::<gtk::GridLayout>().ok())
            {
                Some(g) => g,
                None => {
                    let g = gtk::GridLayout::new();
                    container.set_layout_manager(Some(g.clone()));
                    g
                }
            };
            grid.set_row_spacing(u32::from(row_spacing));
            grid.set_column_spacing(u32::from(column_spacing));
        }
    }
}

fn count(node: &NativeNode) -> usize {
    1 + node.children.iter().map(count).sum::<usize>()
}
