# Aura GTK 4 depth: main loop and Pulse, layouts, CSS, AT-SPI, screenshots (wave eight, lane 25)

Claim: #474. Base: `1094e64` (origin/main after plan PR #477; descendant of the
required baseline `ea1ed29`). Branch: `claude/project-thread-lxjrf6`.

Owned paths: `crates/vesper-aura-gtk/**`, `.github/workflows/aura-gtk.yml`,
this report, plus a one-line root `Cargo.lock` patch in its own commit.

## What was built

All GTK code stays behind the crate feature `gtk`; the pure parts (dispatcher,
styles, image comparison) build and are unit-tested everywhere. No unsafe code
(`#![forbid(unsafe_code)]`). The gtk4-rs `v4_12` API level is now enabled
(accessible properties, `CssProvider::load_from_string`,
`ListView::scroll_to`, `TextureDownloader`), so the feature needs GTK ≥ 4.12;
Ubuntu 24.04 has 4.14.5.

1. **GLib main loop and Pulse (§18.4).**
   - `dispatch::channel(capacity)`: a bounded `Dispatcher<T>` (`Send + Sync`,
     cloneable) and a `!Send` `Inbox<T>`. `send` never blocks; a full or
     closed inbox hands the message back (`DispatchError::{Full, Closed}`).
     A send wakes the inbox's registered `std::task::Waker`.
   - `mainloop::spawn_inbox`: runs a handler for an inbox as a local task on
     GLib's default main context. The registered waker is GLib's task waker,
     so a send from any thread schedules the handler on the GTK thread; there
     is no polling. `InboxTask::close` stops it and returns undelivered
     messages.
   - `mainloop::co_schedule`: runs a `vesper_pulse::scheduler::Scheduler` on
     the GTK thread as a local GLib task, at most `slice` resumes per
     dispatch, yielding to GLib between slices. While only I/O is in flight
     it re-polls the driver after a bounded exponential backoff (250 µs to
     8 ms by default). `PulseTask::stop` returns the scheduler with all
     still-owned continuations.
   - `mainloop::spawn_pulse_worker`: runs a scheduler on its own thread and
     posts every result through a `Dispatcher`. It never blocks indefinitely
     in the driver (non-blocking polls with a 50 µs to 2 ms backoff while
     nothing is runnable), so closing the inbox cancels in-flight I/O and
     stops the worker with `BrokenPipe` even if no I/O ever completes.
   - The inbox task yields to GLib after every batch, and every helper that
     drains the main context (`pump`, `pump_until`, `settle`, `close`) does
     at most 64 iterations between condition and deadline checks, so an
     always-ready source cannot starve the UI or trap a caller.
   - `mainloop::pump_until` and `run_off_thread` (pump GLib while a helper
     thread works; needed to query this process's own AT-SPI tree).
2. **Layouts, virtual lists, CSS, close wiring.**
   - `Layout::{VStack, HStack, Grid}` on any Aura container, installed as the
     container's `GtkBoxLayout` or `GtkGridLayout` (same `GtkBox`, so widget
     identity survives a layout change). Grid cells are recomputed after every
     transaction that touches the container.
   - CSS: `Style.classes` (validated identifiers, at most 16) and
     `GtkBackend::load_css` / `clear_css`. A stylesheet with any parse error
     is rejected whole with `GtkError::Css(messages)`.
   - Virtual lists: `attach_virtual_list` hosts a `GtkListView` (inside a
     `GtkScrolledWindow`) over a `GtkStringList` in an empty Aura container.
     Row widgets are `GtkLabel`s created by a `GtkSignalListItemFactory` and
     recycled; row text comes from a host closure at bind time. Inserting
     Aura children into that container is rejected before any change;
     destroying the container detaches the list.
   - `GtkHost` owns the `Session<GtkBackend>` and keeps styles and virtual
     lists by Aura key, re-applying them after each commit. Decorations that
     do not fit the new tree fail `render` before anything is committed.
     `poll`/`wait` turn the window's close request into a pending
     `Session::request_close` decision; requests while one is pending are
     coalesced and counted. `resolve_close` vetoes or unmounts.
   - `audit` now also compares the layout manager, grid cells, host CSS
     classes and virtual-list row counts read back from GTK with the model.
3. **Accessibility and screenshots.**
   - `atspi::AtspiClient`: a small AT-SPI 2 client over GIO D-Bus
     (`org.a11y.Bus.GetAddress`, then `org.a11y.atspi.Accessible`
     `GetChildren`/`GetRoleName`/`GetState` and the `Name` property), with
     node and depth budgets. `atspi::audit` reports showing interactive
     objects without an accessible name (WCAG 2.2 SC 4.1.2). An `Input`'s
     placeholder is now exported as its accessible label.
   - `GtkBackend::settle` (present and wait for drawn frames) and
     `GtkBackend::screenshot` (render the mounted root through the window's
     GSK renderer and download unpremultiplied RGBA8). `image::compare`
     checks a screenshot against a golden PNG under a `Tolerance`.

## Tests

`tests/gtk_depth.rs` (`harness = false`, `required-features = ["gtk"]`), all
on the GTK main thread under Xvfb inside `dbus-run-session`:

1. `layouts_follow_keys`: a 2-column grid (cells read back from
   `GtkGridLayoutChild`, then real allocations: column 1 is right of column 0
   by ≥ the column spacing, row 1 below row 0), reorder keeps widgets and
   recomputes cells, nested `HStack` geometry, switching back to a `VStack`,
   and a layout on a node that turns into a non-container rejected before
   commit with the native tree and revision unchanged.
2. `css_classes_and_stylesheets`: a stylesheet with a bad property is
   rejected whole; classes change measured minimum sizes (≥ 177×61), follow
   their key across reorder, are removed by diff, and stop applying after
   `clear_css`; invalid class names and oversized sheets are typed errors.
3. `virtual_list_recycles_rows`: 100 000 rows. Initially 16 rows visible and
   205 row widgets created (GTK's overscan); after scrolling to the last row
   206 row widgets, `row 99999` visible, 410 text fetches in total. Row-count
   changes, a replacement row callback re-texting the rows already shown,
   invalid row counts rejected before anything is stored, rejection of Aura
   children in the list container, and re-attachment after unmount/remount.
4. `close_button_requests_close`: `gtk_window_close` becomes a pending
   decision, a second request is coalesced, veto keeps the window, the next
   request allows and unmounts, a request after closing is ignored.
5. `dispatcher_wakes_the_main_loop`: 4 threads send 250 messages each into a
   64-slot inbox (retrying when full); every batch commits an Aura update on
   the GTK thread; per-sender order holds; closing refuses later sends.
6. `pulse_co_schedules_with_gtk`: three pipe reads (written 150–250 ms
   later by threads) and four 500-step yielding CPU continuations on the
   host Pulse driver (io_uring locally). Measured: 2 010 resumes in 170 GLib
   dispatches, at most 16 per dispatch, 128 yields, 41 timed I/O waits, and
   23–24 button clicks (a 10 ms GLib timeout) routed through Aura while Pulse
   ran. All seven results arrive.
7. `pulse_worker_posts_to_the_ui`: a worker-thread scheduler posts 11
   results through a 4-slot dispatcher; a worker whose inbox is gone stops
   with `BrokenPipe`, also while it waits on a pipe whose writer stays open
   and silent (stopped within 1 s).
8. `accessibility_tree_through_atspi`: through the AT-SPI registry of
   at-spi2-core 2.52.0 (Ubuntu 24.04), the app's tree has the `Save` button
   without `sensitive`, `Go` with `sensitive` and `focusable`, a `text box`
   named `Name` (from the placeholder) with `editable`, the `Status` label,
   and no audit issues; enabling `Save` reaches AT-SPI; an entry without a
   placeholder is flagged by `atspi::audit`.
9. `screenshots_match_goldens`: CSS-coloured containers in a 3-column grid
   and in an `HStack` after a reorder. Pixels at the centres of the swatches
   (located from GTK's own allocations) have the stylesheet's colours, the
   gold swatch's border is white, and the root padding has the background
   colour. Both screenshots match `tests/goldens/grid-swatches.png` (122×58)
   and `tests/goldens/hstack-swatches.png` (232×58) under
   the stated tolerance; a recoloured swatch exceeds it.

**Golden tolerance:** a pixel differs when any RGBA channel differs by more
than 8; a screenshot matches when at most 2 000 pixels per million (0.2 %)
differ, and its size must be equal. Goldens are rendered with
`GSK_RENDERER=cairo`, `GDK_BACKEND=x11`, Xvfb 24-bit, and contain no text,
so fonts do not affect them. Local runs matched with 0 differing pixels and
maximum channel delta 0. `VESPER_UPDATE_GOLDENS=1` rewrites them; on a CI
failure the actual images are uploaded as an artifact.

Unit tests (no GTK): dispatcher wake/order/full/closed, layout and class
validation, image comparison; compile-fail doctests for `Inbox: !Send` and
`GtkBackend: !Send`. The existing `tests/gtk_tree.rs` (8 cases) still passes.

## Checks run

Local host: Ubuntu 24.04, `libgtk-4-dev 4.14.5+ds-0ubuntu0.10`,
`at-spi2-core 2.52.0-1build1`, Xvfb, rustc 1.97.0 and 1.100.0.

```
GSK_RENDERER=cairo GDK_BACKEND=x11 VESPER_REQUIRE_ATSPI=1 dbus-run-session -- \
  xvfb-run -a --server-args="-screen 0 1280x800x24" \
  cargo test -p vesper-aura-gtk --features gtk           # 7 unit + 8 + 9 GTK + 2 doctests, ok (3 repeated runs)
cargo test -p vesper-aura-gtk                            # without GTK: 7 unit + 1 doctest, ok
cargo +1.100.0 clippy -p vesper-aura-gtk --features gtk --locked --all-targets -- -D warnings   # clean
cargo +1.100.0 clippy -p vesper-aura-gtk --locked --all-targets -- -D warnings                  # clean
cargo fmt --all -- --check                               # ok
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings   # ok
cargo test --workspace --locked --no-fail-fast           # 4 460 passed, 22 failed (see below)
VLB_CARGO_LOCK=$PWD/Cargo.lock cargo test -p vesper-loom-build --locked  # ok (26)
```

The 22 workspace failures are environmental and unrelated to this crate:
`lean4lean42_acceptance`, `oracle42_acceptance`, `oracle42_runner`,
`proof_audit_private`, `replay42_kernel` and `resource_model42` need a `lean`
binary or a prepared `VESPER_LEAN4LEAN42_ROOT`, which this host lacks (CI
provides both). `vesper-loom-build` locates `Cargo.lock` by walking up from
the target directory, which was outside the checkout here; with
`VLB_CARGO_LOCK` set it passes.

Bazel was not run locally (the Bazel download is blocked in this cloud host).
Under Bazel the `gtk` feature is off: the library, its unit tests, doctests,
Clippy and rustfmt targets build without GTK, and both GTK test targets are
skipped by their required feature. The GTK tests run in
`.github/workflows/aura-gtk.yml`, which now also installs `at-spi2-core`, runs
inside `dbus-run-session`, sets `VESPER_REQUIRE_ATSPI=1` and triggers on
`crates/vesper-pulse/**` changes.

## Root patch (own commit)

`Cargo.lock`: `vesper-aura-gtk` gains the (optional, `gtk`-feature)
dependency edge on the workspace crate `vesper-pulse`. No new packages and no
version changes. No BUILD.bazel change (`tools/bazel/sync_build_files.py`
produces none).

## Decisions

- **Layouts and CSS are host decorations keyed by Aura key, not new core
  kinds or properties.** The core `vesper-aura` grammar (one `Container`
  kind, five properties) is not this lane's path. Keeping decorations in
  `GtkHost` and re-applying them after each commit gives the same
  key-following behaviour; they are checked against the new tree before the
  commit so a misfit never leaves a half-applied state.
- **Layout managers instead of different container widgets.** Swapping
  `GtkBoxLayout`/`GtkGridLayout` on the same `GtkBox` keeps widget identity
  across layout changes, so the reconciler's identity guarantees hold.
- **Pulse integration uses bounded re-polls while only I/O is pending.** The
  Pulse drivers (`vesper-pulse`, not owned here) expose neither a descriptor
  to add to the `GMainContext` nor a way to interrupt a blocking wait, so the
  GLib task cannot sleep on I/O readiness and a worker blocked in the driver
  could not notice that its inbox closed. Both modes therefore poll the
  driver without waiting and back off (co-scheduled: 250 µs to 8 ms; worker:
  50 µs to 2 ms). The UI-to-Pulse direction is fully waker-driven.
- **Virtual list model.** `GtkStringList` holds one empty `GtkStringObject`
  per row (bounded by `MAX_VIRTUAL_ROWS = 1 000 000`); a custom lazy
  `GListModel` would need GObject subclassing, which uses unsafe code in
  gtk4-rs's macros. Row widgets and row text are lazy.
- **AT-SPI client in-crate over GIO D-Bus** rather than a new dependency
  (`atspi` crate or `pyatspi`): GIO is already linked through gtk4-rs, and
  the lane may only use versions already in `Cargo.lock`.
- **Text-free goldens.** Font hinting and families differ between hosts;
  coloured CSS boxes render identically under the Cairo renderer.

## Remaining gaps

- No GMainContext/Pulse fd fusion: needs a pollable descriptor from the
  Pulse drivers (io_uring eventfd / epoll fd) in `vesper-pulse`.
- No `AdwApplicationWindow`/libadwaita containers, no GTK `Vulkan` GSK
  renderer under Xvfb (Cairo only), no Wayland session.
- Virtual list rows are plain labels with text from the host; rows are not
  Aura subtrees, and changed row data is only re-read when a row is rebound.
- Layouts and CSS are host-level; they are not part of the Vesper source
  `View` language or the core reconciliation plan.
- AT-SPI checks cover roles, names and the sensitive/focusable/editable/
  showing/visible states; no keyboard-navigation order, contrast or text
  attributes, and no Lean WCAG proofs (§18.6).
- No performance or allocation claims beyond the counts above.
- WinUI 3 and SwiftUI stay deferred. Rust ownership in the host is not
  evidence that Vesper's source typing rules for `CloseDecision` or
  `UiThread` are implemented.
