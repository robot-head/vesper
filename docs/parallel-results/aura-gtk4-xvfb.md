# Aura GTK 4 backend under Xvfb (wave seven, lane 31)

Claim: #355. Base: `283c8115d2203232d0e9b473cc4bb0312c3a8509` (descendant of
the required baseline `c025b9a`). Branch: `claude/project-thread-06uljc`.

Owned paths: new `crates/vesper-aura-gtk/**`, new
`.github/workflows/aura-gtk.yml`, this report, and the root registration patch
(own commit).

## What was built

`vesper-aura-gtk` implements the existing `vesper_aura::Backend` trait
(read-only dependency) with real GTK 4 widgets, behind the crate feature `gtk`.

- **Plans to widgets.** `Create`, `Destroy`, `BindSlot`, `SetSlot`,
  `UnbindSlot`, `Subscribe`, `Disconnect`, `InsertChild`, `RemoveChild`,
  `MoveChild` and `SetRoot` map to `GtkBox`/`GtkLabel`/`GtkButton`/`GtkEntry`
  construction, property setters, `gtk_box_insert_child_after`,
  `gtk_box_reorder_child_after`, `gtk_box_remove` and `gtk_window_set_child`.
  Same key and kind keep the same `GtkWidget` across reorder and reparent
  (asserted by object identity).
- **Atomicity.** GTK has no transactional API, so every transaction is first
  applied to a private `RecordingBackend` shadow (the core's staged,
  all-or-nothing checker) and to GTK-specific checks (`Width` must fit
  `i32`). Only an accepted transaction is replayed into GTK. Replay calls cannot
  fail on an accepted state; if GTK and the shadow ever disagree mid-replay the
  backend poisons itself (`Desynchronized`, then `Poisoned` on every call).
- **Callbacks by key.** `clicked`, `GtkEditable::changed` and a named
  `GtkEventControllerFocus::enter` push `UiEvent { subscription, handler,
  event, payload }` into a bounded queue drained with `drain_events`.
  Programmatic `Value` writes block the entry's `changed` handler, so only user
  edits route. Events queued for a since-disconnected registration are counted
  `stale` and never delivered; overflow is counted `dropped`; over-budget entry
  text is reported by length only. The window's `close-request` is stopped and
  reported for the host to resolve through `Session::request_close`.
- **Introspection.** `native_tree` reads the widget tree back through GTK's own
  accessors (`gtk_window_get_child`, `gtk_widget_get_first_child` /
  `get_next_sibling`, `G_OBJECT_TYPE_NAME`, label/placeholder/text getters,
  `get_sensitive`, `get_visible`, `width-request`,
  `gtk_widget_observe_controllers`). `audit` compares it with the shadow and
  checks that every live widget is reachable from the window.
- **UiThread discipline.** `GtkBackend::new` initializes GTK on the calling
  thread (serialized by a process lock) or returns `NotGtkMainThread` when
  another thread owns GTK; this never reaches gtk4-rs's "initialize from two
  threads" panic. Every method re-checks the current thread and returns
  `WrongThread` before touching GTK. The backend is `!Send`/`!Sync`
  (compile-fail doctest). On Linux this attests the GTK main thread (the one
  that ran `gtk_init` and acquired the default main context), not the process's
  first thread.
- **Teardown.** `GtkBackend::close` (after `Session::shutdown`) refuses a
  mounted backend (`StillMounted`, returning it; this path is not reachable
  after `Session::shutdown` and is untested), destroys the window, pumps
  the main context and reports `still_alive` from weak references to every GTK
  object it created. A backend dropped while mounted is counted by
  `dropped_while_mounted()` and its window destroyed. Rust ownership here is not
  evidence that Vesper's source typing rules are implemented.

No unsafe code (`#![forbid(unsafe_code)]`). Dependency: `gtk4 = "=0.11.5"`
(gtk4-rs, glib 0.22.10), optional; no GTK feature flags, so it needs GTK ≥ 4.0.

## Tests

`tests/gtk_tree.rs` (`harness = false`, `required-features = ["gtk"]`) runs all
cases sequentially on the main thread against a real GTK 4:

1. `reconcile_sequence`: four plans (mount; reorder + property change +
   disable + add with width; reparent into a nested box + kind replacement +
   removal; root replacement). Asserts GTK types, labels, entry text, sensitivity,
   width and child order after each plan, widget identity across reorder and
   reparent, and a full `audit` after every commit.
2. `keyed_list_transitions`: all 24 permutations of a four-item keyed list plus
   24 prefixes (48 sequential plans on the same `GtkBox`), order and identity
   checked through GTK after each.
3. `callbacks_route_by_key`: `gtk_button_clicked` and a user `set_text` route to
   the right handler keys with payloads; a plan-driven value change emits
   nothing; a click queued before a handler change is stale, later clicks route
   to the new key; removed subscriptions disconnect the GTK handler.
4. `focus_subscriptions_install_controllers`: the named focus controller is
   installed and removed by plans; with the window presented under Xvfb,
   `grab_focus` delivers exactly one Focused event to the handler key.
5. `rejected_plans_leave_gtk_untouched`: rejection injected before every patch
   index (and after the last) and an over-`i32` width leave the GTK tree equal
   to its pre-commit read-back; the retained plan then commits.
6. `close_request_is_routed`: `gtk_window_close` is reported, not obeyed, then
   allowed through the session's close decision, which unmounts.
7. `other_threads_get_typed_errors`: `GtkBackend::new` on a second thread
   returns `NotGtkMainThread` (no panic).
8. `teardown_leaves_no_live_widgets`: after shutdown and close, weak refs to
   every native widget and the window are dead and `still_alive == 0`; a backend
   dropped while mounted is counted and its widgets freed.

Plus two unit tests for the pure helpers (run in every build) and a
compile-fail doctest for `!Send`.

## Checks run

Local host: Ubuntu 24.04, `libgtk-4-dev 4.14.5+ds-0ubuntu0.10`, Xvfb via
`xvfb-run -a`, rustc 1.97.0 and 1.100.0.

```
GSK_RENDERER=cairo xvfb-run -a cargo test -p vesper-aura-gtk --features gtk   # 2 unit + 8 GTK + 1 doctest, ok (5 repeated runs)
xvfb-run -a cargo test -p vesper-aura-gtk --features gtk                      # default GSK renderer, ok
cargo test -p vesper-aura-gtk                                                 # without GTK: 2 unit tests, ok
cargo +1.100.0 clippy -p vesper-aura-gtk --features gtk --locked --all-targets -- -D warnings   # clean
cargo +1.100.0 clippy -p vesper-aura-gtk --locked --all-targets -- -D warnings                  # clean
cargo fmt --all -- --check                                                    # WORKSPACE_FMT
cargo +1.100.0 clippy --workspace --locked --all-targets -- -D warnings       # WORKSPACE_CLIPPY
cargo test --workspace --locked                                               # WORKSPACE_TEST
```

Bazel was not run locally (the Bazel download is blocked in this cloud host);
BuildBuddy CI runs `bazel test //...`. Under Bazel the `gtk` feature is off,
so the library, its unit tests, Clippy and rustfmt targets build without GTK
and the GTK test target is skipped by its required feature. No target needed
the `manual` tag. The GTK tests run in `.github/workflows/aura-gtk.yml` on every
PR touching the crate, `vesper-aura`, the workflow or the root manifests.

## Root patch (own commit)

`crates/vesper-aura-gtk` added to root `members` and `default-members` (before
`experiments/native-pulse`), `Cargo.lock` regenerated by Cargo (new gtk4-rs
packages only; no existing version changed), and
`crates/vesper-aura-gtk/BUILD.bazel` from `tools/bazel/sync_build_files.py`.

## Decisions

- Feature-gated GTK rather than `manual` Bazel targets: the workspace (macOS,
  Windows, Bazel) must build without system GTK, and a non-optional `gtk4`
  dependency would break `cargo test --workspace` there. The tests re-use the
  crate's re-exported `gtk4` so no dev-dependency drags GTK into default builds.
- Xvfb with the X11 GDK backend and the Cairo GSK renderer, chosen over
  Broadway because it exercises the ordinary X11 path and needs no daemon.
- The shadow model is the core's own `RecordingBackend`, so the GTK backend
  inherits its full set of stale/foreign/budget/teardown checks without
  duplicating them.

## Remaining gaps

- WinUI 3 and SwiftUI backends stay deferred (no Windows App SDK or Apple host
  in this effort).
- No GLib main-loop/Pulse co-scheduling: the host drains events and calls
  `pump`; there is no async integration or Send dispatcher from other threads.
- Only `GtkBox` children are modelled; no virtualized lists, layouts beyond a
  vertical box, CSS, accessibility tree checks, or screenshots.
- Window close is reported, not tied to an automatic `request_close` call.
- No performance or allocation claims.
- The backend validates the modelled Aura contract against a real GTK; it is
  not a proof about GTK, and nothing here is a Vesper compiler feature.
