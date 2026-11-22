# Rust exit, SH6 lane 15: graphics and GUI on C APIs (#1410)

Parts 1 and 2 of 5: the audit, and the `vesper-gpu-compute` runtime in
Vesper. Base: `main` at `ab965f2a` (2026-11-21).

The four pieces this lane owns were classed "stays Rust" because they talk to
a C library. This audit lists, for each one, what it needs from the OS or the
C library and what the compiler lacks to express it. Each gap was checked
against a release `vesper` built from the base commit, with Mesa lavapipe,
GTK 4.14 and Xvfb installed on the host.

## Headline

**Update (part 2d):** `vesper-gpu-compute` no longer compiles WGSL. Its ten
kernels are committed SPIR-V, the Vesper library reads each module's binding
interface before it creates a pipeline, and naga is only a test dependency.
See "Part 2d" below.

**Update (part 2c):** `vesper-gpu-compute`'s Vulkan runtime is Vesper-only.
It opens the loader with `dlopen`, now that #1457 fixed gaps 3, 6 and 7. The
Rust runtime over ash is deleted, and the flip gate passes at 1.03x
[0.98x, 1.11x]. See "Part 2c" below.

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
floats by value, or threads (gaps 1, 2 and 8; gap 3, calling a function
pointer, is fixed). With those gaps, each crate can be ported as follows:

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
  `xcb_flush`, ...) and libc `free`. Most are plain C calls a `.vsys` can
  declare, with the reply structs read as wire structs through their
  pointers. `xcb_setup_roots_iterator` returns `xcb_screen_iterator_t` by
  value, which `.vsys` cannot declare (gap 9). The port avoids that call
  and reads the first screen straight from the setup data that
  `xcb_get_setup` points to: the fixed setup header, then the vendor string
  and the pixmap formats, then the screens.
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

**Update (part 2d):** done for `vesper-gpu-compute`, with two changes to the
plan below. The SPIR-V is regenerated by a test that uses naga as a dev
dependency (a test oracle, which stays Rust), not by `naga-cli`. And
`Context::kernel` takes any SPIR-V module rather than a bundled name, so
callers can still bring their own kernels; only WGSL at run time goes away.
There is no manifest: the Vesper reader takes bindings and immediates from
the module itself.

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
words equal what the Rust crate produces.

This narrows the public API. `vesper-gpu-compute`'s `Context::kernel` takes
caller-supplied WGSL, and `vesper-ignis-vulkan` exports `compile_wgsl`.
With committed SPIR-V, both take a kernel or shader from the bundled set
by name (the crate's own kernels and the test kernels), and arbitrary WGSL
at run time goes away. The only callers in the repository are the crates'
own modules and tests, plus `vesper-kinetic-gpu`, which uses the broadphase
module rather than `Context::kernel`. The flip PR names this API change.
If the owner wants arbitrary WGSL at run time kept, the fallback is a
one-function naga shim (`wgsl_to_spirv`) in lane 17's shim crate, and every
current test stays as it is. Until the flip, both builds compile WGSL with
naga, so nothing changes.

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

**Fixed** by #1457 (0787bb71): `.vsys` `callback` types, called as
`Lib::Name(f, args)`, and typed `#[link_name = "..."]` lookups.

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

**Partly fixed** by #1457: `#[link_name]` accepts versioned names. One
library per `.vsys` stands.

### Gap 7: optional libraries

`link = "dynamic"` makes the executable fail to load when the library is
missing. The Rust crates `dlopen` libvulkan and libxcb so that their tests
skip on hosts without them (macOS, Windows, hermetic Bazel). A Vesper port can
build its GPU tests only where the library exists (a `pkg-config` probe in
the crate's build script) until there is weak linking or a `dlopen` path,
which needs gap 3.

**Fixed** by #1457: a package declares `dlopen` and one typed
`#[link_name = "dlsym"]` lookup per entry point. Part 2c uses it to flip
gpu-compute's runtime.

### Gap 8: threads

No thread spawn exists in Vesper or `@vesper-std`. aura-gtk's Pulse worker
and ignis-vulkan's sync-fd bridge thread each run one. `pthread_create`
needs gap 2. Lane 13 (runtime glue, `vesper-pulse`) is the natural owner of a
thread primitive.

### Gap 9: aggregates returned by value

A `.vsys` function result is a scalar, an enum or a handle. A `wire
struct` crosses only through a pointer (`nocapture in`/`inout`).
`xcb_setup_roots_iterator` returns its 16-byte iterator by value, and so do
a few other XCB iterator functions. The ignis-vulkan port reads the setup
data directly instead (see above), so this gap blocks nothing here.

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

### Flip-gate verdict (part 2, before the flip)

| Module | Differential | ABBA (Vesper/Rust) | Verdict |
|---|---|---|---|
| `vesper-gpu-compute` runtime (`src/runtime.rs`, 1,093 lines) | 0 differences: 25 of 25 tests pass in both builds, and 0 validation-layer errors | 1.034 [0.987, 1.081] | passed the bar; flip held back by gap 7 |

The flip was held back because that build linked libvulkan at link time,
and the hermetic Bazel host has no Vulkan loader.

## Part 2c: the runtime flip (Rust runtime deleted)

Gap 7 is fixed (#1457), so the runtime is now Vesper-only:

- **The loader is optional.** `vk.vsys` declares no library: it imports
  `dlopen`, `dlclose` and `dlsym` from libc, a `callback` type per entry
  point (`PfnCreateBuffer`, ...) and a typed `#[link_name = "dlsym"]` lookup
  for each. `open` tries `libvulkan.so.1`, `libvulkan.so`,
  `libvulkan.1.dylib`, `libvulkan.dylib` and `libMoltenVK.dylib`, then checks
  that all 47 entry points exist. A host without a loader gets
  `GpuError::Unavailable("no Vulkan loader: ...")`, as the ash build did;
  that was checked by hiding libvulkan in a mount namespace. Nothing of
  Vulkan is linked, so Bazel builds the crate as any port
  (`vesper_port_object` in `BUILD.bazel`).
- **A lookup per call.** A callback handle cannot cross the export ABI
  (`cannot export ...: type Pfn... is an opaque .vsys extern-type handle`), so
  `Dev` cannot carry a table of function pointers. Each call looks its entry
  point up again (`pfn_*`, one `dlsym`, about 44 ns on glibc). Letting a
  callback handle cross as an opaque word would remove that (gap 10).
- **Deleted.** `src/runtime.rs` (the ash runtime, 1,093 lines) and the
  `ash` dependency of this crate; `src/runtime_port.rs` became
  `src/runtime.rs`. The `vesper-port` feature is gone, and the lavapipe
  workflow runs every test against the one build.
- **Windows.** The ash build loaded `vulkan-1.dll`; the Vesper library uses
  `dlopen`, which Windows lacks, and a `.vsys` has no per-target
  declarations (gap 11). No CI job builds this crate on Windows or macOS.

| Module | Differential | ABBA (Vesper/Rust) | Verdict |
|---|---|---|---|
| `vesper-gpu-compute` runtime | 0 differences: 25 of 25 tests pass, also under `VK_LAYER_KHRONOS_validation` (0 errors) | 1.03x [0.98x, 1.11x], n = 10 (flip gate, `-C target-cpu=native`) | **PASS: Rust deleted** |

`flip-gate.toml` runs it: `python3 scripts/ci/flip_gate.py vesper-gpu-compute`
on lavapipe. Its ABBA arm (`port/bench-abba.sh`) builds the Rust runtime at
0787bb71, the last commit that had it, in a worktree. A run with
`abba.py` directly (no native CPU flags) gave 1.098 [1.051, 1.146].

The rest of the crate remained Rust: `shader.rs` (naga), and the
`tensor.rs` and `broadphase.rs` drivers, which part 2b ports.

## Part 2b: the broadphase and tensor drivers in Vesper

The host-side drivers of `src/broadphase.rs` and `src/tensor.rs` move into
the same package, next to the runtime they call:

- **Broadphase.** `bp_check` validates the boxes from their f32 bits (finite
  exponent, ordered through `order_key`, so no float arithmetic), and
  `bp_run` writes them, records the key pass, the bitonic network, both
  sweeps and the scan into one command buffer, submits it, waits, and reads
  the pair count back. `order_key` is the Vesper function too.
- **Tensor kernels.** `tk_matmul`, `tk_rms_norm`, `tk_rope`, `tk_swiglu`,
  `tk_kv_write` and `tk_attention` do every shape and limit check in the
  same order, with the same messages, and record the dispatch. `tk_step`
  records the whole llama2.c decode step (`14 * layers + 2` dispatches). The
  head-ratio check stays a call to `vesper_tensor::shape::gqa_ratio`, so
  its error keeps its type.
- **What Rust keeps.** The structs that own buffers, kernels, pages and
  sessions; packing `Aabb`s and weights into words; the page lease and
  the embedding row write; sorting the returned pairs; and the RoPE
  frequencies, whose `powf` must match `vesper-tensor`'s CPU formula bit for
  bit. Two crate-internal runtime hooks connect them:
  `Recording::record_with` (a Vesper-recorded dispatch that keeps its
  buffers and kernels alive) and `Context::submit_with` (a Vesper-made
  submission under the queue lock).

| Module | Differential | ABBA (Vesper/Rust) | Verdict |
|---|---|---|---|
| broadphase driver | 0 differences: the 6 broadphase tests (exact pair sets against the CPU LBVH and brute force, typed errors, determinism) | 1.02x [0.96x, 1.05x], n = 10 | **PASS** |
| tensor driver | 0 differences: the 6 kernel tests and 3 generation tests (200 greedy tokens equal to the CPU decoder and the llama2.c reference text) | 1.00x [0.96x, 1.05x], n = 10 | **PASS** |

The baselines (`port/bench-abba.sh broadphase` and `generation`) are the
examples built at 6628b18a, the last commit with the Rust drivers. One
compiler gap showed up while porting: a handler whose result is a
`Result[T, E]` built in its clauses fails in codegen ("native layout is not
yet defined for _"), so `bp_run` returns a small copy struct instead.

### Gap 10: a callback handle in an exported value

See above: a `copy` struct holding a `callback` handle cannot be a
parameter or result of an exported function, even when Rust never reads the
field. An opaque word that Rust can only hand back would let the device
keep its function table.

### Gap 11: per-target `.vsys` declarations

A `.vsys` is the same text for every target (`@TARGET@` and `@ABI@` aside),
so one package cannot import `dlopen` on Unix and `LoadLibraryA` and
`GetProcAddress` on Windows.

## Part 2d: precompiled SPIR-V

`src/shader.rs` compiled WGSL with naga at run time (about 220 lines of
Rust and naga's dependency tree). It is replaced by:

- **Committed SPIR-V.** `src/shaders/spirv/*.spv` hold naga 30.0.2's output
  for the ten kernels (the same options: SPIR-V 1.3, `Restrict` bounds
  checks, and the `strip_workgroup_layout` pass). `tests/spirv.rs` compiles
  the WGSL again and fails if a file is stale; `VESPER_SPIRV_BLESS=1`
  rewrites them. The compiler moved to `tests/support/wgsl.rs`, and naga is
  a dev dependency only.
- **The interface check in Vesper.** `spirv_interface` in
  `vesper/src/lib.ves` reads the module: the compute `OpEntryPoint` by name,
  every `StorageBuffer` variable's `DescriptorSet`/`Binding` (set 0, bindings
  0..n without gaps), the size of the `PushConstant` block from its members'
  `Offset`s and scalar, vector or struct sizes, and it refuses uniform
  buffers, images and samplers. `kernel_new` calls it, so a module that
  binds a different number of buffers or more immediate bytes than the
  caller asked for is a typed `ShaderError`, as before. A test checks that
  it agrees with naga's own metadata on every kernel.
- **API.** `Context::kernel(spirv: &[u8], entry, bindings, immediate_bytes)`
  takes a SPIR-V module instead of WGSL. `ShaderError` keeps `EntryPoint`
  and `Interface`, gains `Malformed`, and loses `Parse`, `Validation` and
  `Backend` (those are naga errors, now caught by `tests/spirv.rs`).
  `shader::interface` exposes the reader. The only callers outside the crate
  (`vesper-kinetic-gpu`) use the broadphase module and are unchanged.
- **Bounds safety.** The WGSL path gave every kernel naga's `Restrict`
  bounds checks; a caller's own SPIR-V may have none. The device is now
  created with `robustBufferAccess` (required of every Vulkan 1.0 device),
  so an out-of-bounds write is discarded and a read stays inside the
  buffer. A test runs a kernel compiled without bounds checks that writes
  past its buffer and checks that a neighbouring buffer is untouched. With
  it on, generation measured 0.99x [0.98x, 1.01x] and broadphase 1.04x
  [1.01x, 1.08x].

Kernel code on the device is byte-for-byte what it was, so the dispatch
workloads cannot change. All 27 tests pass on lavapipe, also under
`VK_LAYER_KHRONOS_validation` (0 errors).

The flip gate passes again: runtime 1.02x [0.97x, 1.05x], broadphase 1.01x
[0.97x, 1.08x], generation 1.00x [0.98x, 1.03x], n = 10 each. The Rust
baselines build the examples as they were at 086e3684, the last commit whose
`Context::kernel` took WGSL (`GPU_EXAMPLE_COMMIT` in `port/bench-abba.sh`).

## Plan for the remaining parts

1. **Part 2d: naga.** Done (see above).
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
   `scripts/rust-census.py`. Part 2c moved `vesper-gpu-compute` to
   `partial` (runtime Vesper-only).

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
- Part 2c, at 0787bb71: `cargo test -p vesper-gpu-compute -p
  vesper-kinetic-gpu` on lavapipe with `VESPER_GPU_REQUIRE=1` (25 passed),
  again under the validation layer (0 errors) and with libvulkan hidden
  (`Unavailable`), `python3 scripts/ci/flip_gate.py vesper-gpu-compute`
  (PASS), `cargo +1.100.0 clippy --all-targets -D warnings`, `cargo fmt
  --check`, `vesper fmt --check`.

