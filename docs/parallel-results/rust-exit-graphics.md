# Rust exit, SH6 lane 15: graphics and GUI on C APIs (#1410)

Parts 1 and 2 of 5: the audit, and the `vesper-gpu-compute` runtime in
Vesper. Base: `main` at `ab965f2a` (2026-11-21).

The four pieces this lane owns were classed "stays Rust" because they talk to
a C library. This audit lists, for each one, what it needs from the OS or the
C library and what the compiler lacks to express it. Each gap was checked
against a release `vesper` built from the base commit, with Mesa lavapipe,
GTK 4.14 and Xvfb installed on the host.

## Headline

Vesper can already call GTK 4 and Vulkan directly, with no Rust in between.
Two probe programs built with `vesper build` ran on this host:

- **Vulkan (lavapipe).** A package whose `.vsys` links `libvulkan` dynamically
  calls `vkEnumerateInstanceVersion`, `vkCreateInstance`,
  `vkEnumeratePhysicalDevices` and `vkGetPhysicalDeviceProperties`. It printed
  loader version 1.3.275, one device, type 4 (CPU) and the name `llvmpipe`.
- **GTK 4 (Xvfb).** A package that imports GTK and GLib calls `gtk_init_check`,
  builds a window holding a `GtkBox` of two `GtkLabel`s, iterates the main
  context, then walks the widget tree. It read back `hello` and `from vesper`.

Both probes use one pattern, which needs no compiler change:

- Every C pointer is a `u64` in the `.vsys`: opaque objects
  (`GtkWidget *`, `VkInstance`), and pointer fields inside `wire struct`s
  (`pNext`, `pApplicationInfo`, arrays).
- Memory the C side reads or writes lives in `malloc`ed blocks. Vesper fills a
  block with `memcpy(dst: u64, nocapture in src: CSlice[u8])` and reads it back
  with `bcopy(src: u64, nocapture inout dst: CSlice[u8])`. A `CSlice` lowers
  to `(pointer, length)`, which matches both libc signatures.
- Create-info structs with no pointer fields go in by reference, as a
  `nocapture in` `wire struct`. Out-parameters (`VkInstance *`, counts) are
  `nocapture inout u64`/`u32`.

The pattern loses type safety at the boundary, because raw addresses are
plain integers. The port wraps them in linear owners inside the package, so
nothing outside the package sees a raw address. Gap 4 below proposes a typed
replacement.

What still blocks a full port is the work that needs C to call Vesper,
Vesper to call a function pointer, floats by value, or threads (gaps 1-3 and
8). With those gaps, each crate can be ported as follows:

| Crate | Rust (src / tests) | Needs | Blocked by | Plan |
|---|--:|---|---|---|
| `vesper-gpu-compute` | 2,385 / 964 | Vulkan 1.3 core only: instance, device, buffers, memory mapping, descriptor sets, compute pipelines, timeline semaphores, push constants | nothing | **Part 2: port now** |
| `vesper-ignis-vulkan` | 7,925 / 2,577 | Vulkan 1.3 core and synchronization2, plus `VK_KHR_push_descriptor`, `VK_KHR_external_semaphore_fd`, `VK_EXT_debug_utils`, the XCB surface and swapchain, libxcb, epoll on sync fds, a bridge thread | gaps 2, 3, 8 (extension entry points, debug callback, bridge thread); gap 1 for 2 calls | Part 3: core paths first, extensions once gap 3 lands |
| `vesper-aura-gtk` | 3,431 / 1,578 | GTK 4, GLib, GIO D-Bus (AT-SPI), GDK textures, signals, GLib sources, a Pulse worker thread | gap 2 (every signal handler), gap 1 (`gdouble`), gap 6 (link names), gap 8 | Part 4: after gaps 2 and 1 |
| `vesper-highlight` rasterizer (`src/shot/{raster,font}.rs` and the PNG writer) | 704 of `shot/` | glyph outlines (`ttf-parser`), shaping (`rustybuzz`), anti-aliased path fill and stroke (`tiny-skia`), PNG encoding (`png`), BLAKE3 (cache key) | compiler-linked: the `vesper shot` command links the frozen seed copy | Part 5: bit-exact Vesper port of the subset used; deletion at Stage 2 unless lane 3 moves `shot` out of the compiler |

## What each crate needs from C or the OS

### `vesper-gpu-compute` (ash, naga, bytemuck)

- Vulkan entry points: `vkCreateInstance`, `vkEnumeratePhysicalDevices`,
  `vkGetPhysicalDeviceProperties2` (with driver properties),
  `vkGetPhysicalDeviceFeatures2` (Vulkan 1.2 and timeline-semaphore
  features), memory properties, `vkCreateDevice`, `vkGetDeviceQueue`, buffers,
  `vkAllocateMemory`, `vkMapMemory`, descriptor set layouts, pools and
  writes, shader modules, pipeline layouts with push-constant ranges, compute
  pipelines, command pools and buffers, `vkCmdPushConstants`,
  `vkCmdDispatch`, `vkCmdPipelineBarrier`, `vkQueueSubmit` with a
  `VkTimelineSemaphoreSubmitInfo`, `vkWaitSemaphores`,
  `vkGetSemaphoreCounterValue`, and the destroy calls. The loader exports
  all of them, so no function pointers are needed.
- No callbacks, threads, floats by value or extensions. Push constants and
  uploads are bytes, and `f32` data is encoded with `to_bits`.
- `bytemuck` only casts `f32`/`u32` slices to bytes, which Vesper does with
  `to_bits` and byte writes.
- Shaders: 11 WGSL kernels compiled with naga at run time. The runtime tests
  also compile a few inline WGSL strings (see "Shaders" below). `strip_workgroup_layout` post-processes the SPIR-V
  words, which is plain integer work and ports directly.
- CPU references: the tests compare with `vesper-tensor` (llama decoder,
  quant) and `vesper-kinetic` (LBVH broadphase). Both have Vesper ports under
  their own lanes (11 and 9). The port keeps the Rust tests as a differential
  against those crates, so it does not need their ports.

### `vesper-ignis-vulkan` (ash, naga, libloading, vesper-pulse)

- Everything above plus synchronization2 (`vkCmdPipelineBarrier2`,
  `vkQueueSubmit2`, `vkCmdSetEvent2`/`vkCmdWaitEvents2`), images and views,
  render passes and dynamic rendering, graphics pipelines (vertex input,
  depth/stencil), queue-family ownership transfers. These are core 1.3 and
  exported by the loader.
- **Extension entry points that the loader does not export:**
  `vkCmdPushDescriptorSetKHR` (every pass binds this way),
  `vkGetSemaphoreFdKHR` (sync fds) and the `VK_EXT_debug_utils` messenger.
  They are reached only through `vkGetDeviceProcAddr` or
  `vkGetInstanceProcAddr`, which is gap 3. Push descriptors became core in
  Vulkan 1.4, but CI's Ubuntu 24.04 loader (1.3.275) does not export
  `vkCmdPushDescriptorSet`. The port can bind with ordinary descriptor sets
  instead, an internal change no test observes.
- The validation messenger is a C callback (gap 2). Tests collect
  `ValidationMessage`s from it.
- Presentation: libxcb is loaded with `libloading` (`xcb_connect`,
  `xcb_get_setup`, `xcb_create_window`, `xcb_map_window`, `xcb_get_image`,
  `xcb_flush`, ...) and libc `free`. These are plain C calls a `.vsys` can
  declare; `xcb_screen_iterator_t` and the reply structs are wire structs.
- Sync fds: `poll`/`epoll_ctl`/`epoll_wait` (wire-struct `epoll_event`;
  packed on x86_64, which a `wire struct` expresses exactly), `close`, `dup`.
  Where `VK_KHR_external_semaphore_fd` is missing, a bridge thread waits on
  the semaphore and signals an `eventfd`. That thread is gap 8.
- Run-time loading: ash's `loaded` feature `dlopen`s libvulkan so the crate
  builds and its tests skip on hosts without a loader (macOS, Windows,
  hermetic Bazel). A `.vsys` with `link = "dynamic"` makes the executable fail
  to load there instead (gap 7).

### `vesper-aura-gtk` (gtk4-rs, vesper-pulse)

- GTK 4 widgets: `GtkWindow`, `GtkBox`, `GtkLabel`, `GtkButton`, `GtkEntry`,
  `GtkScrolledWindow`, `GtkListView` with a `GtkSignalListItemFactory` over a
  `GtkStringList` and `GtkNoSelection`, `GtkBoxLayout`/`GtkGridLayout`,
  `GtkCssProvider`, `GtkEventControllerFocus`, accessible properties,
  `GtkWidgetPaintable`/`GtkSnapshot`, and GDK textures
  (`GdkMemoryTexture`, `GdkTextureDownloader`) for screenshots.
- **Signals and sources are C callbacks into Vesper (gap 2):** `clicked`,
  `changed`, `close-request`, `enter` (focus), `setup`/`bind`/`unbind` of the
  list factory, the CSS `parsing-error`, the GLib timeout and the
  `spawn_local` futures of the main-loop integration.
- `gdouble` by value (gap 1): `gtk_widget_set_opacity`, adjustments
  (`gtk_adjustment_get_value`) for virtual-list scrolling, `GdkRGBA`
  (four `float`s in a struct, which wire structs can only carry as `u32` bit
  patterns).
- GObject reference counting (`g_object_ref`/`g_object_unref`,
  `g_object_ref_sink`), weak references (`g_object_weak_ref`, a callback
  again), `g_signal_handler_disconnect`. Varargs `g_object_new`/`g_object_set`
  are avoidable through the typed constructors and setters.
- AT-SPI over D-Bus (`atspi.rs`): today through gio's `DBusConnection` with
  `glib::Variant` arguments. GIO is C (`g_dbus_connection_call_sync`,
  `g_variant_new_*`, `g_variant_get_child_value`). The `_sync` calls need no
  callback.
- Thread discipline: GTK's main-thread check (`g_main_context_is_owner`) is a
  C call. The Pulse worker thread of `spawn_pulse_worker` is gap 8.
- Libraries: `gtk-4`, `gobject-2.0`, `glib-2.0`, `gio-2.0`. The last three
  names are refused by the `.vsys` parser (gap 6).

### `vesper-highlight` rasterizer (tiny-skia, rustybuzz, ttf-parser, png, blake3, base64)

- Glyph outlines from the bundled font (`glyf`/`CFF` quadratic and cubic
  segments), HarfBuzz-equivalent shaping for ligatures and advances
  (`rustybuzz`), anti-aliased fill of paths, rounded rects, shadows and
  strokes (`tiny-skia`, scalar pipeline, no SIMD), PNG encoding (`png`,
  zlib), BLAKE3 for the cache key, base64 for the SVG font embedding.
- The tests pin BLAKE3 hashes of the RGBA pixels (`tests/fixtures/shot/golden/pixels.txt`
  via `tests/shot_cards.rs`), so the flip bar's "0 differences" means
  bit-identical pixels. Calling FreeType and HarfBuzz instead would change
  the pixels and need the goldens re-blessed, which is an owner call. The
  plan is therefore a Vesper port of the subset of tiny-skia's scalar
  pipeline the cards use (non-zero/even-odd fill with Skia's 4x
  supersampling, solid source-over, hairline and thick strokes), a Vesper
  OpenType reader for the outlines the font uses, and a stored-deflate or
  zlib PNG writer. That port needs only integers and floats, not C.
  HarfBuzz (C) remains an option for shaping: rustybuzz is a port of it and
  should give the same glyphs and advances, which the differential checks.
- BLAKE3 has a C library. Lane 13 and lane 17 own how Vesper reaches it.
- The `vesper shot` command links the frozen seed copy
  (`crates/vesper-highlight/seed`, `shot` feature, root `Cargo.toml`), so this
  Rust is compiler-linked. It flips with the seed pattern and is deleted at
  Stage 2, unless lane 3 moves `shot_commands` out of the compiler.

## Shaders: precompiled SPIR-V, no naga shim

Both crates compile WGSL only at known places. ignis-vulkan's tests use only
the four bundled shaders. gpu-compute bundles 11 kernels, and its runtime
tests add a few inline WGSL strings: an `add` kernel, an out-of-bounds
kernel, and malformed modules that check naga's errors and the binding
interface (`tests/runtime.rs`). The port therefore precompiles every entry
point it runs, the inline test kernels included, to SPIR-V 1.3. It uses the
same naga version (30.0.2), the same options and the same
`strip_workgroup_layout` pass, and checks the words into the crate. A
manifest beside them records what the Rust code reads from the naga module
today: storage bindings, immediate bytes and the workgroup size. The Vesper
runtime checks the binding interface again on the SPIR-V itself
(`OpEntryPoint` execution model, `DescriptorSet`/`Binding` decorations,
storage classes), so the interface error tests keep a Vesper counterpart.
WGSL parse and validation errors are naga's own behaviour. They become
checks of the regeneration step. That step runs `naga-cli` (pinned and
installed with `cargo install`, a development tool rather than repository
Rust). Until the Rust crate is deleted, CI also checks that the committed
words equal what the Rust crate produces. No naga shim goes into lane 17's
crate.

## Compiler gaps

Every gap is in a path another lane owns: `crates/vesper-ffi` (the `.vsys`
parser and descriptors) is SH6 lane 13's, and the code generator belongs to
the SH4/SH5 lanes. None of them was small enough to fix here without editing
those paths. The repros below are minimal and were run against the base
commit.

### Gap 1: `f32`/`f64` by value in `.vsys`

```text
sys library Gtk (target = "@TARGET@", abi = "@ABI@", link = "dynamic", name = "gtk-4",
    artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") {
    fn gtk_widget_set_opacity(w: u64, opacity: f64) -> ForeignVal[Unit];
}
```

`vesper check`: `Vsys: unsupported value layout; use explicit supported
carriers`. `f32`/`f64` are also refused as `wire struct` fields, so
`GdkRGBA`, `VkViewport` and `VkClearValue` must carry `u32` bit patterns.
Needed by aura-gtk (opacity, adjustments, colours) and ignis-vulkan
(`vkCmdSetDepthBias`, `vkCmdSetLineWidth`; struct fields work around it).
The library ABI already passes `f64` in float registers (porting-a-crate.md),
so the code generator has the pieces.

### Gap 2: C calling Vesper (callbacks)

```text
    fn g_signal_connect_data(instance: u64, nocapture in signal: CStr,
        handler: fn(u64, u64) -> Unit, data: u64, destroy: u64, flags: u32) -> ForeignVal[u64];
```

`vesper check`: `Vsys: unsupported value layout; use explicit supported
carriers` (`fn` is refused). There is also no way to take a Vesper
function's address, and no C-ABI entry point in an executable. Needed by
every GTK signal and GLib source, the list-item factory, the Vulkan debug
messenger and `pthread_create`. The smallest useful form is a `.vsys`
parameter `handler: extern fn(u64, u64)` that accepts a top-level Vesper
function whose parameters are all scalars, compiled with the C calling
convention and run with no captured state (`user_data` carries the state).
The callback body then performs only `Ffi` effects, which keeps the effect
rules intact. This is a language decision: the manuscript (§16.3) has no
foreign function-pointer type.

### Gap 3: calling a foreign function pointer

`vkGetDeviceProcAddr(device, "vkCmdPushDescriptorSetKHR")` returns an address
that must be called. A `.vsys` cannot declare a function whose symbol comes
from a run-time value. A form like `fn(ptr) vkCmdPushDescriptorSetKHR(...)`,
calling through a `u64` the package resolved, would cover the Vulkan
extensions and `dlsym` (gap 7). Needed by ignis-vulkan only.

### Gap 4: pointers inside C structs, and foreign memory

A `wire struct` field must be a total integer, so `pNext`, string arrays
and buffer pointers are `u64`s pointing into `malloc`ed memory, filled and
read with libc `memcpy`/`bcopy` (the pattern above). It works, but an
address is an unchecked integer, and every read is a copy. A typed
`ForeignBuf` (a linear extern type over `malloc`'s block, with `view_wire`
projections, §16.2) and a `ForeignPtr[W]` field type would make it safe. This
is a manuscript decision too. The ports use the pattern meanwhile and keep
every address inside the package behind linear owners.

### Gap 5: a `.vsys` import of a symbol the runtime already imports

```text
sys library LibC (target = "@TARGET@", abi = "@ABI@", link = "dynamic", name = "c",
    artifact = "blake3:0000000000000000000000000000000000000000000000000000000000000000") {
    fn write(fd: i32, nocapture in buf: CSlice[u8]) -> ForeignVal[i64];
}
```

`vesper build`: `Codegen: foreign symbol 'write': Function write signature
... is incompatible with previous declaration` (the runtime declares `write`
as three `i64`s). Declaring `fd: i64` links. The compiler should either
reuse the runtime's declaration for an ABI-compatible import, or report the
clash as a `.vsys` diagnostic.

### Gap 6: link names with a dot, and one library per `.vsys`

`name = "glib-2.0"` gives `Vsys: link name must be a bare library name, never
flags/paths`. GLib, GObject and GIO all have a dot in their names. The
probe works around it with a `libglib2.so` symlink in a `--link-search`
directory. The parser accepts `pkg_config = ...` but nothing links from it, so a
symbol that lives in `glib-2.0` but is declared in the `gtk-4` library fails
with `DSO missing from command line`. Allowing `.` in link names (it is
neither a flag nor a path separator) is enough.

### Gap 7: optional libraries

`link = "dynamic"` makes the executable fail to load when the library is
missing. The Rust crates `dlopen` libvulkan and libxcb so that their tests
skip on hosts without them (macOS, Windows, hermetic Bazel). A Vesper port can
build its GPU tests only where the library exists (a `pkg-config` probe in
the crate's build script) until there is weak linking or a `dlopen` path,
which needs gap 3.

### Gap 8: threads

No thread spawn exists in Vesper or `@vesper-std`. aura-gtk's Pulse worker
and ignis-vulkan's sync-fd bridge thread each run one. `pthread_create`
needs gap 2. Lane 13 (runtime glue, `vesper-pulse`) is the natural owner of a
thread primitive.

### Not gaps

- `usize` and `u64` are different source types. A `.vsys` that declares `u64`
  for `size_t` and pointers avoids conversions on LP64 targets.
- Integer arithmetic and indexing raise `Fail[ArithOverflow]` and
  `Fail[RuntimeError]`, and a native entry must handle every effect. The
  probes wrap `main` in `handle Fail[..]`.

## Part 2: the `vesper-gpu-compute` runtime in Vesper

`crates/vesper-gpu-compute/vesper/` is a Loom package (`@vesper/gpu-compute`)
that implements the runtime: open the device, buffers, kernels, recordings,
submissions and timeline waits. It calls the Vulkan loader and libc
directly, through `vk.vsys` (47 core 1.2 entry points) and `libc.vsys`
(`calloc`, `free`, `memcpy`, `bcopy`, `memset`), with no Rust in between.
Every create-info struct is built as a byte image in Vesper and copied
into `calloc`ed C memory. Its offsets are the Vulkan headers' LP64 layout,
checked with `offsetof` against the 1.3.275 headers. The package does no
work that ash did not do: no extensions, callbacks or floats by value.

- **Build switch.** The feature `vesper-port` builds the package with
  `vesper build --lib` (`build.rs`, `vesper-port-harness`), links it and the
  system libvulkan, and selects `src/runtime_port.rs`. That module keeps the
  crate's public API and its ownership rules (`Arc` owners, the queue lock,
  `busy_until`, the leak counter), and it forwards each Vulkan operation to
  the binding (`port/binding.rs`, generated and checked by the harness).
  Without the feature, the crate builds the Rust runtime over ash, as before.
- **Differential.** All 23 crate tests and the 2 `vesper-kinetic-gpu` tests
  pass against both builds on lavapipe with `VESPER_GPU_REQUIRE=1`. Under
  `VK_LAYER_KHRONOS_validation` the Vesper build reports 0 validation
  errors. `.github/workflows/gpu-compute.yml` now runs clippy, the tests and
  the validation-layer run for the Vesper build as well.
- **Speed.** `examples/runtime_bench.rs` opens a device, compiles one
  kernel, then runs 200 rounds of: write 16 KiB, record 4 dispatches,
  submit, wait, read back. `port/abba.py` alternates the two release
  binaries A B B A in fresh processes. The result for 10 pairs on lavapipe
  is Vesper/Rust = **1.034, 95% interval [0.987, 1.081]**, and both builds
  print the same result digest. The runtime is within the 1.5x bar.

### Flip-gate verdict

| Module | Differential | ABBA (Vesper/Rust) | Verdict |
|---|---|---|---|
| `vesper-gpu-compute` runtime (`src/runtime.rs`, 1,093 lines) | 0 differences: 25 of 25 tests pass in both builds, and 0 validation-layer errors | 1.034 [0.987, 1.081] | passes the bar; **flip held back by gap 7** |

The flip is held back because the Vesper build links libvulkan at link
time. The default build has to link on macOS, Windows and the hermetic
Bazel host, and none of them has a Vulkan loader. The Rust build `dlopen`s
the loader and reports the device as unavailable there. Making the Vesper
build the default needs gap 7 (an optional library, or `dlopen` plus gap
3's calls through a function pointer). Until then the Vesper runtime is the
tested non-default build, and `src/runtime.rs` stays. Nothing is deleted
in this part.

The rest of the crate remains Rust: `shader.rs` (naga, replaced by
committed SPIR-V at the flip), and `tensor.rs` and `broadphase.rs`. The
last two are host-side orchestration over the runtime and port next.

## Plan for the remaining parts

1. **Part 2b: the rest of `vesper-gpu-compute`.** `tensor.rs` and
   `broadphase.rs` in Vesper over the runtime, then the committed SPIR-V
   and manifest in place of naga. The flip waits for gap 7.
2. **Part 3: `vesper-ignis-vulkan`.** The core paths (barriers, timelines,
   formats, depth/stencil, split barriers, ranges, ownership) first, binding
   with ordinary descriptor sets. Presentation over libxcb. Sync fds and the
   validation messenger once gaps 2 and 3 land.
3. **Part 4: `vesper-aura-gtk`,** once gap 2 lands (and gap 1 for opacity and
   scrolling).
4. **Part 5: the highlight rasterizer,** a bit-exact port of the
   tiny-skia subset, with OpenType outlines and the PNG writer, under the
   seed pattern.
5. Each flip runs the flip gate (lane 1) and updates the crate's row in
   `scripts/rust-census.py`. Parts 1 and 2 delete nothing, so the census
   rows are unchanged.

## Checks run

- `cargo build --release --bin vesper` at `ab965f2a`.
- Part 2: `cargo test -p vesper-gpu-compute -p vesper-kinetic-gpu`, with
  and without `--features vesper-gpu-compute/vesper-port`, on lavapipe with
  `VESPER_GPU_REQUIRE=1` (25 passed each), and again under the Khronos
  validation layer for the Vesper build (0 validation errors).
  `cargo +1.100.0 clippy --all-targets -D warnings` with and without the
  feature, `cargo +1.100.0 fmt --check`, and the ABBA run above.
- The Vulkan probe: `vesper check`, `vesper build`, and a run on lavapipe
  (Mesa 25.2.8, loader 1.3.275).
- The GTK probe: `vesper build --link-search <dir>` and a run under Xvfb
  (GTK 4.14.5).
- The gap repros above, each with `vesper check` or `vesper build`.

