# 38 Aura native interfaces

A stream inspector can receive a row while its user is selecting another. Aura separates the table’s state from its native controls so it can insert the row and preserve the selection without rebuilding the entire view.

<!-- technical-detail -->

A stream inspector can receive live partition rows while its user is filtering topics or closing a window with an uncommitted bookmark. Aura separates pure application state projections from native widget handles so incremental patches update only the controls that changed without rebuilding the interface or dropping active state.

The `@vesper-ui/aura` design maps a single typed view calculus onto Windows, Linux, and macOS toolkits, while the current workspace implements the core reconciler, slot table, and headless simulator in `crates/vesper-aura`, the GTK 4 and AT-SPI backend in `crates/vesper-aura-gtk`, and formal control-label and close-protocol proofs in `lean/InspectorUiSpecs.lean`. As recorded in `REVIEW-STATUS.md` (`ECO-03`), the WinUI 3 COM and macOS Swift 6 host backends remain proposed `.vsys` designs whose widget coverage and callback lifetimes require target-specific verification.

## 38.1 Define state, view, and lifecycle

A view reads application state. An update changes the state in response to input. When the view goes away, its owner must unregister its native event subscriptions. A virtualized table can keep controls only for visible rows while the underlying data set is much larger.

<!-- technical-detail -->

Aura combines Xilem-style typed view-tree reconciliation with generic incremental differentiation from DeCo (OOPSLA 2026) so view trees compile to static inline structs rather than heap-boxed `AnyView` trait objects or reference-counted `RefCell` callback graphs. View functions inspect application models exclusively through second-class `in` borrows, while reducers update independent subtrees such as `inout app.{sidebar}` and `inout app.{editor}` whose non-aliasing is checked statically by `[T-CallProj]` over `vesper-hir` field trees.

Every native event registration—a 64-bit `EventRegistrationToken` on WinUI 3, a `gulong` signal handler ID on GTK 4, or a reactive slot callback on SwiftUI—is wrapped in a `linear struct` subscription token stored inside `View::State`. Under Theorem 2, tearing down a component in `View::unmount` must consume its state and disconnect every OS listener, turning orphaned callbacks and listener cycles into compile-time linearity errors. Large tabular views bind Vesper Strata `TiledSeq` (`AoSoA`) columns directly to native virtualized list controls (`ItemsRepeater`, `GtkListView`, and `NSTableView`), keeping widget instances proportional to visible viewport rows rather than total dataset cardinality.

## 38.2 Patch the part of a view that changed

A state lens identifies the portion a subview reads or updates. Computing a view delta produces changes such as replacing one label or inserting a row. Stable identities let a backend retain focus and selection while applying such patches. Two updates may proceed independently when their write footprints are disjoint.

<!-- technical-detail -->

Static layout combinators (`AppShell`, `Toolbar`, `NavigationSplit`, `VStack`) encode widget structure in their generic types, while dynamic child sequences bump-allocate transient diff descriptors inside a 32 KiB per-frame lexical `Region` (`VirtualArena`) that resets in one pointer write after each reconciliation pass. Parent components isolate sub-views through `Lens::zoom(|in app.{sidebar}| sidebar_view(in app.sidebar), Msg::Sidebar)`, restricting diff traversal and native widget mutation to the exact changed projection.

    ┌──────────────────────────────────────────────────────────────────────────────┐
    │                      InspectorApp State (FieldTree)                          │
    │   ┌───────────────────────────────┐      ┌───────────────────────────────┐   │
    │   │   [T-CallProj] Disjoint Lens  │      │   [T-CallProj] Disjoint Lens  │   │
    │   │      inout app.{sidebar}      │      │       inout app.{editor}      │   │
    │   └───────────────┬───────────────┘      └───────────────┬───────────────┘   │
    └───────────────────┼──────────────────────────────────────┼───────────────────┘
                        ▼                                      ▼
    ┌──────────────────────────────────────────────────────────────────────────────┐
    │      DeCo O(Delta) Reconciler + 32 KiB Per-Frame VirtualArena (1-op reset)   │
    │        View::reconcile(in curr, in prev) ──▶ AuraSlotTable::set_slot()       │
    └───────────────┬──────────────────────────┬───────────────────────────┬───────┘
                    │ dirty slots              │ dirty slots               │ dirty slots
                    ▼                          ▼                           ▼
    ┌───────────────────────────┐ ┌──────────────────────────┐ ┌───────────────────────────┐
    │ WinUI 3 (Proposed .vsys)  │ │ GTK 4 (vesper-aura-gtk)  │ │ SwiftUI (Proposed .vsys)  │
    │ COM vtable + stack HSTRING│ │ FloatingGObj -> ref_sink │ │ Flat NodeOp[] FFI slice + │
    │ linear WinRtEventSub      │ │ linear GSignalSub + GSK  │ │ ObservationRegistrar slot │
    └───────────────────────────┘ └──────────────────────────┘ └───────────────────────────┘

~~~vesper
// Colorless View trait parameterized over state S, linear message Msg, and backend B:
trait View[S, Msg: mod linear, B: UiBackend] {
    // Persistent widget state owning linear OS signal subscriptions and cached props:
    type State: mod linear;
    // Initial mount constructs the native control and returns its linear subscription state:
    fn mount(in self, inout ctx: B::MountCtx) -> (B::Widget, Self::State) eff [UiThread];
    // O(Delta) DeCo reconciliation diffs `self` against `prev` and patches changed slots:
    fn reconcile(
        in self,
        in prev: Self,
        inout widget: B::Widget,
        inout state: Self::State,
        inout ctx: B::ReconcileCtx,
    ) -> Unit eff [UiThread];
    // Exact-once teardown consumes widget and linear State, disconnecting OS handlers:
    fn unmount(
        sink widget: B::Widget,
        sink state: Self::State,
        inout ctx: B::UnmountCtx,
    ) -> Unit eff [UiThread];
}
~~~

## 38.3 Lower one view to native controls

WinUI 3, GTK 4, and SwiftUI have different object, subscription, and update conventions. A backend maps a common view description to those conventions. Preserve the native ownership and notification rules at each boundary so a deleted widget leaves no active subscription.

<!-- technical-detail -->

Aura bridges imperative retained-mode toolkits (WinUI 3 and GTK 4) and declarative value-tree toolkits (SwiftUI) through three package-internal `.vsys` boundary declarations while keeping downstream application packages free of direct FFI (`forbid_unsafe_ffi = true` in `Loom.toml`). Under `ECO-03`, `crates/vesper-aura-gtk` implements the Linux GTK 4 and AT-SPI bridge in the repository today, whereas `sys/winui3.vsys` and `sys/swiftui_host.vsys` specify proposed Windows and macOS lowering contracts.

On Windows (`AuraWinUi3`), `sys/winui3.vsys` targets Windows App SDK 2.x COM activation (`RoGetActivationFactory`, `MicaBackdrop`, `NavigationView`, `ItemsRepeater`), returning a `linear struct WinRtEventSub` from `add_Click` or `add_TextChanged` that must be consumed by `remove_Click` on unmount. Text patches create fast-pass stack `HSTRING` headers over UTF-16 slices in `VirtualArena` via `WindowsCreateStringReference`; this avoids heap allocation only when the COM setter consumes the string synchronously before the frame arena resets, so retained asynchronous WinRT calls must copy into owned buffers. On Linux (`AuraGtk4`), `sys/gtk4.vsys` wraps `GInitiallyUnowned` constructors in `ForeignVal[FloatingGObj[GtkWidget]]`, forcing immediate `g_object_ref_sink` promotion into an owned `affine struct GObj[T]` and pairing `g_signal_connect_data` with a `linear struct GSignalSub`. On macOS (`AuraSwiftUi`), wrapping raw `NSView` loses SwiftUI vibrancy and `AttributeGraph` animations, whereas regenerating `AnyView` across FFI rebuilds subtrees; instead, `AuraSwiftHost` (a Swift 6 `@_cdecl` host) ingests a flat `wire struct NodeOp` topology slice once on mount and wakes individual leaf views through an `ObservationRegistrar`-backed slot store during `reconcile`.

    // sys/winui3.vsys — Proposed Windows App SDK 2.x COM & fast-pass HSTRING bridge
    sys library WinUi3 (winmd = "Microsoft.UI.Xaml >= 2.0") {
        affine extern type XamlElement;
        linear extern type WinRtEventSub;
        // Borrow caveat: utf16 slice must outlive synchronous setter; never retained past frame!
        fn vesper_winui_set_text_fast_hstring(inout el: XamlElement, nocapture in utf16: CSlice[u16]) -> Unit;
        fn vesper_winui_add_click(inout btn: XamlElement, slot_id: u32) -> WinRtEventSub;
        fn vesper_winui_remove_click(inout btn: XamlElement, sink sub: WinRtEventSub) -> Unit;
    }
    // ----
    // sys/gtk4.vsys — Linux GTK 4 + libadwaita C ABI (implemented in crates/vesper-aura-gtk)
    sys library LibGtk4 (pkg_config = "gtk4 >= 4.18 libadwaita-1 >= 1.6", header = "adwaita.h") {
        affine extern type GtkWidget;
        linear extern type GSignalSub;
        fn adw_application_window_new(in app: GtkApp) -> ForeignVal[FloatingGObj[GtkWidget]];
        fn vesper_gobject_ref_sink(sink raw: ForeignVal[FloatingGObj[GtkWidget]]) -> GtkWidget;
        fn gtk_label_set_text(inout label: GtkWidget, nocapture in utf8: CSlice[u8]) -> Unit;
        fn vesper_gtk_connect_clicked(inout btn: GtkWidget, slot_id: u32) -> GSignalSub;
        fn vesper_gtk_disconnect_signal(sink sub: GSignalSub) -> Unit;
    }
    // ----
    // sys/swiftui_host.vsys — Proposed macOS Swift 6 @_cdecl + ObservationRegistrar slot host
    sys library SwiftUiHost (swift_package = "AuraSwiftHost ^1.0.0") {
        linear extern type SwiftHostHandle;
        fn aura_swift_mount_root(nocapture in topology: CSlice[NodeOp]) -> ForeignVal[SwiftHostHandle];
        fn aura_swift_write_i64_slot(inout host: SwiftHostHandle, slot: u32, val: i64) -> Unit;
        fn aura_swift_write_utf8_slot(inout host: SwiftHostHandle, slot: u32, nocapture in s: CSlice[u8]) -> Unit;
        fn aura_swift_notify_slot_changed(inout host: SwiftHostHandle, slot: u32) -> Unit;
        fn aura_swift_unmount_root(sink host: SwiftHostHandle) -> Unit;
    }

## 38.4 Keep UI work on its owning thread

A native widget handle can be thread-affine. A background fiber may prepare data and hand an update back to the UI event loop. A close request can carry a linear decision to finish saving and close or to keep the window open. The close handler must consume the decision exactly once, including when a save fails.

<!-- technical-detail -->

Native toolkit widgets are thread-affine, so Aura marks widget handles `!portable` and gates `mount`, `reconcile`, and `unmount` with the lexical capability `eff [UiThread]`. Background fibers scheduled on Vesper Pulse worker shards do not carry `UiThread` in their effect row and cannot capture `!portable` handles, turning cross-thread widget races into static type errors while letting background tasks compute `TiledSeq` batches and deliver linear messages back to Core 0.

Core 0 fuses the native GUI dispatcher with Pulse I/O completion primitives without polling threads: `DispatcherQueue` co-schedules with Win32 IOCP on Windows, `GMainContext` and Wayland socket descriptors register directly inside Core 0's `io_uring` ring on Linux (`crates/vesper-aura-gtk/src/mainloop.rs`), and `CFRunLoopObserver` plus `CADisplayLink` pair with `kqueue` on macOS. Window-close interception uses a `linear struct CloseDecision` delivered inside `Msg::RequestClose`; because a linear value cannot be implicitly dropped, every branch of the close handler—including I/O error paths—must explicitly consume the token via `decision.veto()`, `decision.allow_immediate_close()`, or after committing an open `WriteTxn`.

    ┌─────────────────────────────────────────┐    Command::perform     ┌────────────────────────────────────┐
    │ Core 0: UI & Pulse Fused Reactor        │ ──────────────────────▶ │ Background Pulse Worker Shards     │
    │ • Has lexical capability eff [UiThread] │                         │ • UiThread ABSENT from eff [...]   │
    │ • Owns !portable Widget & Signal tokens │ ◀────────────────────── │ • Cannot capture !portable handles │
    │ • Win32: DispatcherQueue + IOCP         │  Msg::RecordsLoaded(    │ • Fetches & decodes TiledSeq rows  │
    │ • Linux: GMainContext fd in io_uring    │    TiledSeq[Row])       │   over Net / Storage fibers        │
    │ • macOS: CFRunLoop + CADisplayLink/kq   │                         └────────────────────────────────────┘
    ├─────────────────────────────────────────┴──────────────────────────────────────────────────────────────┤
    │ Linear Close Protocol: OS Close Event ──▶ Msg::RequestClose(linear CloseDecision)                      │
    │   ├─ Some(tx: WriteTxn) ──▶ tx.commit()? (Storage WAL flush) ──▶ decision.allow_immediate_close()      │
    │   ├─ Commit Err(e)      ──▶ decision.veto() + surface error banner (zero silent data loss)             │
    │   └─ None               ──▶ decision.allow_immediate_close()                                           │
    └────────────────────────────────────────────────────────────────────────────────────────────────────────┘

## 38.5 Build the stream inspector’s update path

A background source adds records. A reducer updates the table model, and a view delta tells the backend which visible rows changed. The selection can refer to a stable row key even as new records arrive. Closing the inspector must also end its subscriptions.

<!-- technical-detail -->

In the stream inspector showcase, `PartitionRecordRow` uses a 16-wide `tiled(16) copy struct` layout so hot numeric columns (`offset`, `timestamp_ms`, `crc32`, `byte_len`) stay contiguous in 64-byte L1 cache lines during 120 Hz scrolling while cold strings sit out-of-line. Holding `unsaved_bookmark_tx: Option[WriteTxn]` inside `InspectorApp` lifts the application state and `Msg::RequestClose` to `mod linear` (`[K-Adt]`), forcing `app_update` to commit any pending WAL bookmark transaction before permitting window teardown.

~~~vesper
tiled(16) copy struct PartitionRecordRow {
    hot offset: i64,
    hot timestamp_ms: i64,
    hot crc32: u32,
    hot byte_len: u32,
    cold producer_id: i64,
    cold key_preview: InlineStr[32],
}
// InspectorApp holds an optional linear WriteTxn for unsaved bookmark edits:
affine struct InspectorApp {
    topics: ChunkSeq[TopicMeta],
    selected_topic: u32,
    filter_query: InlineStr[64],
    records: TiledSeq[PartitionRecordRow],
    unsaved_bookmark_tx: Option[WriteTxn],
}
linear enum Msg {
    SelectTopic(u32),
    FilterChanged(InlineStr[64]),
    RefreshPartition,
    RecordsLoaded(TiledSeq[PartitionRecordRow]),
    RequestClose(CloseDecision),
}
@view
fn app_view(in app.{topics, selected_topic, filter_query, records}: InspectorApp)
    -> impl View[InspectorApp, Msg] eff [Theme]
{
    AppShell(title: "Krabka Stream Inspector") {
        Toolbar {
            SearchField(
                placeholder: "Filter by key or offset...",
                value: in app.filter_query,
                on_change: Msg::FilterChanged,
            )
            Button("Refresh", icon: Icon::Refresh, on_press: Msg::RefreshPartition)
        }
        NavigationSplit(
            sidebar: List(in app.topics, selected: app.selected_topic, on_select: Msg::SelectTopic)
                |in topic| NavRow(title: in topic.name, badge: topic.lag_count),
            detail: VStack {
                MetricBanner(count: app.records.len())
                VirtualTable(in app.records, row_height: 28.px()) {
                    Column("Offset", width: 110.px()) |in r.{offset}|   MonoText(r.offset)
                    Column("CRC32",  width: 100.px()) |in r.{crc32}|    HexText(r.crc32)
                    Column("Bytes",  width: 90.px())  |in r.{byte_len}| MonoText(r.byte_len)
                }
            }
        )
    }
}
fn app_update(
    inout app: InspectorApp,
    sink msg: Msg,
) -> Command[Msg] eff [UiThread, Storage, Fail[IoError]] {
    match msg {
        Msg::SelectTopic(id) => {
            app.selected_topic = id;
            Command::perform(|cap net| fetch_partition_rows(id), Msg::RecordsLoaded)
        }
        Msg::FilterChanged(q) => {
            app.filter_query = q;
            Command::none()
        }
        Msg::RefreshPartition => {
            let id = app.selected_topic;
            Command::perform(|cap net| fetch_partition_rows(id), Msg::RecordsLoaded)
        }
        Msg::RecordsLoaded(sink rows) => {
            app.records = rows;
            Command::none()
        }
        Msg::RequestClose(sink decision) => {
            match app.unsaved_bookmark_tx.take() {
                Some(tx) => {
                    let _receipt = tx.commit()?;
                    decision.allow_immediate_close();
                    Command::none()
                }
                None => {
                    decision.allow_immediate_close();
                    Command::none()
                }
            }
        }
    }
}
~~~

## 38.6 Test semantics without opening a window

A headless backend can record a widget tree and accessibility tree for a sequence of inputs. Check a control’s accessible name, focus, and state after a selection change. Such tests exercise the model. Native accessibility integration and appearance still need checks on each toolkit.

<!-- technical-detail -->

Running `vesper test --sim-ui` instantiates `SimAura` (`crates/vesper-aura/src/sim_aura.rs`) over an in-memory semantic widget and AccessKit tree paired with deterministic `SimPulse` scheduling, exercising focus retention, keyboard navigation, async `RecordsLoaded` races, and `CloseDecision` error paths without a display server. In `lean/InspectorUiSpecs.lean`, `Vesper.InspectorUiSpecs` proves WCAG 4.1.2 accessible-label completeness (`appView_all_controls_accessible`) and transactional close safety (`close_never_drops_uncommitted_txn`) with zero custom axioms over the pure `UiControl` and `RefStorageModel` specification; native AT-SPI, UIA, and `NSAccessibility` tree exposure and visual layout still require toolkit-level tests such as `crates/vesper-aura-gtk/tests/gtk_tree.rs`.

    -- lean/InspectorUiSpecs.lean (namespace Vesper.InspectorUiSpecs)
    theorem appView_all_controls_accessible (s : InspectorApp) :
        ∀ ctrl ∈ InteractiveControls (appView s),
          ctrl.accessibleLabel.isEmpty = false
    --
    theorem close_never_drops_uncommitted_txn
        (s : InspectorApp) (d : CloseDecision) (st : RefStorageModel) :
        s.unsavedBookmarkTx.isSome = true →
        (RefStorageModel.run st (appUpdate s (.requestClose d))).2.isOk = true →
        (RefStorageModel.run st (appUpdate s (.requestClose d))).1.committedCount =
          st.committedCount + 1

On the Rust host side, `crates/vesper-aura` encodes `#[repr(C)]` `NodeOp` instructions and deduplicates reactive property updates in `AuraSlotTable::set_slot` so unchanged slot writes emit zero FFI notifications across `SimAura`, GTK 4, WinUI 3, or SwiftUI.

    // crates/vesper-aura — FFI topology instruction and deduplicating reactive slot store
    #[repr(C)]
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub struct NodeOp {
        pub kind_tag: u16,
        pub child_count: u16,
        pub prop_slot_base: u32,
        pub event_action_id: u32,
        pub layout_flags: u32,
    }
    // ----
    impl AuraSlotTable {
        #[inline]
        pub fn set_slot(&mut self, slot_id: u32, next: SlotValue) {
            let idx = slot_id as usize;
            if self.values.get(idx) != Some(&next) {
                self.values[idx] = next;
                self.dirty_slots.push(slot_id);
            }
        }
    }

# 39 Ignis render graphs and GPU lifetimes

A render graph describes which passes read or write each resource on the graphics processing unit (GPU). Ignis uses that information to plan synchronization and identify when memory can safely return to its owner.

<!-- technical-detail -->

A modern renderer overlaps CPU simulation, asynchronous asset decompression, and multi-pass GPU execution without stalling on per-draw reference counts or freeing buffers still referenced by in-flight command lists. Ignis uses substructural resource handles and render-graph access projections to synthesize synchronization barriers and enforce fence-gated memory reclamation at compile time.

The `@vesper-game/ignis` architecture targets DirectX 12 Agility SDK 1.620+ on Windows, Vulkan 1.4 (`VK_EXT_descriptor_heap`) on Linux, and Metal 4 (`MTL4CommandBuffer` and `MTLResidencySet`) on macOS 26+. In the repository, `crates/vesper-ignis` implements the render-graph hazard planner, lifecycle state machines, and multi-backend barrier emitter, `crates/vesper-ignis-vulkan` implements the Vulkan 1.4 backend, timeline sync-fd bridge, and Lavapipe test suite, and `lean/IgnisTriBackendEquiv.lean` proves tri-backend hazard preservation over the modeled barrier tables. Under `REVIEW-STATUS.md` (`ECO-02`), D3D12 Agility SDK 1.620+ Work Graphs, WASI DXC/Slang shader sandboxes, DirectStorage bindings, and Metal 4 native bindings remain proposed targets that require explicit fallback passes where GPU features diverge.

## 39.1 Describe who reads and writes each frame resource

A component query can name disjoint columns in an entity-component system (ECS). GPU passes similarly declare which buffers and textures they access. A command list moves from recording to closed to submitted. Submission creates an outstanding obligation until the relevant GPU fence completes.

<!-- technical-detail -->

Ignis stores scene entities in 16-wide Vesper Strata `tiled(16)` AoSoA archetypes whose columns are projected via `[T-CallProj]` (`inout instances.{pos_x, pos_y, pos_z}` vs `in instances.{vel_x, vel_y, vel_z}`), enabling lock-free SIMD updates across `Pulse::scope` worker fibers without runtime archetype borrow tables or `UnsafeCell` casts. Render-graph passes likewise declare `in` and `inout` borrows over transient and persistent GPU textures and buffers so read/write hazards are visible during graph construction.

Command recording and submission follow a linear typestate chain: `begin_cmd_list` borrows a `ReadyCmdAllocator` to yield `CmdList[Queue, Recording]`, `close()` consumes the recording handle to produce `CmdList[Queue, Closed]`, and `submit_and_signal` consumes both the closed list and allocator to return a `linear struct InFlightFrame` fence ticket. Under Theorem 2, neither the `CommandAllocator` nor per-frame upload memory can be reset until hardware fence completion retires the `InFlightFrame` obligation.

## 39.2 Match shader bindings to host data

The proposed build runs the DirectX Shader Compiler (DXC) in isolation, then inspects the reflection metadata it produces for a High-Level Shading Language (HLSL) program. Reflection describes what the compiled shader expects. Compare field offsets and resource types with host declarations. A typed bindless index associates a descriptor slot with its intended resource kind.

<!-- technical-detail -->

In the proposed shader build pipeline, shaders annotated with `@hlsl_sm69` or `@work_graph_pso` compile inside Vesper Forge's fuel-metered `wasm32-wasip2` Wasmtime sandbox (`vesper-loom-sandbox`, `macro_emits_effects = []`) using WASI DXC 1.9 targeting Shader Model 6.9, caching Prebuilt Shader Databases (PSDB) in `~/.vesper/store/` keyed by `(DxcWasmBlake3, HlslAstBlake3)`. Macro expansion inspects compiled DXIL `RDEF` reflection tables under SM 6.9 scalar block layout to verify exact byte-offset, scalar-width, and size agreement with shared Vesper `wire struct` declarations.

Descriptor access uses 4-byte phantom-typed bindless wrappers (`SrvIdx[T]` and `UavIdx[T]`) indexing a shader-visible 1,000,000-entry `ResourceDescriptorHeap`, turning mismatched buffer-element reads into compile-time type errors while wrapping fallible Agility SDK 1.619 `CreateShaderResourceView1` calls in `ForeignVal` -> `Fail[GpuError]` and resolving GPU timestamps on the CPU timeline via `ID3D12Device15::ResolveQueryData`.

~~~vesper
// 32-byte layout verified 1-to-1 against WASI DXC 1.9 (SM 6.9) DXIL RDEF reflection:
wire struct CullNodeRecord {
    instance_offset: u32le,
    instance_count: u32le,
    lod_error_threshold_fp16: u16le,
    flags: u16le,
    bvh_node_srv: SrvIdx[BvhClusterNode],
    vis_uav: UavIdx[u64],
    draw_stats_uav: UavIdx[u32],
    _pad: ZeroPad[8],
}
~~~

## 39.3 Keep work-graph backing memory live

A Direct3D 12 (D3D12) work graph can schedule nodes on the GPU. Its backing allocation may remain in use after the CPU submits the graph. A lease tracks that storage through submission and fence completion. Reusing it for the next graph too early would overlap two owners.

<!-- technical-detail -->

On hardware supporting DirectX 12 Work Graphs with SM 6.9 Mesh Nodes, frustum culling, cluster expansion, and meshlet rasterization can execute inside a single GPU-scheduled `DispatchGraph` call rather than chained CPU `ExecuteIndirect` passes. Using uninitialized Work Graph backing memory or aliasing scratch ranges across concurrent frames triggers `DXGI_ERROR_DEVICE_REMOVED`; Ignis prevents both errors by parameterizing `linear struct WorkGraphScratch[S]` with a phantom state transitions from `Uninitialized` (forcing exact-once `D3D12_SET_PROGRAM_FLAG_INITIALIZE`) to `Ready` (requiring exclusive `inout` access during dispatch).

~~~vesper
linear struct WorkGraphScratch[S] {
    gpu_va_range: D3D12GpuVirtualRange,
    program_id: D3D12ProgramIdentifier,
}
impl WorkGraphScratch[Uninitialized] {
    // Enforces exact-once D3D12_SET_PROGRAM_FLAG_INITIALIZE before any graph dispatch:
    fn initialize_on_gpu(
        sink self,
        inout cmd: CmdList[Graphics, Recording],
    ) -> WorkGraphScratch[Ready] eff [Ffi[D3D12Agility]] {
        cmd.init_work_graph_backing(sink self)
    }
}
fn dispatch_visibility_graph(
    inout cmd: CmdList[Graphics, Recording],
    inout wg_scratch: WorkGraphScratch[Ready],
    in entry_records: UploadSlice[CullNodeRecord],
) eff [Ffi[D3D12Agility]] {
    cmd.set_work_graph_program(inout wg_scratch);
    cmd.dispatch_graph_cpu_records(in entry_records);
}
~~~

## 39.4 Insert a barrier at the dependency

A pass writing a texture followed by one sampling it needs an ordering and visibility transition appropriate to the target. Two compatible reads may need no transition. A split barrier can begin synchronization near the producer and finish near the consumer while independent work proceeds.

<!-- technical-detail -->

Ignis tracks each render-graph subresource through a compile-time `(LastSync, LastAccess, CurrentLayout)` automaton driven by pass `in` and `inout` parameter modes. Read-after-write transitions from `inout` to `in` emit exact `D3D12_TEXTURE_BARRIER` or `D3D12_BUFFER_BARRIER` descriptors updating sync stages, access masks, and texture layouts without per-draw hash lookups, while consecutive `in` reads in matching layouts are elided automatically (RAR elision). When a writer pass and reader pass share the same layout and are separated by at least three pass indices (`pass_idx - writer_idx >= 3`), Ignis emits an Agility SDK 1.620 Fence Barriers Tier 1 pair (`IntraListFenceSignal` after the producer and `IntraListFenceWait` before the consumer) so cache flushes overlap independent intermediate passes within a single command list.

    Subresource Automaton: (LastSync, LastAccess, CurrentLayout, is_read_only_since_write)
    ──────────────────────────────────────────────────────────────────────────────────────
    Case 1 (Adjacent RAW + Layout Change):
      [Pass 0: inout RT Write] ──▶ TextureBarrier(RT -> ShaderResource) ──▶ [Pass 1: in Read]
    Case 2 (Compatible Read-After-Read):
      [Pass 1: in ShaderRead]  ──▶ (0 barriers emitted: RAR elided!)    ──▶ [Pass 2: in Read]
    Case 3 (Same-Layout Distance >= 3 Split Synchronization):
      [Pass 0: inout DepthWrite] ──▶ IntraListFenceSignal(val = k)
                                          │  (cache flush overlaps independent GPU work)
                                     [Pass 1: Independent Compute] ──▶ [Pass 2: Shadow Copy]
                                          │
                                     IntraListFenceWait(val = k)   ──▶ [Pass 3: in Raytrace]

## 39.5 Keep a streamed asset resident

Asset streaming can read and decompress content asynchronously before the GPU consumes it. DirectStorage submission, I/O completion, residency, and final GPU use are separate events. Releasing a resource requires evidence for its last relevant event.

<!-- technical-detail -->

Streaming GDeflate-compressed meshlets and mipmaps from NVMe storage into D3D12 `PlacedResource` heaps separates queue submission, GPU decompression, shader residency, and tile eviction into distinct lifecycle stages under `eff [AssetStream]`. A `linear struct PendingTileLoad[T]` suspends the calling 64 KiB Pulse fiber on Win32 IOCP until `IDStorageQueue2` GPU decompression signals completion and yields a `ResidentAsset[T]`, whose 64 KiB VRAM tile cannot return to `VramTilePool` without a `RetiredFenceReceipt` proving the last referencing frame has retired.

~~~vesper
linear struct PendingTileLoad[T] {
    dstorage_fence_val: u64,
    placed_heap_slot: HeapTileId,
    staging_srv: u32,
}
linear struct ResidentAsset[T] {
    pub srv: BindlessSrv[T],
    placed_heap_slot: HeapTileId,
}
impl[T] PendingTileLoad[T] {
    fn await_gpu_gdeflate(sink self) -> Result[ResidentAsset[T], DStorageError]
        eff [AssetStream]
    {
        AssetStream::wait_tile_ready(sink self)
    }
}
impl[T] ResidentAsset[T] {
    fn evict_after_fence(
        sink self,
        in retired: RetiredFenceReceipt,
        inout heap: VramTilePool,
    ) -> Unit {
        heap.reclaim_verified_slot(sink self, in retired)
    }
}
~~~

## 39.6 Walk through one D3D12 frame

Update disjoint kinematics columns, record work that consumes them, close the command list, and submit it. The CPU may prepare another frame while the first frame is in flight. Keep per-frame temporary memory attached to the fence that actually covers its final use.

<!-- technical-detail -->

Each render tick executes with `Alloc` omitted from its effect row: temporary CPU structures bump-allocate inside a 4 MiB `Region::scoped(4.mib(), ...)` frame arena while `integrate_kinematics` updates 16-wide `StaticMeshInstance` position columns in `VVec` SIMD registers across `Pulse::scope`. Recording then transitions the visibility buffer, dispatches visibility and DXR 1.2 SER raytraced lighting, closes the command list linearly, and parks the resulting `InFlightFrame` fence ticket inside `gpu.frame_ring` before presenting.

~~~vesper
tiled(16) copy struct StaticMeshInstance {
    hot pos_x: f32,
    hot pos_y: f32,
    hot pos_z: f32,
    hot bounding_radius: f32,
    hot vel_x: f32,
    hot vel_y: f32,
    hot vel_z: f32,
    hot meshlet_srv_idx: u32,
    cold debug_entity_label: InlineStr[32],
}
fn integrate_kinematics[P: IsaProfile](
    inout instances.{pos_x, pos_y, pos_z}: TiledSeq[StaticMeshInstance],
    in instances.{vel_x, vel_y, vel_z}: TiledSeq[StaticMeshInstance],
    dt: f32,
) eff [Isa[P]] {
    let v_dt = VVec::splat[P, f32](dt);
    for tile in 0..instances.tile_count() {
        let px = instances.load_tile_vec[P, pos_x](tile);
        let vx = instances.load_tile_vec[P, vel_x](tile);
        instances.store_tile_vec[P, pos_x](tile, vx.mul_add(v_dt, px));
        let py = instances.load_tile_vec[P, pos_y](tile);
        let vy = instances.load_tile_vec[P, vel_y](tile);
        instances.store_tile_vec[P, pos_y](tile, vy.mul_add(v_dt, py));
        let pz = instances.load_tile_vec[P, pos_z](tile);
        let vz = instances.load_tile_vec[P, vel_z](tile);
        instances.store_tile_vec[P, pos_z](tile, vz.mul_add(v_dt, pz));
    }
}
fn step_and_render_frame[P: IsaProfile](
    inout world.{instances, camera}: GameWorld,
    inout gpu.{device, graphics_queue, swapchain, frame_ring}: D3D12Context,
    in pipelines: IgnisPipelines,
    dt: f32,
) -> Result[FrameStats, GpuError] eff [
    Scope,
    GpuSync,
    Isa[P],
    Ffi[D3D12Agility],
    Fail[GpuError]
] {
    let mut cmd_alloc: ReadyCmdAllocator = gpu.frame_ring.acquire_next_slot();
    Region::scoped(4.mib(), |cap frame| {
        Pulse::scope(|inout s| eff [Scope, Isa[P]] {
            let sim_join = s.spawn(|| eff [Isa[P]] {
                integrate_kinematics[P](
                    inout world.instances.{pos_x, pos_y, pos_z},
                    in world.instances.{vel_x, vel_y, vel_z},
                    dt,
                )
            });
            sim_join.join()
        });
        let mut cmd: CmdList[Graphics, Recording] = gpu.device.begin_cmd_list(inout cmd_alloc);
        cmd.enhanced_tex_barrier(in gpu.frame_ring.vis_buffer, Layout::Undefined, Layout::RenderTarget);
        cmd.dispatch_work_graph(in pipelines.meshlet_cull_and_draw_wg);
        cmd.enhanced_tex_barrier(in gpu.frame_ring.vis_buffer, Layout::RenderTarget, Layout::DirectQueueShaderResource);
        cmd.trace_rays_dxr12(in pipelines.rt_lighting_ser_pso, gpu.swapchain.resolution());
        cmd.enhanced_tex_barrier(in gpu.swapchain.current_backbuffer(), Layout::RenderTarget, Layout::Present);
        let closed_cmd: CmdList[Graphics, Closed] = cmd.close();
        let fence_ticket: InFlightFrame = gpu.graphics_queue.submit_and_signal(sink closed_cmd, sink cmd_alloc);
        gpu.frame_ring.park_in_flight(sink fence_ticket);
        gpu.swapchain.present_tearing_allowed()
    })
}
~~~

## 39.7 Compile dependencies into barriers

For each resource, track its last access and the access required by the next pass. A render-graph compiler uses these facts to generate enhanced barriers or fence coordination. Test a write followed by a read on another queue as well as repeated reads on the same queue.

<!-- technical-detail -->

In `crates/vesper-ignis`, `IgnisBarrierCompiler` maintains `TrackedSubresourceState` (`last_writer_pass`, `last_sync`, `last_access`, `layout`, and `is_read_only_since_write`) for every registered `RgResourceId`. Calling `transition_texture` elides compatible read-after-read accesses immediately, upgrades same-layout hazards across `pass_idx.saturating_sub(writer_idx) >= 3` into paired `IntraListFenceSignal` and `IntraListFenceWait` records, and otherwise emits a direct `EmittedD3D12Barrier::Texture` transition.

    // crates/vesper-ignis — Static Enhanced and Agility SDK 1.620 Fence Barrier compiler
    pub fn transition_texture(
        &mut self,
        pass_idx: usize,
        id: RgResourceId,
        sync_after: BarrierSync,
        access_after: BarrierAccess,
        layout_after: BarrierLayout,
        is_write: bool,
        out: &mut Vec<EmittedD3D12Barrier>,
    ) -> Result<(), SmolStr> {
        let st = self.states.get_mut(&id)
            .ok_or_else(|| SmolStr::new("unregistered render-graph texture"))?;
        if !is_write && st.is_read_only_since_write && st.layout == layout_after {
            return Ok(());
        }
        if let Some(writer_idx) = st.last_writer_pass {
            if pass_idx.saturating_sub(writer_idx) >= 3 && st.layout == layout_after {
                self.next_fence_val += 1;
                let val = self.next_fence_val;
                out.push(EmittedD3D12Barrier::IntraListFenceSignal {
                    pass_idx: writer_idx, sync_before: st.last_sync, fence_val: val,
                });
                out.push(EmittedD3D12Barrier::IntraListFenceWait {
                    pass_idx, sync_after, fence_val: val,
                });
            } else {
                out.push(EmittedD3D12Barrier::Texture {
                    resource: id, sync_before: st.last_sync, sync_after,
                    access_before: st.last_access, access_after,
                    layout_before: st.layout, layout_after,
                });
            }
        } else if st.layout != layout_after {
            out.push(EmittedD3D12Barrier::Texture {
                resource: id, sync_before: st.last_sync, sync_after,
                access_before: st.last_access, access_after,
                layout_before: st.layout, layout_after,
            });
        }
        st.last_sync = sync_after;
        st.last_access = access_after;
        st.layout = layout_after;
        st.is_read_only_since_write = !is_write;
        if is_write { st.last_writer_pass = Some(pass_idx); }
        Ok(())
    }

## 39.8 Compile a shared shader interface for several targets

A Slang pipeline can express a shader once and emit target-specific modules. Compare reflected bindings across outputs and preserve data layouts at the host boundary. A common source file can still require different synchronization or feature selection on each device.

<!-- technical-detail -->

To share shader modules across Windows, Linux, and macOS, the proposed `@gpu_module` macro runs Khronos Slang (`slang-2026.wasm`, `macro_emits_effects = []`) inside `vesper-loom-sandbox` and emits three `mif:blake3`-addressed artifacts into `~/.vesper/store/`: DXIL SM 6.9 plus PSDB archives for D3D12, SPIR-V 1.6 targeting `SPV_EXT_descriptor_heap`, `SPV_EXT_mesh_shader`, `SPV_EXT_shader_invocation_reorder`, and `SPV_KHR_cooperative_matrix` for Vulkan 1.4, and Metal 4 AIR bitcode serialized via `MTL4Compiler` and `MTL4PipelineDataSetSerializer` on macOS. Macro verification checks that every shared `wire struct` keeps identical byte offsets, scalar widths, and total sizes across DXIL scalar block layout, SPIR-V physical storage buffers, and Metal `device const` buffers, while 4-byte `SrvIdx[T]` and `UavIdx[T]` handles map uniformly to D3D12 `ResourceDescriptorHeap`, Vulkan `VK_EXT_descriptor_heap` (`vkWriteResourceDescriptorsEXT` + `vkCmdPushDataEXT`), and a 2-slot `MTL4ArgumentTable` over `MTLTextureViewPool` and `MTLResidencySet`.

As highlighted in `REVIEW-STATUS.md` (`ECO-02`) and Manuscript Section 39.8, single-source shader compilation does not erase structural GPU execution gaps: neither Vulkan 1.4 nor Metal 4 provides a general core equivalent to D3D12 Work Graphs, and cooperative-matrix or neural tensor extensions vary across vendors and devices. Portable render graphs must therefore lower GPU-driven visibility chains into explicit multi-pass compute, task/mesh, or indirect-command-buffer (`ICB`) fallback passes and emit conservative barriers across the entire dynamically indexed bindless resource pool whenever shaders select descriptors at runtime.

## 39.9 Adapt submission to Vulkan and Metal

Vulkan and Metal expose different resource states, command structures, and synchronization primitives. The proposed native bindings translate a common frame description into each backend’s API. Features such as work graphs and cooperative matrices require capability checks and defined fallback behavior.

<!-- technical-detail -->

Vulkan 1.4 and Metal 4 match Ignis's explicit allocator and unretained command-buffer discipline. On Linux (`crates/vesper-ignis-vulkan`), `sys/vulkan14.vsys` writes descriptors directly into host-mapped heap memory via `vkWriteResourceDescriptorsEXT` without `VkDescriptorSet` or `VkImageView` objects, exports timeline `VkSemaphore` sync file descriptors (`vk/syncfd.rs`) into Pulse's `io_uring` reactor, streams tiles via `VK_KHR_host_image_copy`, and keeps G-Buffer attachments on-chip via `VK_KHR_dynamic_rendering_local_read`. On macOS 26+, `MTL4CommandBuffer` is queue-decoupled and intentionally omits automatic Objective-C reference retention of encoded buffers; `sys/metal4.vsys` pairs `Mtl4CmdBufRecording` and `Mtl4CommandAllocatorToken` with `MTLResidencySet` and signals Pulse's `kqueue` loop through `MTLSharedEvent` on commit, while `MTLIOCommandQueue` and `MTL4MachineLearningCommandEncoder` (`MTLTensor`) handle proposed streaming and neural shading passes.

    // sys/vulkan14.vsys — Vulkan 1.4.340+ descriptor heap & io_uring timeline sync-fd ABI
    sys library Vulkan14 (pkg_config = "vulkan >= 1.4.340", header = "vulkan/vulkan.h") {
        affine extern type VkDevice;
        affine extern type VkQueue;
        linear extern type VkCmdBufRecording;
        linear extern type VkCmdBufExecutable;
        linear extern type VkTimelineTicket;
        fn vkWriteResourceDescriptorsEXT(
            in dev: VkDevice,
            nocapture in infos: CSlice[VkResourceDescriptorInfoEXT],
            nocapture inout heap_mem: CSlice[u8],
        ) -> ForeignVal[VkResult];
        fn vesper_vk_cmd_pipeline_barrier2(
            inout cmd: VkCmdBufRecording,
            nocapture in dep_info: VkDependencyInfoWire,
        ) -> Unit;
        fn vesper_vk_end_cmd_buf(
            sink cmd: VkCmdBufRecording,
        ) -> ForeignVal[Result[VkCmdBufExecutable, VkResult]];
        // Exports timeline VkSemaphore sync-fd polled directly by Vesper Pulse io_uring:
        fn vesper_vk_queue_submit2_timeline(
            inout queue: VkQueue,
            sink cmd: VkCmdBufExecutable,
        ) -> VkTimelineTicket;
    }
    // ----
    // sys/metal4.vsys — Proposed macOS 26+ Metal 4 unretained command buffer & kqueue bridge
    sys library Metal4 (apple_framework = "Metal >= 4.0") {
        affine extern type Mtl4Device;
        affine extern type Mtl4CommandQueue;
        affine extern type MtlResidencySet;
        linear extern type Mtl4CommandAllocatorToken;
        linear extern type Mtl4CmdBufRecording;
        linear extern type Mtl4CmdBufCommitted;
        fn vesper_mtl4_begin_cmd_buf(
            in dev: Mtl4Device,
            inout alloc: Mtl4CommandAllocatorToken,
            in residency: MtlResidencySet,
        ) -> Mtl4CmdBufRecording;
        fn vesper_mtl4_cmd_stage_barrier(
            inout cmd: Mtl4CmdBufRecording,
            after_stages: u32,
            before_stages: u32,
        ) -> Unit;
        // Consumes recording buffer + allocator token and wakes Pulse kqueue via MTLSharedEvent:
        fn vesper_mtl4_commit_and_signal_kqueue(
            inout queue: Mtl4CommandQueue,
            sink cmd: Mtl4CmdBufRecording,
            sink alloc: Mtl4CommandAllocatorToken,
        ) -> Mtl4CmdBufCommitted;
    }

## 39.10 Define equivalence by what a frame observes

A portable frame loop acquires resources, runs passes, presents, and retires work in a defined order. Compare the observable dependencies and results across backends. Equal numbers of barriers are unnecessary if the backends enforce the same required ordering.

<!-- technical-detail -->

Parameterizing `step_and_render_portable[B: GpuBackend, P: IsaProfile]` over a colorless `trait GpuBackend` monomorphizes one zero-global-heap frame loop across DirectX 12, Vulkan 1.4, and Metal 4. Cross-backend equivalence is defined by preserving every required happens-before data dependency (`readAfterWrite`, `writeAfterRead`, and `writeAfterWrite`) rather than emitting identical barrier counts, since unified-memory tile GPUs legitimately elide pure layout transitions.

    ┌─────────────────────────────────────────────────────────────────────────────────┐
    │                     Ignis Hazard DAG (compileIgnisRenderGraph)                  │
    │  [Edge E0: Undefined -> RT (layoutOnly)] ──▶ [Pass 1: Visibility Write (RT)]    │
    │                                                     │                           │
    │                         [Edge E1: RT -> Raytrace/Pixel (readAfterWrite)]        │
    │                                                     ▼                           │
    │                                              [Pass 2: Hybrid Lighting]          │
    ├───────────────────────────┬─────────────────────────┴─┬─────────────────────────┤
    │ D3D12 Agility SDK 1.620+  │ Vulkan 1.4 (vesper-ignis) │ Metal 4 (Unified TBDR)  │
    │ E0: TextureBarrier        │ E0: VkImageMemoryBarrier2 │ E0: None (elided!)      │
    │ E1: Texture / Fence Split │ E1: Barrier2 / VkEvent    │ E1: MTL4StageBarrier    │
    │ Completion: Win32 IOCP    │ Completion: sync-fd uring │ Completion: kqueue Evt  │
    └───────────────────────────┴───────────────────────────┴─────────────────────────┘

In `lean/IgnisTriBackendEquiv.lean`, `Vesper.IgnisTriBackendEquiv` models exact `D3D12_BARRIER_SYNC`/`ACCESS` masks (`crates/vesper-ignis/src/d3d12.rs`), Vulkan 1.4 `SYNC_TABLE`/`ACCESS_TABLE` masks including `VERTEX_ATTRIBUTE_INPUT` (`crates/vesper-ignis-vulkan/src/translate.rs`), and `MTL4Stages` masks. It discharges both `tri_backend_barriers_preserve_hazards` across all six `RenderFeatureFlags` profiles by `decide` and the arbitrary-graph generalization `tri_backend_barriers_preserve_any_dag`, alongside negative regression theorems rejecting stage-only or read-only compute WAW barriers.

    -- lean/IgnisTriBackendEquiv.lean (namespace Vesper.IgnisTriBackendEquiv)
    theorem tri_backend_barriers_preserve_hazards (cfg : RenderFeatureFlags) :
        let dag := compileIgnisRenderGraph cfg
        HappensBeforeSound dag (lowerBarriers .d3d12 dag) ∧
        HappensBeforeSound dag (lowerBarriers .vulkan14 dag) ∧
        HappensBeforeSound dag (lowerBarriers .metal4 dag) := by
      cases cfg <;> decide
    --
    theorem tri_backend_barriers_preserve_any_dag (dag : List HazardEdge)
        (h_valid : dag.all HazardEdge.hasValidAccess = true) :
        HappensBeforeSound dag (lowerBarriers .d3d12 dag) ∧
        HappensBeforeSound dag (lowerBarriers .vulkan14 dag) ∧
        HappensBeforeSound dag (lowerBarriers .metal4 dag)

## 39.11 Emit and inspect a target plan

The backend emitter turns abstract hazards into concrete barrier operations. Look up a producer and consumer in the emitted plan and check which operation connects them. Missing transitions risk stale data, while unnecessary transitions can serialize work that was independent.

<!-- technical-detail -->

In `crates/vesper-ignis/src/multi_backend.rs`, `lower_to_target_barrier` translates abstract `EmittedD3D12Barrier` records into target-native synchronization commands: DirectX 12 retains Enhanced and intra-list Fence Barriers verbatim, Vulkan 1.4 lowers immediate transitions to `Vulkan14ImageBarrier2` and long-distance fence barriers to paired `Vulkan14SplitEventSignal`/`Vulkan14SplitEventWait` commands, and Metal 4 collapses data hazards to `Metal4StageBarrier` bitmasks while returning `None` for initial layout-only transitions (`sync_before == None` and `access_before == NoAccess`) in Apple Silicon unified TBDR memory.

    // crates/vesper-ignis/src/multi_backend.rs — Target barrier lowering & TBDR elision
    pub fn lower_to_target_barrier(
        target: GpuApiTarget,
        d3d12_ir: &EmittedD3D12Barrier,
    ) -> Option<NativeBackendBarrier> {
        match (target, d3d12_ir) {
            (GpuApiTarget::D3D12Agility1620, b) => Some(NativeBackendBarrier::D3D12(b.clone())),
            (GpuApiTarget::Vulkan14, EmittedD3D12Barrier::Texture {
                resource, sync_before, sync_after, access_before, access_after, layout_before, layout_after,
            }) => Some(NativeBackendBarrier::Vulkan14ImageBarrier2 {
                resource: *resource,
                src_stage_mask: vk_stage_bits(*sync_before),
                dst_stage_mask: vk_stage_bits(*sync_after),
                src_access_mask: vk_access_bits(*access_before),
                dst_access_mask: vk_access_bits(*access_after),
                old_layout: *layout_before,
                new_layout: *layout_after,
            }),
            (GpuApiTarget::Vulkan14, EmittedD3D12Barrier::IntraListFenceSignal { pass_idx, sync_before, fence_val }) =>
                Some(NativeBackendBarrier::Vulkan14SplitEventSignal {
                    event_id: *fence_val as u32, pass_idx: *pass_idx, src_stage_mask: vk_stage_bits(*sync_before),
                }),
            (GpuApiTarget::Vulkan14, EmittedD3D12Barrier::IntraListFenceWait { pass_idx, sync_after, fence_val }) =>
                Some(NativeBackendBarrier::Vulkan14SplitEventWait {
                    event_id: *fence_val as u32, pass_idx: *pass_idx, dst_stage_mask: vk_stage_bits(*sync_after),
                    old_layout: BarrierLayout::RenderTarget, new_layout: BarrierLayout::RenderTarget,
                }),
            (GpuApiTarget::Metal4, EmittedD3D12Barrier::Texture { sync_before, sync_after, access_before, .. }) => {
                if matches!(sync_before, BarrierSync::None) && matches!(access_before, BarrierAccess::NoAccess) {
                    None
                } else {
                    Some(NativeBackendBarrier::Metal4StageBarrier {
                        after_stages_mask: mtl4_stage_bits(*sync_before),
                        before_stages_mask: mtl4_stage_bits(*sync_after),
                    })
                }
            }
            (GpuApiTarget::Metal4, EmittedD3D12Barrier::IntraListFenceSignal { .. }) => None,
            (GpuApiTarget::Metal4, EmittedD3D12Barrier::IntraListFenceWait { sync_after, .. }) =>
                Some(NativeBackendBarrier::Metal4StageBarrier {
                    after_stages_mask: mtl4_stage_bits(BarrierSync::ComputeShading),
                    before_stages_mask: mtl4_stage_bits(*sync_after),
                }),
        }
    }

# 40 Kinetic deterministic physics and rollback

When a late input arrives, a multiplayer simulation may need to restore an earlier tick and replay it. Reproducing the result requires the saved state, the intervening inputs, and rules for arithmetic and update order.

<!-- technical-detail -->

When a remote player's input arrives several ticks late, a networked simulation must restore an earlier world snapshot and replay intervening substeps inside a single frame budget. Reproducing identical state hashes across peers requires strict control over floating-point rounding, spatial broadphase ordering, parallel constraint scheduling, and snapshot lifetimes.

The `@vesper-sim/kinetic` subsystem is implemented in `crates/vesper-kinetic` (covering rigid bodies, `Ball`/`Hinge`/`Prismatic` joints, swept CCD, 4-body soft tetrahedra, Morton LBVH broadphase, CORE-MATH polynomials, and rollback rings), audited by `vesper build --verify-detfloat`, and paired with pure graph-coloring and Morton-code proofs in `lean/KineticColoring.lean`. As noted in `REVIEW-STATUS.md` (`ECO-03`) and Manuscript Section 40.2, `eff [DetFloat]` and Lean coloring proofs do not by themselves establish cross-architecture bit identity across `x86_64`, AArch64, and RISC-V without backend-specific validation of reciprocals, NaNs, and reductions.

## 40.1 Decide what must repeat

Extended Position-Based Dynamics (XPBD) resolves physical constraints by adjusting predicted positions. Different orders of floating-point operations can produce different answers. A cross-machine replay contract must define rounding, fused operations, NaNs, reductions, and any math functions used by the solver.

<!-- technical-detail -->

Extended Position-Based Dynamics (XPBD) with Temporal Gauss-Seidel (TGS) substeps amplifies single-ULP drift across contact iterations into divergent trajectories within tens of frames. Kinetic gates simulation kernels on `eff [DetFloat]` and `vesper build --verify-detfloat`, constraining Stage 4 (`vesper-opt-egglog`) and Stage 5 (`vesper-codegen-clif`) to IEEE-754 round-to-nearest-even (RNE), separate multiply and add rounding unless `DetFloat::fma` is invoked explicitly, fixed binary-tree SIMD horizontal reductions, canonical quiet-NaN bit patterns, and CORE-MATH (`crates/vesper-kinetic/src/crmath.rs`) polynomials accurate to `<= 0.5` ULP in place of platform `libm` calls.

Hardware floating-point instructions alone still cannot guarantee cross-ISA bit identity (`ECO-03`, Section 40.2): approximate SIMD reciprocal/rsqrt instructions (`rcpss`/`rsqrtss` vs `frecpe`/`frsqrte`), default NaN payload propagation rules, implicit FMA contraction, and lane reduction orders differ across `x86_64`, AArch64, and RISC-V. Achieving identical `world_state_blake3` digests across heterogeneous machines requires lowering reciprocals to IEEE division or fixed Newton-Raphson sequences over software/normalized seeds, normalizing NaNs at operation boundaries, and differentially testing compiled codegen output per target ISA.

## 40.2 Save state needed for the next tick

A wire snapshot records the state required for deterministic restoration. AoSoA tiles store the same body attributes together for vector operations. A rollback ticket accounts for the lifetime of a retained snapshot. Asynchronous GPU work must finish before its backing memory is reused.

<!-- technical-detail -->

Kinetic packs 16 rigid bodies per cache-line-aligned `RigidBodyTile` (`AoSoA`) so `memcpy` snapshots of 10,000 bodies into a pre-allocated ring buffer complete without heap traffic, emitting a padding-free 48-byte `wire struct KineticSnapshotWireHdr` carrying `tick_number`, `active_body_count`, `manifold_count`, and the 32-byte `world_state_blake3` digest. Speculative rollback frames and optional GPU broadphase dispatches are tracked by `linear struct RollbackCheckpoint` and `linear struct GpuBroadphaseLease` handles so unconfirmed history slots cannot be overwritten and in-flight GPU contact buffers cannot be recycled before fence retirement.

    KineticSnapshotWireHdr (48 bytes, 0 padding) & Pre-Allocated Rollback Ring
    ┌──────────────┬───────────────────┬────────────────┬───────────────────────────┐
    │ [0..8] u64le │ [8..12] u32le     │ [12..16] u32le │ [16..48] ByteArray[32]    │
    │ tick_number  │ active_body_count │ manifold_count │ world_state_blake3        │
    └──────────────┴───────────────────┴────────────────┴───────────────────────────┘
    Rollback Ring Slots (memcpy of TiledSeq[RigidBodyTile], 0 GlobalAlloc):
      [Slot k-2: Confirmed] ──▶ confirm_and_retire(sink ckpt) frees ring slot
      [Slot k-1: linear RollbackCheckpoint] ◀── Late remote input arrives at Tick k+3!
           └─▶ rewind_and_resimulate(sink ckpt) restores k-1 & replays k..k+3 in <1 ms

~~~vesper
// 48-byte padding-free rollback frame header carrying canonical BLAKE3 world digest:
wire struct KineticSnapshotWireHdr {
    tick_number: u64le,
    active_body_count: u32le,
    manifold_count: u32le,
    world_state_blake3: ByteArray[32],
}
copy effect DetFloat {}
tiled(16) copy struct RigidBodyTile {
    hot pos_x: f32,
    hot pos_y: f32,
    hot pos_z: f32,
    hot inv_mass: f32,
    hot vel_x: f32,
    hot vel_y: f32,
    hot vel_z: f32,
    hot compliance_alpha: f32,
    cold collider_asset_id: u32,
    cold collision_layer_mask: u32,
}
linear struct RollbackCheckpoint {
    saved_tick: u64,
    ring_slot: u16,
}
linear effect KineticRollback {
    fn capture_checkpoint(tick: u64, in bodies: TiledSeq[RigidBodyTile]) -> RollbackCheckpoint;
    fn confirm_and_retire(sink ckpt: RollbackCheckpoint) -> Unit;
    fn rewind_and_resimulate(sink ckpt: RollbackCheckpoint, inout bodies: TiledSeq[RigidBodyTile]) -> u64;
}
~~~

## 40.3 Solve disjoint constraints together

Treat bodies as vertices and constraints between them as edges. Give conflicting constraints different colors. Constraints of one color can update disjoint bodies in parallel, then the solver advances to the next color. For a chain A–B–C, the constraints A–B and B–C both update B and need different colors.

<!-- technical-detail -->

Within each fixed `dt = 1/120` s tick (4 TGS/XPBD substeps), Kinetic runs inside a resettable 1 MiB lexical `Region::scoped(1.mib(), ...)` step arena (`Alloc` omitted from `eff [...]`), building a Morton LBVH, detecting GJK/EPA manifolds, and partitioning constraints into conflict-free `ColoredConstraintBatch` layers so 16 independent constraints project positional impulses simultaneously in `VVec` SIMD lanes via `[T-CallProj]` (`inout bodies.{pos_x, pos_y, pos_z}` vs `in bodies.{inv_mass, compliance_alpha}`).

In `crates/vesper-kinetic`, rigid joints (`JointKind::Ball`, `JointKind::Hinge` with angular limits, servo/velocity drives, and positional static friction, and `JointKind::Prismatic` with perpendicular axis pinning and slide limits in `src/rigid/joints.rs`) are colored in the same list after contact manifolds, while 4-body tetrahedral volume constraints (`src/soft.rs`) use the `k`-body `MultiColorer`. Fast-moving or fast-spinning bodies whose sweep exceeds `CcdConfig::motion_fraction` run conservative-advancement swept continuous collision detection (`src/rigid/ccd.rs`), advancing along ballistic paths by `s += d / (mu_a + mu_b)` with deterministic time-of-impact tie-breaking and optional single-pass normal-impulse re-sweeping.

    1 MiB Step Arena (Region::scoped, 0 Alloc) — Tick dt = 1/120 s (4 TGS Substeps)
    ┌───────────────────────────────────────────────────────────────────────────────┐
    │ 1. 30-bit/63-bit Morton LBVH + GJK/EPA + Swept CCD (s += d / (mu_a + mu_b))   │
    │ 2. Greedy 64-Bitmask Coloring (Colorer 2-body & MultiColorer 4-body soft tet) │
    │    • Color 0 (SIMD VVec 16-wide): (B0,B1) | (B2,B3) | (B4,B5) ... [Disjoint!] │
    │    • Color 1 (SIMD VVec 16-wide): (B1,B2) | (B3,B4) | Hinge(B5,B6) [Disjoint] │
    │    • Degree >= 64 saturation    : overflow_serial[c] single-item fallback     │
    │ 3. 4x Substeps: Ballistic ──▶ Color 0..C XPBD Project ──▶ Derive Velocities   │
    └───────────────────────────────────────────────────────────────────────────────┘

~~~vesper
fn solve_xpbd_color_batch_simd[P: IsaProfile](
    inout bodies.{pos_x, pos_y, pos_z}: TiledSeq[RigidBodyTile],
    in bodies.{inv_mass, compliance_alpha}: TiledSeq[RigidBodyTile],
    in batch: ColoredConstraintBatch,
    inv_dt_sq: f32,
) -> Unit eff [DetFloat, Isa[P]] {
    let v_alpha_tilde = batch.load_compliance_vec[P]().det_mul(VVec::splat[P, f32](inv_dt_sq));
    let v_eps = VVec::splat[P, f32](1.0e-9);
    let (c_err, nx, ny, nz, w_sum) = Kinetic::eval_contact_jacobian_simd[P](in bodies, in batch);
    let denom = w_sum.det_add(v_alpha_tilde).det_max(v_eps);
    let d_lambda = c_err.det_neg().det_div(denom);
    Kinetic::scatter_xpbd_positional_deltas[P](inout bodies.{pos_x, pos_y, pos_z}, in batch, d_lambda, nx, ny, nz);
}
fn step_physics_world_deterministic[P: IsaProfile](
    tick: u64,
    inout bodies: TiledSeq[RigidBodyTile],
    dt_fixed: f32,
) -> KineticSnapshotWireHdr eff [DetFloat, Isa[P]] {
    let sub_dt = dt_fixed.det_mul(0.25);
    let inv_sub_dt_sq = 1.0f32.det_div(sub_dt.det_mul(sub_dt));
    let manifold_cnt = Region::scoped(1.mib(), |cap step_arena| {
        let contacts = Kinetic::detect_contacts_morton_lbvh[P](in step_arena, in bodies);
        let colored_layers = Kinetic::color_contact_graph_deterministic(in step_arena, in contacts);
        for _sub in 0..4 {
            Kinetic::integrate_ballistic_substep[P](inout bodies, sub_dt);
            for layer in colored_layers {
                for simd_batch in layer.simd_batches() {
                    solve_xpbd_color_batch_simd[P](
                        inout bodies.{pos_x, pos_y, pos_z},
                        in bodies.{inv_mass, compliance_alpha},
                        in simd_batch,
                        inv_sub_dt_sq,
                    );
                }
            }
            Kinetic::derive_xpbd_velocities[P](inout bodies, sub_dt);
        }
        contacts.len() as u32
    });
    KineticSnapshotWireHdr {
        tick_number: tick.to_le(),
        active_body_count: (bodies.len() as u32).to_le(),
        manifold_count: manifold_cnt.to_le(),
        world_state_blake3: Kinetic::hash_world_canonical_le[P](in bodies),
    }
}
~~~

## 40.4 Check disjointness and spatial ordering

A graph-coloring property can show that two constraints in one batch share no writable body. A Morton code interleaves coordinate bits to group nearby cells in a spatial index. Validating the physical model requires comparing simulated motion with the behavior of the system being modeled.

<!-- technical-detail -->

In `lean/KineticColoring.lean` (`Vesper.KineticColoring`, model `vesper.kinetic.coloring-morton-model/3`), Lean 4 proves pairwise body disjointness both for 2-body greedy coloring (`colorAll_pairwise_disjoint` and `same_color_constraints_disjoint_bodies`, matching `Colorer::color` on contacts and `Ball`/`Hinge`/`Prismatic` joints) and for arbitrary-arity `k`-body batches (`colorAllSets_pairwise_disjoint`, `same_color_sets_disjoint_bodies`, and `colorAll_eq_colorAllSets`, matching `MultiColorer::color` on 1-body ground contacts, 2-body joints, and 4-body soft-body tetrahedra). The same module proves 30-bit 3D Morton `interleave`/`deinterleave` round-trip bijections for 10-bit quantized axes (`morton_10bit_roundtrip`, `morton_10bit_injective`), 63-bit LBVH 21-bit axes (`morton_21bit_roundtrip`, `morton_21bit_injective`), contiguous Karras LBVH prefix ranges (`monotone_keys_prefix_range`), and unique tie-broken sort keys (`augKey_injective` on `code * 2^32 + index`). These proofs cover the pure `Nat` models; differential tests in `crates/vesper-kinetic/tests/lean_model.rs` check the Rust bitmask colorer and BMI2/magic-mask Morton paths on concrete worlds.

    -- lean/KineticColoring.lean (namespace Vesper.KineticColoring)
    theorem same_color_constraints_disjoint_bodies (cs : List Constraint)
        (i j : Nat) (hij : i < j) (hj : j < (colorAll cs).length)
        (hcol : ((colorAll cs)[i]'(Nat.lt_trans hij hj)).2 = ((colorAll cs)[j]'hj).2) :
        Disjoint ((colorAll cs)[i]'(Nat.lt_trans hij hj)).1 ((colorAll cs)[j]'hj).1
    --
    theorem colorAllSets_pairwise_disjoint (cs : List SetConstraint) :
        (colorAllSets cs).Pairwise OkSet
    --
    theorem colorAll_eq_colorAllSets (cs : List Constraint) :
        (colorAll cs).map (·.2) = (colorAllSets (cs.map pairSet)).map (·.2)
    --
    theorem morton_10bit_roundtrip (x y z : Nat)
        (hx : x < 1024) (hy : y < 1024) (hz : z < 1024) :
        deinterleaveX 10 (interleave 10 x y z) = x ∧
        deinterleaveY 10 (interleave 10 x y z) = y ∧
        deinterleaveZ 10 (interleave 10 x y z) = z
    --
    theorem morton_10bit_injective (x y z x' y' z' : Nat)
        (hx : x < 1024) (hy : y < 1024) (hz : z < 1024)
        (hx' : x' < 1024) (hy' : y' < 1024) (hz' : z' < 1024)
        (h : interleave 10 x y z = interleave 10 x' y' z') :
        x = x' ∧ y = y' ∧ z = z'

## 40.5 Inspect the numerical pipeline

A deterministic-float verifier checks the operations allowed by the numerical profile. A Morton encoder supports the broad-phase search for candidate contacts, and the colorer schedules the constraints. For rollback testing, compare saved and replayed states tick by tick to locate the first divergence.

<!-- technical-detail -->

In `crates/vesper-kinetic`, `verify_det_float_ir` audits lowered floating-point instructions under `vesper build --verify-detfloat`, accepting `StrictAddF32`, `StrictMulF32`, `ExplicitHardwareFmaF32`, and `CanonicalNanNormalize` while rejecting `ImplicitContractedFma`, `FastMathReassocReduction`, and `PlatformLibmTranscendental` with span offsets. For broadphase and batching, `morton_encode_3d_30bit` interleaves 10-bit coordinates via magic-mask shifts (`expand_bits_10`), and `color_constraints_deterministic` assigns each `BodyPairConstraint` the lowest free color bit `(!(mask_a | mask_b)).trailing_zeros()` in a 64-color per-body `u64` bitmask, routing any degree-64+ saturation into single-constraint `overflow_serial` layers so determinism and safety hold at arbitrary contact density.

    // crates/vesper-kinetic — DetFloat IR verifier & 64-color bitmask constraint colorer
    pub fn verify_det_float_ir(insts: &[DetFloatInst]) -> Result<(), DetFloatViolation> {
        for inst in insts {
            match *inst {
                DetFloatInst::StrictAddF32
                | DetFloatInst::StrictMulF32
                | DetFloatInst::ExplicitHardwareFmaF32
                | DetFloatInst::CanonicalNanNormalize => {}
                DetFloatInst::ImplicitContractedFma { span_offset } =>
                    return Err(DetFloatViolation::ImplicitFmaContractionForbidden { span_offset }),
                DetFloatInst::FastMathReassocReduction { span_offset } =>
                    return Err(DetFloatViolation::NonAssociativeFastMathForbidden { span_offset }),
                DetFloatInst::PlatformLibmTranscendental { sym, span_offset } =>
                    return Err(DetFloatViolation::NonDeterministicHostLibmCall { sym, span_offset }),
            }
        }
        Ok(())
    }
    // ----
    pub fn color_constraints_deterministic(
        num_bodies: usize,
        constraints: &[BodyPairConstraint],
    ) -> Vec<Vec<BodyPairConstraint>> {
        let mut body_used_colors = vec![0u64; num_bodies];
        let mut layers: Vec<Vec<BodyPairConstraint>> = Vec::new();
        let mut overflow_serial: Vec<BodyPairConstraint> = Vec::new();
        for &c in constraints {
            let mask_a = body_used_colors.get(c.body_a as usize).copied().unwrap_or(0);
            let mask_b = body_used_colors.get(c.body_b as usize).copied().unwrap_or(0);
            let color = (!(mask_a | mask_b)).trailing_zeros() as usize;
            if color < 64 {
                if let Some(slot) = body_used_colors.get_mut(c.body_a as usize) { *slot |= 1u64 << color; }
                if let Some(slot) = body_used_colors.get_mut(c.body_b as usize) { *slot |= 1u64 << color; }
                if layers.len() <= color { layers.resize_with(color + 1, Vec::new); }
                layers[color].push(c);
            } else {
                overflow_serial.push(c);
            }
        }
        for c in overflow_serial { layers.push(vec![c]); }
        layers
    }

# 41 Resonance audio and control loops

An audio callback receives a fixed interval in which to produce its next buffer. Work that waits for a lock or allocates unpredictably can miss that deadline even when its average runtime is small.

<!-- technical-detail -->

A 64-sample audio callback at 48 kHz or a 1 kHz robotics torque loop has roughly one millisecond to read inputs, update filter or controller state, and commit its DMA output buffer. Any heap allocation, mutex wait, unguarded destructor drop, or subnormal floating-point stall on that thread can miss the hardware deadline even when average CPU load is low.

The `@vesper-rt/resonance` design is implemented in `crates/vesper-resonance` alongside stereo/biquad fusion in `crates/vesper-fusion`, verified at compile time with `vesper build --verify-realtime`, and modeled formally in `lean/ResonanceSpsc.lean`, `lean/ResonanceStability.lean`, and `lean/FusionRules.lean`. As cautioned in `REVIEW-STATUS.md`, `@realtime` effect checks and worst-case stack bounds eliminate language-level blocking and allocation hazards inside the callback, but hard real-time deadlines still depend on OS thread scheduling (`SCHED_FIFO`, CoreAudio workgroups, or `PREEMPT_RT`), `mlockall` page residency, and interrupt/DMA isolation on the target machine.

## 41.1 Keep unbounded work outside the callback

A realtime effect policy restricts blocking, allocation, and other unpredictable operations. Prepare new graph state on a control thread and transfer it through a bounded channel. Very small floating-point values can have target-dependent costs, so the design also specifies a subnormal-handling scope.

<!-- technical-detail -->

Decorating an entry point with `@realtime(stack_budget_bytes = 4096, period_us = 1333)` and running `vesper build --verify-realtime` restricts the transitive call graph to the five wait-free capabilities checked by `verify_realtime_effect_row` (`Realtime`, `ConstDenorm`, `RtBus`, `Isa`, and `Mmio`), statically rejecting `Alloc`, `Net`, `Storage`, `Async`, `Scope`, and blocking FFI locks while enforcing Silicon's Stage 5 worst-case stack-depth (WCSD) bound against `stack_budget_bytes`. Entering the quantum activates `eff [ConstDenorm]` (`crates/vesper-resonance/src/denorm.rs`), setting hardware Flush-to-Zero and Denormals-Are-Zero bits (`MXCSR.FTZ|DAZ` on `x86_64`, `FPCR.FZ` on AArch64) and restoring caller FPU flags on exit so decaying IIR tails cannot trigger 50x–100x microcode denormal assists.

To hot-swap live DSP patch banks or trajectory splines without running `GlobalAlloc::dealloc` on the realtime core when reference counts drop to zero, Resonance pairs atomic mailbox swaps with `RtRetirementSink[T]`: retired `affine` values are pushed in O(1) over a pre-allocated wait-free SPSC ring to a non-realtime Vesper Pulse janitor fiber for reclamation. Language-level confinement cannot prevent kernel page faults, SMI pauses, or priority inversion outside the process, so deployment still requires locked pages (`mlock`), dedicated realtime OS priorities, and target WCET measurement.

    ┌──────────────────────────────────────────┐   RtAtomicSlot::swap    ┌───────────────────────────────────┐
    │ Non-Realtime Control / Pulse Thread      │ ──────────────────────▶ │ @realtime Audio / 1 kHz Ctrl Core │
    │ • Compiles Box[VoicePatchBank] & PDC DAG │                         │ • WCSD stack <= 4096 B, 0 Alloc   │
    │ • Pushes 16B RtParamEventWire SPSC queue │ ◀────────────────────── │ • MXCSR.FTZ|DAZ / FPCR.FZ active  │
    │ • Janitor fiber drains RtRetirementSink  │  RtRetirementSink SPSC  │ • Sub-block split: [0..17)|[17..64│
    │   and runs GlobalAlloc::dealloc safely   │  (O(1), 0 syscalls)     │ • Commits linear RtOutputLease    │
    └──────────────────────────────────────────┘                         └───────────────────────────────────┘

## 41.2 Lend a buffer for one processing interval

A linear buffer lease accounts for access to a block of samples. A timestamped event identifies its exact position within the block. A single-producer, single-consumer (SPSC) ring carries parameter updates or retired resources between threads without requiring the callback to perform reclamation.

<!-- technical-detail -->

Resonance represents each hardware DMA period (CoreAudio `IOProc`, PipeWire `memfd`, WASAPI Exclusive, or EtherCAT cycle) as a `linear struct RtOutputLease[Chans: dim, Frames: dim]` that the callback must consume via `RtBus::commit_dma_output` or `RtBus::emergency_silence_and_commit` before returning. Incoming CLAP, MIDI 2.0, or sensor parameter changes arrive through a padding-free 16-byte `wire struct RtParamEventWire` slice carrying the exact `sample_offset_in_block`.

~~~vesper
// 16-byte padding-free sample-accurate automation / MIDI 2.0 / control event:
wire struct RtParamEventWire {
    sample_offset_in_block: u16le,
    node_id: u16le,
    param_id: u32le,
    target_value_f32_bits: u32le,
    ramp_samples: u32le,
}
copy effect Realtime {}
copy effect ConstDenorm {}
linear struct RtOutputLease[Chans: dim, Frames: dim] {
    dma_ptr: DmaSliceMut,
}
copy struct RtRetirementSink[T] {
    ring_slot_base: usize,
}
linear effect RtBus {
    fn poll_param_events(max_events: u32) -> WireSlice[RtParamEventWire];
    fn retire_to_janitor_waitfree[T](in sink_ring: RtRetirementSink[T], sink old_state: T) -> Unit;
    fn commit_dma_output[C: dim, F: dim](sink out_lease: RtOutputLease[C, F]) -> Unit;
    fn emergency_silence_and_commit[C: dim, F: dim](sink out_lease: RtOutputLease[C, F]) -> Unit;
}
~~~

## 41.3 Split a block at an event

Suppose a filter change falls at sample 17 of a 64-sample block. Process the first 17 samples with the old setting, apply the event, and process the remainder with the new setting. The same timing discipline applies to a periodic controller consuming timestamped measurements. Deadlines need measurements on the actual target.

<!-- technical-detail -->

When a filter cutoff or torque setpoint event lands at sample 17 of a 64-sample quantum, `process_realtime_quantum` renders frames `[0..17)` across 16-voice `SvfVoiceTile` AoSoA state in `VVec` registers using the prior coefficients, applies `ev` at sample 17, and renders `[17..64)` before discharging `out_lease`. Any newly swapped `Box[VoicePatchBank]` from `cmd_mailbox` is handed immediately to `RtBus::retire_to_janitor_waitfree` so zero deallocation occurs inside the 4,096-byte stack budget.

~~~vesper
tiled(16) copy struct SvfVoiceTile {
    hot ic1eq: f32,
    hot ic2eq: f32,
    hot g_coeff: f32,
    hot k_damping: f32,
    hot gain: f32,
    cold patch_id: u32,
}
@realtime(stack_budget_bytes = 4096, period_us = 1333)
fn process_realtime_quantum[P: IsaProfile](
    in in_samples: AlignedSlice[f32, 64],
    inout voices.{ic1eq, ic2eq, g_coeff, k_damping, gain}: TiledSeq[SvfVoiceTile],
    inout cmd_mailbox: RtAtomicSlot[Box[VoicePatchBank]],
    in trash_sink: RtRetirementSink[Box[VoicePatchBank]],
    sink out_lease: RtOutputLease[2, 64],
) -> Unit eff [Realtime, ConstDenorm, RtBus, Isa[P]] {
    if let Some(old_bank) = cmd_mailbox.try_swap_in_latest(inout voices) {
        RtBus::retire_to_janitor_waitfree(in trash_sink, sink old_bank);
    }
    let events = RtBus::poll_param_events(32);
    let mut cursor: usize = 0;
    for ev in events {
        let split_at = (ev.sample_offset_in_block.to_native() as usize).min(64);
        if split_at > cursor {
            render_svf_subblock_simd[P](in in_samples, inout voices, inout out_lease, cursor, split_at);
            cursor = split_at;
        }
        apply_voice_param_instant(inout voices, in ev);
    }
    if cursor < 64 {
        render_svf_subblock_simd[P](in in_samples, inout voices, inout out_lease, cursor, 64);
    }
    RtBus::commit_dma_output(sink out_lease);
}
~~~

## 41.4 Align parallel signal paths

If one branch of an audio graph delays its output by several samples, summing it directly with a faster branch mixes different instants. Plugin delay compensation adds the missing delay to the faster path. A ring-safety proof concerns publication and reclamation, while a filter stability argument needs its own signal and coefficient assumptions.

    Branch A    32 samples of processing delay + 32 samples of compensation
    Branch B    64 samples of processing delay +  0 samples of compensation

An impulse sent into both branches now reaches the mixer at sample 64 on each path. Recompute those delays when an edit changes the graph.

<!-- technical-detail -->

Summing parallel DSP branches with unequal lookahead latencies mixes different instants and causes comb-filtering phase cancellation until Plugin Delay Compensation (PDC) inserts `max_arrival - branch_arrival` samples on faster edges:

    Branch A    32 samples of processing delay + 32 samples of compensation
    Branch B    64 samples of processing delay +  0 samples of compensation
    ───────────────────────────────────────────────────────────────────────
                     ┌──▶ [Branch A: 32 samp] ──▶ [PDC Ring: +32 samp] ──┐
    [Dry Input t=0] ─┤                                                   ├──▶ [Sum Mixer: t=64]
                     └──▶ [Branch B: 64 samp] ──▶ [PDC Ring:  +0 samp] ──┘
    Feedback Rule: Unguarded cycle ──▶ Err(FeedbackCycleWithoutUnitDelay); requires explicit z^-1!

Three dedicated Lean 4 modules verify Resonance's concurrency, filter/control, and loop-fusion models without `sorry` or custom axioms. First, `lean/ResonanceSpsc.lean` (`Vesper.ResonanceSpsc`) proves ring occupancy bounds (`spsc_ring_occupancy_bounded`), concurrent slot disjointness across arbitrary producer/consumer start/end interleavings (`spsc_no_shared_slot`), and all-paths DAG phase alignment (`pdc_equalizes_parallel_branch_latency`, `pdc_paths_equal`). Second, `lean/ResonanceStability.lean` (`Vesper.ResonanceStability`) proves discrete trapezoidal TPT SVF Lyapunov BIBO stability (`svf_bibo`, `svf_state_bounded`, `svf_output_bounded` using `V(c) = c1^2 + k * c1 * c2 + c2^2`) and fault-safe PID torque/integrator clamping (`pid_invariant`) over exact `Rat` arithmetic for fixed damping `0 < k < 2` (`Q > 1/2`) and sample-varying cutoff `0 <= g n <= G`; `Rat` does not model IEEE-754 `f32` rounding/NaNs (which Rust turns into typed `none` faults) or time-varying damping `k`. Third, `lean/FusionRules.lean` (`Vesper.FusionRules`) proves `cascade_fused_eq_seq`, showing that fusing chained biquad/DSP sections into one per-sample loop produces the exact output list of sequential whole-block passes over any carrier type.

    -- lean/ResonanceSpsc.lean, lean/ResonanceStability.lean, and lean/FusionRules.lean
    theorem spsc_no_shared_slot {cap : Nat} (hcap : 0 < cap) {s : State} (h : Reachable cap s)
        (p c : Nat) (hp : s.prod = some p) (hc : s.cons = some c) :
        p % s.ring.cap ≠ c % s.ring.cap
    --
    theorem svf_bibo (k G Msq K : Rat) (g x : Nat → Rat) (c : Nat → SvfState)
        (h0 : 0 < k) (h2 : k ≤ 2) (hg : ∀ n, 0 ≤ g n ∧ g n ≤ G) (hx : ∀ n, x n * x n ≤ Msq)
        (hstep : ∀ n, c (n + 1) = svfStep (g n) k (x n) (c n)) (hinit : svfV k (c 0) ≤ K)
        (hK : (2 * (G * G) * (k * k) + 8 * (1 + G * G + G * k)) * Msq ≤ k * k * K) :
        ∀ n, svfV k (c n) ≤ K
    --
    theorem cascade_fused_eq_seq (secs : List (C × σ)) (xs : List α) :
        runFused tick secs xs = runSeq tick secs xs

## 41.5 Compile a bounded processing schedule

A graph compiler orders dependencies, computes compensation delays, and can fuse compatible buffer operations. An effect verifier checks callbacks for operations outside the realtime policy. A cycle needs an explicit delay or feedback rule before a block schedule is meaningful.

<!-- technical-detail -->

In `crates/vesper-resonance`, `verify_realtime_effect_row` enforces the five-effect whitelist (`Realtime`, `ConstDenorm`, `RtBus`, `Isa`, `Mmio`), and `compile_pdc_schedule` topologically sorts `DspNodeSpec` graphs via Kahn's queue, computing `max_in_lat - out_latency[&src]` per input edge while rejecting unguarded feedback loops (`schedule.len() != nodes.len()`) with `RealtimeViolation::FeedbackCycleWithoutUnitDelay` unless broken by an explicit unit-delay node.

    // crates/vesper-resonance — @realtime effect whitelist & topological PDC scheduler
    pub fn verify_realtime_effect_row(
        fn_name: &SmolStr,
        active_effect_names: &[SmolStr],
    ) -> Result<(), RealtimeViolation> {
        const ALLOWED_RT_EFFECTS: &[&str] = &["Realtime", "ConstDenorm", "RtBus", "Isa", "Mmio"];
        for eff in active_effect_names {
            if !ALLOWED_RT_EFFECTS.contains(&eff.as_str()) {
                return Err(RealtimeViolation::ForbiddenBlockingOrAllocEffect {
                    fn_name: fn_name.clone(), offending_effect: eff.clone(),
                });
            }
        }
        Ok(())
    }
    // ----
    pub fn compile_pdc_schedule(nodes: &[DspNodeSpec]) -> Result<Vec<ScheduledDspOp>, RealtimeViolation> {
        // ... Kahn's topological traversal over in_degree == 0 nodes ...
        // For each popped node `id`, max_in_lat = inputs.map(|s| out_latency[s]).max().unwrap_or(0)
        // inserts (src, max_in_lat - out_latency[&src]) and sets out_latency[id] = max_in_lat + intrinsic.
        if schedule.len() != nodes.len() {
            return Err(RealtimeViolation::FeedbackCycleWithoutUnitDelay {
                involved_nodes: nodes.len() - schedule.len(),
            });
        }
        Ok(schedule)
    }

# 42 Tensor shapes, fusion, and the KV cache

During inference, Tensor checks whether the dimensions of a matrix multiply agree. It also tracks leases on pages of cached attention keys and values (the KV cache), so a cancelled request can release its pages.

<!-- technical-detail -->

Autoregressive language-model serving combines fixed architecture dimensions with dynamic sequence lengths, sub-byte quantized weight blocks, and paged key-value (KV) caches that must not leak memory when a client disconnects mid-stream. Tensor encodes static shape equalities in types while tracking every allocated 16-token KV page through a linear sequence lease.

The `@vesper-ml/tensor` subsystem is implemented in `crates/vesper-tensor` (including `GGUF` and `Safetensors` zero-copy mappers, `Q4_0`/`Q4_K`/`FP8`/`MXFP4` quantized formats, GQA attention, and paged KV pools) together with `egglog` rewrite saturation in `crates/vesper-fusion`, and is backed by Lean 4 models in `lean/TensorKv.lean` and `lean/FusionRules.lean`. As documented in `REVIEW-STATUS.md`, symbolic runtime dimensions require explicit boundary checks rather than relying on axis labels alone, and reassociating fusion rules proved over `Rat` hold in IEEE-754 `f32` only within floating-point rounding tolerances.

## 42.1 Unify the contracted dimensions

Multiplying a 2-by-3 matrix by a 3-by-4 matrix gives a 2-by-4 result. The two inner dimensions must agree. Named axes distinguish quantities such as batch, heads, and sequence length even when their sizes happen to match. Unknown dimensions may require a runtime check against the contract.

<!-- technical-detail -->

Tensor parameterizes views as `TensorView[r: Region, DType, Shape]` over named dimension expressions such as `[Batch: dim, Seq: dim, Heads: 32, HeadDim: 128]`. During compilation, `crates/vesper-tensor` unifies contracted inner axes (`lk == rk` in `[M, K]` times `[K, N]` -> `[M, N]`), enforces even rotary-embedding widths (`HeadDim % 2 == 0` for RoPE), verifies grouped-query head divisibility (`QHeads % KvHeads == 0`, such as 32 query heads over 8 KV heads), and checks quantized super-block divisibility (`K % 32 == 0` for `Q4_0`). Whereas static architectural constants (`4096`, `32`, `8`, `128`) discharge at compile time, dynamic dimensions such as prompt length `Seq`, batch size, or tensor shapes read from external `GGUF`/`Safetensors` headers must be validated at load or request-entry boundaries before constructing typed `TensorView` handles.

## 42.2 Interpret a quantized weight block

Quantized formats store compact values together with scales or other decoding information. A mapped tensor view must respect the file’s dimensions, block layout, alignment, and lifetime. A `KvPageLease` instead represents ownership of a page of cached attention keys and values for a live sequence.

<!-- technical-detail -->

Mapped `GGUF` and `Safetensors` files (`Q4_0`, `Q4_K`, `FP8` `E4M3`, and `MXFP4` in `crates/vesper-tensor/src/quant.rs` and `src/formats.rs`) project immutable weights in place through padding-free Vesper Bridge `wire struct` blocks without heap parsing or `unsafe` slice transmutations; for `Q4_0`, `Q4Block32Wire` packs an IEEE `fp16` scale (`u16le`) alongside 32 4-bit weights (`ByteArray[16]`) in exactly 18 bytes, while `Q4_K` packs 256 weights into 144 bytes. Dynamic per-sequence attention history is governed separately by `linear struct KvPageLease[KvHeads: dim, HeadDim: dim]` (one 16-token physical block) owned inside `linear struct ActiveKvSeq`, which `KvCachePool` allocates when `token_pos % 16 == 0` and reclaims via `release_sequence`.

    Q4Block32Wire (18 B, 32 weights):  ┌───────────────┬─────────────────────────────────────────┐
      w[i] = d * (nib[i] - 8)          │ [0..2] fp16 d │ [2..18] 16 B nibbles (lo: 0..15, hi:+16)│
                                       └───────────────┴─────────────────────────────────────────┘
    Q4_K Super-Block (144 B, 256 wts): ┌──────────┬───────────┬──────────────────┬───────────────┐
      w = (d*sc)*q - (dmin*m)          │[0..2] d  │[2..4] dmin│[4..16] 12B 6b s/m│[16..144] 128B │
                                       └──────────┴───────────┴──────────────────┴───────────────┘
    ActiveKvSeq[KvHeads=8, HeadDim=128] & 32:8 GQA PagedAttention Block Table:
      QHeads [0..3] ──▶ KvHead 0 │ Logical Tok [0..16)  ──▶ linear KvPageLease(phys_blk = 41, fill = 16)
      QHeads [4..7] ──▶ KvHead 1 │ Logical Tok [16..32) ──▶ linear KvPageLease(phys_blk =  7, fill =  9)
      ...           ...          │ Exit (EOS / SSE Disconnect / KvOomError) ──▶ release_sequence(sink)

~~~vesper
// Exact 18-byte Q4_0 block (fp16 scale + 32 packed int4 nibbles, 0 implicit padding):
wire struct Q4Block32Wire {
    scale_fp16: u16le,
    packed_nibbles: ByteArray[16],
}
copy struct Bf16 {}
copy struct F32 {}
copy struct Q4_0 {}
copy struct TensorView[r: Region, DType, Shape] {
    data_ptr: RegionPtr[r],
    elem_len: usize,
}
// Mode::Linear 16-token physical KV-cache page and owning sequence table:
linear struct KvPageLease[KvHeads: dim, HeadDim: dim] {
    block_id: u32,
    filled_tokens: u8,
}
linear struct ActiveKvSeq[KvHeads: dim, HeadDim: dim] {
    seq_id: u64,
    token_pos: u32,
    pages: LinearSeq[KvPageLease[KvHeads, HeadDim]],
}
linear effect KvCachePool[KvHeads: dim, HeadDim: dim] {
    fn begin_sequence(seq_id: u64) -> Result[ActiveKvSeq[KvHeads, HeadDim], KvOomError];
    fn append_kv_step[r: Region](
        inout seq: ActiveKvSeq[KvHeads, HeadDim],
        in k_step: TensorView[r, Bf16, [KvHeads, HeadDim]],
        in v_step: TensorView[r, Bf16, [KvHeads, HeadDim]],
    ) -> Result[Unit, KvOomError];
    fn release_sequence(sink seq: ActiveKvSeq[KvHeads, HeadDim]) -> Unit;
}
~~~

## 42.3 Trace one token through attention

A decode step forms queries, keys, and values, appends the new keys and values to the sequence cache, and attends to the retained history. Grouped-query attention lets several query heads share a key/value head. Temporary projections can live in a step arena. Persistent KV pages must survive the step and be returned on completion or cancellation.

In the manuscript example 32 query heads share eight key/value heads in groups of four. Query heads 0 through 3 use key/value head 0, and heads 4 through 7 use head 1. The final group uses head 7, which fits the cache’s eight-head dimension. The next token can reuse those cached keys and values. At the end of the request, the cache must return the pages to the pool.

<!-- technical-detail -->

In `forward_gqa_layer`, 32 query heads share 8 KV heads in 4-to-1 groups (`qh / 4 < 8`) while intermediate projections (`x_norm`, `q`, `k`, `v`, `attn_out`) bump-allocate inside a 2 MiB per-token `Region::scoped(2.mib(), ...)` arena (`Alloc` omitted from `eff [...]`). Surrounding the decode loop, `stream_completion_tokens` holds `kv_seq: ActiveKvSeq[8, 128]`; under Theorem 2 and `[T-Let]`, every exit branch—`EOS_TOKEN_ID` completion, SSE client disconnect, and `KvOomError` failure—must explicitly consume `kv_seq` via `KvCachePool::release_sequence(sink kv_seq)` to compile.

~~~vesper
copy struct TransformerLayerWeights[DModel: dim, QHeads: dim, KvHeads: dim, HeadDim: dim] {
    rms_attn_weight: StaticTensor[F32, [DModel]],
    wq_q4: StaticQuantTensor[Q4_0, [QHeads * HeadDim, DModel]],
    wk_q4: StaticQuantTensor[Q4_0, [KvHeads * HeadDim, DModel]],
    wv_q4: StaticQuantTensor[Q4_0, [KvHeads * HeadDim, DModel]],
    wo_q4: StaticQuantTensor[Q4_0, [DModel, QHeads * HeadDim]],
}
fn forward_gqa_layer[P: IsaProfile, r: Region](
    in scratch: RegionCap[r],
    in weights: TransformerLayerWeights[4096, 32, 8, 128],
    in x_hidden: TensorView[r, F32, [4096]],
    inout kv_seq: ActiveKvSeq[8, 128],
) -> Result[TensorView[r, F32, [4096]], KvOomError] eff [
    r: Region,
    KvCachePool[8, 128],
    Isa[P],
    Fail[KvOomError]
] {
    let x_norm = Tensor::rms_norm[P](in scratch, in x_hidden, in weights.rms_attn_weight, 1.0e-5);
    let q = Tensor::q4_matvec_rope[P, 32, 128](in scratch, in weights.wq_q4, in x_norm, kv_seq.token_pos);
    let k = Tensor::q4_matvec_rope[P, 8, 128](in scratch, in weights.wk_q4, in x_norm, kv_seq.token_pos);
    let v = Tensor::q4_matvec_reshaped[P, 8, 128](in scratch, in weights.wv_q4, in x_norm);
    KvCachePool::append_kv_step(inout kv_seq, in k, in v)?;
    let attn_out: TensorView[r, F32, [4096]] =
        Tensor::paged_flash_decode_gqa[P, 32, 8, 128](in scratch, in q, in kv_seq);
    let proj = Tensor::q4_matvec[P, 4096, 4096](in scratch, in weights.wo_q4, in attn_out);
    Ok(Tensor::residual_add[P](in scratch, in x_hidden, in proj))
}
fn stream_completion_tokens[P: IsaProfile](
    seq_id: u64,
    prompt_tokens: Slice[u32],
    in model: Llama3Weights[4096, 32, 8, 128],
    inout stream: SseTokenResponder,
) -> Result[Unit, InferenceError] eff [KvCachePool[8, 128], Net, Isa[P], Fail[InferenceError]] {
    let mut kv_seq: ActiveKvSeq[8, 128] = KvCachePool::begin_sequence(seq_id)
        .map_err(InferenceError::KvPool)?;
    let mut next_tok = prefill_prompt[P](in model, prompt_tokens, inout kv_seq);
    loop {
        let step_res = Region::scoped(2.mib(), |cap step_arena| {
            decode_next_token[P](in step_arena, in model, next_tok, inout kv_seq)
        });
        match step_res {
            Ok(tok) if tok == EOS_TOKEN_ID => {
                KvCachePool::release_sequence(sink kv_seq);
                return Ok(());
            }
            Ok(tok) => {
                match stream.send_token_checked(tok) {
                    Ok(()) => { next_tok = tok; }
                    Err(_client_disconnected) => {
                        KvCachePool::release_sequence(sink kv_seq);
                        return Ok(());
                    }
                }
            }
            Err(oom_err) => {
                KvCachePool::release_sequence(sink kv_seq);
                Fail::raise(InferenceError::KvPool(oom_err));
            }
        }
    }
}
~~~

## 42.4 Check the shape and the page pool independently

A contraction theorem relates input axes to the output shape. A page-conservation property accounts for every page as free or held by a permitted lease. Streaming softmax updates partial statistics across blocks. Its numerical behavior depends on the chosen arithmetic. For a cancelled request, check that its KV pages return to the pool even if every tensor shape was valid.

<!-- technical-detail -->

In `lean/TensorKv.lean` (`namespace TensorKv`, model `vesper.tensor.kv-model/1`), Lean 4 proves that every pool step (`begin`, `append`, speculative `rollback`, and `release`) preserves `free + active` (`step_conserves`, `run_conserves`), maintains the exact 16-token page-count invariant `s.pages = pagesFor s.tokens` where `pagesFor t = (t + 15) / 16 = ceil(t / 16)` (`run_wf`), reclaims all sequence pages across EOS, client disconnect, and prefill/decode out-of-pages in `stream_completion_conserves_kv_pages`, and keeps GQA head and flat row indices strictly in bounds (`gqa_kv_head_index_in_bounds`, `gqa_flat_index_in_bounds`). Differential tests in `crates/vesper-tensor/tests/lean_kv.rs` check `KvPool` against the Lean transition model on generated operation traces.

Operator-fusion soundness in `lean/FusionRules.lean` (`Vesper.FusionRules`) explicitly separates two semantic regimes. In `section Generic`, carrier-parametric rewrites (`ew_act_refines`, `matvec_epilogue_refines`, and `dequant_into_matmul_refines`) assume zero algebraic laws on `Ops α`, proving the fused kernel executes the exact same operations on the exact same operands per lane—and therefore preserves IEEE-754 `f32` bit-identity when dequantizing `qscale * qcode` in place instead of materializing a dense buffer. By contrast, `section Exact` proves `rmsnorm_prologue_refines` over `Rat` only, because factoring the inverse RMS scalar `inv` outside the row dot product (`inv * sum_c W_c * (w_c * x_c)`) reassociates multiplication and addition and holds in `f32` only within bounded rounding tolerances.

    -- lean/TensorKv.lean (namespace TensorKv) & lean/FusionRules.lean (Vesper.FusionRules)
    def pagesFor (t : Nat) : Nat := (t + 15) / 16
    theorem run_wf : ∀ (p : Pool) (ops : List Op), WF p → WF (run p ops)
    --
    theorem stream_completion_conserves_kv_pages (p : Pool) (id prompt : Nat)
        (events : List Outcome) (fresh : pagesOf id p.seqs = 0) :
        let p' := streamCompletion id prompt events p
        p'.free + p'.active = p.free + p.active ∧ pagesOf id p'.seqs = 0
    --
    theorem gqa_kv_head_index_in_bounds (qHeads kvHeads qh : Nat)
        (h_pos : kvHeads > 0) (h_div : qHeads % kvHeads = 0) (h_qh : qh < qHeads) :
        let groupSize := qHeads / kvHeads
        qh / groupSize < kvHeads
    --
    -- Generic carrier (IEEE-preserving lane-exact):
    theorem matvec_epilogue_refines (m : Mat) (x : T) :
        evalE o env (.dot m x) = evalE o env (.lane (.matvec m x))
    theorem dequant_into_matmul_refines (q : String) (x : T) :
        evalT o env (.matvec (.qview q) x) = evalT o env (.matvec (.deq q) x) ∧
        evalE o env (.dot (.qview q) x) = evalE o env (.dot (.deq q) x)
    -- Exact rationals only (reassociates scalar `inv` across row dot product):
    theorem rmsnorm_prologue_refines (n : Nat) (W w x : Nat → Rat) (inv : Rat) :
        inv * dotR n W (fun c => w c * x c) = dotR n W (fun c => w c * (inv * x c))

## 42.5 Fuse operators while preserving the contract

A shape unifier collects dimension equalities and rejects incompatible contractions. An epilogue fuser can combine operations such as a projection and an activation to avoid writing an intermediate tensor. Any rewrite must preserve output shape, layout, and the numerical guarantees promised for that operator.

<!-- technical-detail -->

In `crates/vesper-tensor`, `check_quantized_matmul` simplifies symbolic `DimExpr` axes under concrete specializations to verify `lk == rk` and `k_val % blk == 0` (`blk = 32` for `Q4_0`), while `check_gqa_rope_dims` verifies `head_dim % 2 == 0` and `q_heads % kv_heads == 0`. Alongside `crates/vesper-fusion`'s `egglog` e-graph saturation, `TensorIr::fuse_epilogues` folds `Q4MatVec { weight, input: RmsNorm { x, weight: norm_weight, eps_bits } }` into `FusedRmsNormQ4MatVec` to eliminate intermediate HBM round-trips when the numerical profile permits prologue/epilogue fusion.

    Unfused HBM Pipeline (3 intermediate memory round-trips):
      [x] ──▶ RMSNorm ──▶ (HBM) ──▶ Dequant(Q4) ──▶ (HBM) ──▶ MatVec ──▶ (HBM) ──▶ SiLU/Add
    Stage 4 egglog + TensorIr Fused Pipeline (L1-register resident):
      ├─ Generic Carrier (IEEE f32 bit-identical): QView(Q4) + Dot + EAct/EAdd fused per lane
      └─ Exact Rat Regime (tolerance-bounded f32): FusedRmsNormQ4MatVec factors inv = rsqrt(ms+eps)

    // crates/vesper-tensor — Symbolic contraction/GQA unifier & TensorIr fusion pass
    pub fn check_quantized_matmul(
        m: &DimExpr, lhs_k: &DimExpr, rhs_k: &DimExpr, n: &DimExpr,
        quant_block_size: Option<u64>, env: &FxHashMap<SmolStr, u64>,
    ) -> Result<(DimExpr, DimExpr), ShapeError> {
        let lk = lhs_k.simplify(env);
        let rk = rhs_k.simplify(env);
        if lk != rk { return Err(ShapeError::ContractionMismatch { lhs_k: lk, rhs_k: rk }); }
        if let (Some(blk), DimExpr::Const(k_val)) = (quant_block_size, &lk) {
            if *k_val % blk != 0 {
                return Err(ShapeError::QuantBlockAlignment { k_dim: *k_val, block_size: blk });
            }
        }
        Ok((m.simplify(env), n.simplify(env)))
    }
    // ----
    pub fn check_gqa_rope_dims(q_heads: u64, kv_heads: u64, head_dim: u64) -> Result<u64, ShapeError> {
        if head_dim == 0 || head_dim % 2 != 0 { return Err(ShapeError::OddRopeHeadDim { head_dim }); }
        if kv_heads == 0 || q_heads % kv_heads != 0 {
            return Err(ShapeError::InvalidGqaHeadRatio { q_heads, kv_heads });
        }
        Ok(q_heads / kv_heads)
    }
    // ----
    impl TensorIr {
        pub fn fuse_epilogues(self) -> Self {
            match self {
                Self::Q4MatVec { weight, input } => match input.fuse_epilogues() {
                    Self::RmsNorm { x, weight: norm_weight, eps_bits } =>
                        Self::FusedRmsNormQ4MatVec { weight, norm_weight, x, eps_bits },
                    other => Self::Q4MatVec { weight, input: Box::new(other) },
                },
                Self::RmsNorm { x, weight, eps_bits } =>
                    Self::RmsNorm { x: Box::new(x.fuse_epilogues()), weight, eps_bits },
                Self::ResidualAdd { a, b } =>
                    Self::ResidualAdd { a: Box::new(a.fuse_epilogues()), b: Box::new(b.fuse_epilogues()) },
                leaf => leaf,
            }
        }
    }
