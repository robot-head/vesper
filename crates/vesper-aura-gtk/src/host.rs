//! [`GtkHost`]: a session over a [`GtkBackend`] with keyed decorations and
//! the window's close button wired to `Session::request_close`.

use crate::{GtkBackend, GtkError, RowText, TeardownReport, UiEvent, style::Style};
use std::{collections::BTreeMap, rc::Rc, time::Duration};
use vesper_aura::{
    CloseAction, CloseDecision, CloseOutcome, CloseRequestError, CommitError, CommitReport, Key,
    Kind, Limits, PrepareError, Session, ShutdownError, UiThread, ViewTree, WidgetId,
};

/// Failures of a [`GtkHost`] call. A failed render leaves the previously
/// mounted tree, its native widgets and its decorations unchanged.
#[derive(Debug)]
pub enum HostError {
    Backend(GtkError),
    Init(String),
    Prepare(PrepareError),
    Commit(CommitError<GtkError>),
    /// A decoration does not fit the node it names (for example a layout on
    /// a non-container, or a virtual list in a container with children).
    Style {
        key: Key,
        error: GtkError,
    },
    Close(CloseRequestError),
    CloseResolve(String),
    Shutdown(ShutdownError<GtkError>),
}

impl From<GtkError> for HostError {
    fn from(e: GtkError) -> Self {
        HostError::Backend(e)
    }
}

/// What one [`GtkHost::poll`] collected.
#[derive(Debug, Default)]
pub struct HostEvents {
    pub events: Vec<UiEvent>,
    /// The user asked to close the window (close button, Alt+F4 or
    /// `gtk_window_close`). The host turned that into a pending
    /// `Session::request_close`; this one-shot decision must be resolved
    /// with [`GtkHost::resolve_close`].
    pub close: Option<CloseDecision>,
    /// Close requests while a decision was already pending (coalesced).
    pub close_coalesced: u64,
    pub dropped: u64,
    pub stale: u64,
}

struct ListSpec {
    rows: u32,
    row_text: RowText,
    attached: Option<WidgetId>,
}

/// An Aura session on GTK with host-side, keyed layouts, CSS classes and
/// virtual lists.
#[must_use = "shut the host down to unmount and release native widgets"]
pub struct GtkHost {
    session: Session<GtkBackend>,
    styles: BTreeMap<Key, Style>,
    lists: BTreeMap<Key, ListSpec>,
}

impl std::fmt::Debug for GtkHost {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GtkHost")
            .field("styles", &self.styles.len())
            .field("lists", &self.lists.len())
            .finish_non_exhaustive()
    }
}

impl GtkHost {
    pub fn new(thread: &UiThread, limits: Limits) -> Result<Self, HostError> {
        let backend = GtkBackend::new(thread, limits.clone())?;
        let session = Session::new(thread, backend, limits)
            .map_err(|failure| HostError::Init(format!("{:?}", failure.error)))?;
        Ok(Self {
            session,
            styles: BTreeMap::new(),
            lists: BTreeMap::new(),
        })
    }

    pub fn session(&self) -> &Session<GtkBackend> {
        &self.session
    }
    pub fn backend(&self, thread: &UiThread) -> Result<&GtkBackend, HostError> {
        self.session
            .backend(thread)
            .map_err(|_| HostError::Backend(GtkError::WrongThread))
    }
    pub fn backend_mut(&mut self, thread: &UiThread) -> Result<&mut GtkBackend, HostError> {
        self.session
            .backend_mut(thread)
            .map_err(|_| HostError::Backend(GtkError::WrongThread))
    }
    pub fn widget(&self, key: Key) -> Option<WidgetId> {
        self.session.widget(key)
    }

    /// Decorate `key` (now if mounted, and after every later commit).
    pub fn set_style(
        &mut self,
        thread: &UiThread,
        key: Key,
        style: Style,
    ) -> Result<(), HostError> {
        let style = style.validate()?;
        if let Some(widget) = self.widget(key) {
            self.backend_mut(thread)?
                .set_style(thread, widget, style.clone())
                .map_err(|error| HostError::Style { key, error })?;
        }
        if style == Style::default() {
            self.styles.remove(&key);
        } else {
            self.styles.insert(key, style);
        }
        Ok(())
    }

    /// Host a virtual list of `rows` rows in the (empty) container `key`,
    /// now if mounted and whenever a commit mounts a new widget for `key`.
    pub fn set_virtual_list(
        &mut self,
        thread: &UiThread,
        key: Key,
        rows: u32,
        row_text: RowText,
    ) -> Result<(), HostError> {
        if let Some(spec) = self.lists.get_mut(&key) {
            spec.row_text = Rc::clone(&row_text);
            if let Some(widget) = spec.attached {
                spec.rows = rows;
                return self
                    .session
                    .backend_mut(thread)
                    .map_err(|_| HostError::Backend(GtkError::WrongThread))?
                    .set_virtual_rows(thread, widget, rows)
                    .map_err(|error| HostError::Style { key, error });
            }
        }
        let attached = match self.widget(key) {
            Some(widget) => {
                self.backend_mut(thread)?
                    .attach_virtual_list(thread, widget, rows, Rc::clone(&row_text))
                    .map_err(|error| HostError::Style { key, error })?;
                Some(widget)
            }
            None => None,
        };
        self.lists.insert(
            key,
            ListSpec {
                rows,
                row_text,
                attached,
            },
        );
        Ok(())
    }

    /// Check decorations against `tree` before anything is committed.
    fn check_decorations(&self, tree: &ViewTree) -> Result<(), HostError> {
        let nodes: BTreeMap<Key, &vesper_aura::Node> =
            tree.nodes.iter().map(|n| (n.key, n)).collect();
        for (key, style) in &self.styles {
            if let (Some(node), Some(_)) = (nodes.get(key), style.layout)
                && node.kind != Kind::Container
            {
                return Err(HostError::Style {
                    key: *key,
                    error: GtkError::Unsupported("layout on a non-container"),
                });
            }
        }
        for key in self.lists.keys() {
            if let Some(node) = nodes.get(key)
                && (node.kind != Kind::Container || !node.children.is_empty())
            {
                return Err(HostError::Style {
                    key: *key,
                    error: GtkError::Unsupported("virtual list needs an empty container"),
                });
            }
        }
        Ok(())
    }

    /// Reconcile to `tree` (or unmount with `None`), then re-apply keyed
    /// decorations to the widgets now mounted.
    pub fn render(
        &mut self,
        thread: &UiThread,
        tree: Option<ViewTree>,
    ) -> Result<CommitReport, HostError> {
        if let Some(tree) = &tree {
            self.check_decorations(tree)?;
        }
        let update = self
            .session
            .prepare(thread, tree)
            .map_err(|f| HostError::Prepare(f.error))?;
        let report = self
            .session
            .commit(thread, update)
            .map_err(|f| HostError::Commit(f.error))?;
        self.redecorate(thread)?;
        Ok(report)
    }

    fn redecorate(&mut self, thread: &UiThread) -> Result<(), HostError> {
        let mounted: Vec<(Key, Option<WidgetId>)> = self
            .styles
            .keys()
            .chain(self.lists.keys())
            .map(|k| (*k, self.session.widget(*k)))
            .collect();
        let backend = self
            .session
            .backend_mut(thread)
            .map_err(|_| HostError::Backend(GtkError::WrongThread))?;
        for (key, widget) in &mounted {
            let Some(widget) = widget else { continue };
            if let Some(style) = self.styles.get(key)
                && backend.style(thread, *widget)? != *style
            {
                backend
                    .set_style(thread, *widget, style.clone())
                    .map_err(|error| HostError::Style { key: *key, error })?;
            }
        }
        for (key, spec) in &mut self.lists {
            let widget = mounted.iter().find(|(k, _)| k == key).and_then(|(_, w)| *w);
            match widget {
                Some(widget) if spec.attached != Some(widget) => {
                    backend
                        .attach_virtual_list(thread, widget, spec.rows, Rc::clone(&spec.row_text))
                        .map_err(|error| HostError::Style { key: *key, error })?;
                    spec.attached = Some(widget);
                }
                Some(_) => {}
                None => spec.attached = None,
            }
        }
        Ok(())
    }

    /// Drain routed signals and turn a window close request into a pending
    /// `Session::request_close` decision.
    pub fn poll(&mut self, thread: &UiThread) -> Result<HostEvents, HostError> {
        let batch = self.backend(thread)?.drain_events(thread)?;
        let mut out = HostEvents {
            events: batch.events,
            dropped: batch.dropped,
            stale: batch.stale,
            ..HostEvents::default()
        };
        if batch.close_requested {
            match self.session.request_close(thread) {
                Ok(decision) => out.close = Some(decision),
                Err(CloseRequestError::AlreadyPending) | Err(CloseRequestError::Closed) => {
                    out.close_coalesced += 1;
                }
                Err(e) => return Err(HostError::Close(e)),
            }
        }
        Ok(out)
    }

    /// Pump GLib until a poll yields events or a close decision, or until
    /// `timeout`; returns what was collected (possibly empty).
    pub fn wait(&mut self, thread: &UiThread, timeout: Duration) -> Result<HostEvents, HostError> {
        let mut collected = HostEvents::default();
        let deadline = std::time::Instant::now() + timeout;
        loop {
            self.backend(thread)?.pump(thread)?;
            let mut got = self.poll(thread)?;
            collected.events.append(&mut got.events);
            collected.close = collected.close.or(got.close.take());
            collected.close_coalesced += got.close_coalesced;
            collected.dropped += got.dropped;
            collected.stale += got.stale;
            if !collected.events.is_empty()
                || collected.close.is_some()
                || std::time::Instant::now() >= deadline
            {
                return Ok(collected);
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    /// Resolve a pending close: `Veto` keeps the window, `Allow` unmounts.
    pub fn resolve_close(
        &mut self,
        thread: &UiThread,
        decision: CloseDecision,
        action: CloseAction,
    ) -> Result<CloseOutcome, HostError> {
        let outcome = self
            .session
            .resolve_close(thread, decision, action)
            .map_err(|f| HostError::CloseResolve(format!("{:?}", f.error)))?;
        self.redecorate(thread)?;
        Ok(outcome)
    }

    /// Unmount, release the window and report surviving native objects.
    pub fn shutdown(self, thread: &UiThread) -> Result<TeardownReport, HostError> {
        let backend = self
            .session
            .shutdown(thread)
            .map_err(|f| HostError::Shutdown(f.error))?;
        backend
            .close(thread)
            .map_err(|f| HostError::Backend(f.error))
    }
}
