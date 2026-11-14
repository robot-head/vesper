# vesper-aura-gtk

A GTK 4 implementation of the `vesper-aura` transactional `Backend` contract
(design §18). With the `gtk` feature, `GtkBackend` creates, updates, reorders
and tears down real GTK 4 widgets from Aura reconciliation plans and routes GTK
signals back to the host by handler key. Without the feature the crate only
exposes the pure mapping helpers, so workspace builds on hosts without GTK
(macOS, Windows, hermetic Bazel) are unaffected.

```sh
sudo apt-get install libgtk-4-dev xvfb dbus-daemon at-spi2-core
GSK_RENDERER=cairo dbus-run-session -- xvfb-run -a cargo test -p vesper-aura-gtk --features gtk
```

`VESPER_REQUIRE_ATSPI=1` makes a missing accessibility bus a failure (CI sets
it); `VESPER_UPDATE_GOLDENS=1` rewrites the screenshot goldens. The gtk4-rs
`v4_12` API level is enabled, so GTK 4.12 or newer is required (Ubuntu 24.04
ships 4.14).

CI: `.github/workflows/aura-gtk.yml` (GTK 4 from Ubuntu 24.04 under Xvfb).

## Mapping

| Aura kind | GTK widget | `Text` | `Value` | Events |
| --- | --- | --- | --- | --- |
| Container | `GtkBox` (vertical) | – | – | – |
| Text | `GtkLabel` | `label` | – | – |
| Button | `GtkButton` | `label` | – | Click → `clicked`, Focused → `GtkEventControllerFocus::enter` |
| Input | `GtkEntry` | `placeholder-text` | `text` | Changed → `GtkEditable::changed`, Focused |

`Enabled` → `sensitive`, `Visible` → `visible`, `Width` → `width-request`
(values above `i32::MAX`, and text containing NUL, are rejected with
`GtkError::Unsupported` before any change). Unbinding a property restores the GTK default. The mounted root is the
child of one `GtkWindow` the backend owns.

## Transactions

Every transaction is first validated in full by a private `RecordingBackend`
shadow (the core's staged checker) plus GTK-specific checks. Only an accepted
transaction is replayed against GTK, using calls that cannot fail on an
accepted state, so rejection leaves the native tree untouched. A disagreement
between GTK and the shadow during replay poisons the backend
(`GtkError::Desynchronized`, then `Poisoned`); it indicates a bug, not bad
input. `GtkBackend::audit` reads the native tree back through GTK's own
accessors (`gtk_window_get_child`, `gtk_widget_get_first_child`/`next_sibling`,
`gtk_label_get_text`, `gtk_widget_observe_controllers`, ...) and compares it
with the shadow.

Programmatic `Value` updates block the entry's `changed` registration, so only
user edits are routed. Queued signals whose registration was disconnected
before `drain_events` are counted as `stale` and never delivered; a full queue
(bounded by `max_patches`) counts `dropped`. The window's close request is
reported (`close_requested`) and stopped: the host decides through
`Session::request_close`.

## Threads

`GtkBackend::new` initializes GTK on the calling thread, or fails with
`GtkError::NotGtkMainThread` if another thread already did. Every later call
checks the current thread and fails with `GtkError::WrongThread` instead of
touching GTK. The backend is `!Send`/`!Sync`. On Linux this attests "the thread
that initialized GTK and owns the default main context", not the process's
first thread.

## Teardown

After `Session::shutdown` returns the backend, `GtkBackend::close` destroys
the window and reports how many of the GTK objects it ever created are still
alive (zero unless the host kept strong references). A backend dropped with a
mounted tree is counted by `dropped_while_mounted()` and its window destroyed.

## Host, layouts, CSS and virtual lists

`GtkHost` wraps a `Session<GtkBackend>` and keeps host decorations by Aura
key, re-applying them after every commit (so they follow a key across reorder
and reparent and are re-applied when a kind change replaces the widget):

- `Style { layout, classes }`: a container's `Layout` is `VStack`/`HStack`
  (`GtkBoxLayout` with spacing) or `Grid { columns, .. }` (`GtkGridLayout`,
  child `i` at column `i % columns`, row `i / columns`, recomputed after every
  transaction). CSS classes are validated identifiers.
  `GtkBackend::load_css` loads a stylesheet at user priority; any parse error
  rejects the whole sheet (`GtkError::Css`).
- `set_virtual_list(key, rows, row_text)`: a `GtkListView` over a
  `GtkStringList` model inside an empty Aura container. Row widgets are made
  only for the visible window plus GTK's overscan and recycled while
  scrolling; row text is fetched from the host at bind time. Inserting Aura
  children into that container is rejected before any change.
- Decorations that do not fit the new tree (a layout on a non-container, a
  list in a container with children) fail `render` before anything commits.
- `poll`/`wait` turn the window's close request into a pending
  `Session::request_close` decision (`HostEvents::close`); a second request
  while one is pending is coalesced. `resolve_close` vetoes or unmounts.

`audit` also compares layouts, grid cells, host CSS classes and virtual list
row counts read back from GTK.

## Main loop and Pulse

- `dispatch::channel` gives a bounded `Send` `Dispatcher` and a UI-thread
  `Inbox`; `mainloop::spawn_inbox` runs the handler as a GLib local task whose
  waker is GLib's, so a send from any thread schedules it without polling.
- `mainloop::co_schedule` runs a `vesper_pulse` scheduler on the GTK thread in
  bounded slices, yielding to GLib between slices; with only I/O in flight it
  re-polls the driver on a bounded backoff (Pulse drivers expose no pollable
  descriptor yet).
- `mainloop::spawn_pulse_worker` runs a scheduler on a worker thread with
  blocking waits and posts each result through a `Dispatcher`.

## Accessibility and screenshots

`atspi::AtspiClient` reads the accessibility tree the way a screen reader
does (at-spi2-core bus, `org.a11y.atspi.Accessible`); `atspi::audit` flags
interactive objects without a name. An `Input`'s placeholder is exported as
its accessible name. `GtkBackend::screenshot` renders the root through the
window's GSK renderer to RGBA8; `image::compare` checks it against a golden
PNG under a stated `Tolerance`.

WinUI 3 and SwiftUI backends remain deferred.
