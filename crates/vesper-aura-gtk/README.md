# vesper-aura-gtk

A GTK 4 implementation of the `vesper-aura` transactional `Backend` contract
(design §18). With the `gtk` feature, `GtkBackend` creates, updates, reorders
and tears down real GTK 4 widgets from Aura reconciliation plans and routes GTK
signals back to the host by handler key. Without the feature the crate only
exposes the pure mapping helpers, so workspace builds on hosts without GTK
(macOS, Windows, hermetic Bazel) are unaffected.

```sh
sudo apt-get install libgtk-4-dev xvfb
xvfb-run -a cargo test -p vesper-aura-gtk --features gtk
```

CI: `.github/workflows/aura-gtk.yml` (GTK 4 from Ubuntu 24.04 under Xvfb).

## Mapping

| Aura kind | GTK widget | `Text` | `Value` | Events |
| --- | --- | --- | --- | --- |
| Container | `GtkBox` (vertical) | – | – | – |
| Text | `GtkLabel` | `label` | – | – |
| Button | `GtkButton` | `label` | – | Click → `clicked`, Focused → `GtkEventControllerFocus::enter` |
| Input | `GtkEntry` | `placeholder-text` | `text` | Changed → `GtkEditable::changed`, Focused |

`Enabled` → `sensitive`, `Visible` → `visible`, `Width` → `width-request`
(values above `i32::MAX` are rejected with `GtkError::Unsupported` before any
change). Unbinding a property restores the GTK default. The mounted root is the
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

WinUI 3 and SwiftUI backends remain deferred.
