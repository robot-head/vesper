-- Vesper Ignis (§38.10): Tri-backend barrier hazard preservation across
-- DirectX 12 Enhanced Barriers, Vulkan 1.4 Synchronization2 (plus split
-- VkEvents), and Metal 4 stage-to-stage barriers (including TBDR layout-only
-- elision).
--
-- What is proved, and about which model:
-- * `compileIgnisRenderGraph` constructs finite hazard DAGs across six
--   `RenderFeatureFlags` profiles (raster-only, raster+post-compute,
--   meshlet+raytrace+present, async compute shadow copy, split-event G-Buffer
--   lighting, and TBDR tile fusion), with explicit `SyncScope` and `AccessScope`.
-- * `lowerBarriers` maps each `HazardEdge` to DirectX 12 enhanced barriers
--   (sync + access scopes), Vulkan 1.4 `VkImageMemoryBarrier2` or paired split
--   `VkEvent` handles (sync + access scopes, matching `SYNC_TABLE`/`ACCESS_TABLE`
--   in `crates/vesper-ignis-vulkan/src/translate.rs`, including
--   `VERTEX_ATTRIBUTE_INPUT`), or Metal 4 stage barriers (eliding layout-only
--   edges while preserving all data hazards).
-- * `tri_backend_barriers_preserve_hazards` discharges the §38.10 manuscript
--   theorem by `cases cfg <;> decide`.
-- * `lowerEdge_preserves_edge`, `lowerBarriers_sound`, and
--   `tri_backend_barriers_preserve_any_dag` generalize hazard preservation to
--   arbitrary well-formed `List HazardEdge` DAGs.
-- * Regression theorems verify that stage-only barriers (`srcAccessMask = 0,
--   dstAccessMask = 0`), read-only access barriers on UAV WAW hazards, and
--   `VERTEX_SHADER`-only barriers missing `VERTEX_ATTRIBUTE_INPUT` are rejected.
--
-- No `sorry`, no `admit`, and no custom axioms are used.

namespace Vesper.IgnisTriBackendEquiv

inductive GpuBackend where
  | d3d12
  | vulkan14
  | metal4
  deriving DecidableEq, Repr

inductive SyncScope where
  | all
  | vertexShading
  | pixelShading
  | depthStencil
  | renderTarget
  | computeShading
  | raytracing
  | copy
  | allShading
  deriving DecidableEq, Repr

inductive AccessScope where
  | noAccess
  | vertexBuffer
  | constantBuffer
  | indexBuffer
  | renderTarget
  | unorderedAccess
  | depthStencilWrite
  | depthStencilRead
  | shaderResource
  | indirectArgument
  | copyDest
  | copySource
  deriving DecidableEq, Repr

inductive LayoutKind where
  | undefined
  | renderTarget
  | depthStencilWrite
  | shaderRead
  | unorderedAccess
  | copySource
  | copyDest
  | present
  deriving DecidableEq, Repr

inductive HazardKind where
  | readAfterWrite
  | writeAfterRead
  | writeAfterWrite
  | layoutOnly
  deriving DecidableEq, Repr

def HazardKind.isDataHazard : HazardKind → Bool
  | .layoutOnly => false
  | _ => true

inductive RenderFeatureFlags where
  | rasterOnly
  | rasterPostCompute
  | meshletRaytracePresent
  | asyncComputeShadowCopy
  | splitEventGbufferLighting
  | tbdrTileFusion
  deriving DecidableEq, Repr

structure HazardEdge where
  beforePass : Nat
  afterPass : Nat
  resourceId : Nat
  kind : HazardKind
  beforeSync : SyncScope
  afterSync : SyncScope
  beforeAccess : AccessScope
  afterAccess : AccessScope
  beforeLayout : LayoutKind
  afterLayout : LayoutKind
  deriving DecidableEq, Repr

def HazardEdge.hasValidAccess (e : HazardEdge) : Bool :=
  !e.kind.isDataHazard || (e.beforeAccess != .noAccess && e.afterAccess != .noAccess)

inductive NativeBarrier where
  | d3d12 (beforePass afterPass resourceId : Nat)
      (syncBefore syncAfter accessBefore accessAfter : Nat)
      (layoutBefore layoutAfter : LayoutKind)
  | vulkan14ImageBarrier2 (beforePass afterPass resourceId : Nat)
      (srcStageMask dstStageMask srcAccessMask dstAccessMask : Nat)
      (oldLayout newLayout : LayoutKind)
  | vulkan14SplitEvent (signalPass waitPass resourceId : Nat)
      (srcStageMask dstStageMask srcAccessMask dstAccessMask : Nat)
      (oldLayout newLayout : LayoutKind)
  | metal4StageBarrier (beforePass afterPass resourceId : Nat)
      (afterStages beforeStages : Nat) (elidedLayoutOnly : Bool)
  deriving DecidableEq, Repr

/-- Exact D3D12_BARRIER_SYNC bit values from `crates/vesper-ignis/src/d3d12.rs`. -/
def d3d12SyncBits : SyncScope → Nat
  | .all => 0x1
  | .vertexShading => 0x8
  | .pixelShading => 0x10
  | .depthStencil => 0x20
  | .renderTarget => 0x40
  | .computeShading => 0x80
  | .raytracing => 0x100
  | .copy => 0x200
  | .allShading => 0x1000

/-- Exact D3D12_BARRIER_ACCESS bit values from `crates/vesper-ignis/src/d3d12.rs`. -/
def d3d12AccessBits : AccessScope → Nat
  | .noAccess => 0x80000000
  | .vertexBuffer => 0x1
  | .constantBuffer => 0x2
  | .indexBuffer => 0x4
  | .renderTarget => 0x8
  | .unorderedAccess => 0x10
  | .depthStencilWrite => 0x20
  | .depthStencilRead => 0x40
  | .shaderResource => 0x80
  | .indirectArgument => 0x200
  | .copyDest => 0x400
  | .copySource => 0x800

/-- Vulkan 1.4 `VkPipelineStageFlags2` masks matching
    `crates/vesper-ignis-vulkan/src/translate.rs::SYNC_TABLE` (including
    `VERTEX_ATTRIBUTE_INPUT = 0x2000000000` in `.vertexShading` and `.allShading`). -/
def vkStageMask : SyncScope → Nat
  | .all => 0x10000
  | .vertexShading => 0x2000000008
  | .pixelShading => 0x80
  | .depthStencil => 0x300
  | .renderTarget => 0x400
  | .computeShading => 0x800
  | .raytracing => 0x200000
  | .copy => 0x900000000
  | .allShading => 0x6000000880

/-- Vulkan 1.4 `VkAccessFlags2` masks matching
    `crates/vesper-ignis-vulkan/src/translate.rs::ACCESS_TABLE`. -/
def vkAccessMask : AccessScope → Nat
  | .noAccess => 0x0
  | .vertexBuffer => 0x4
  | .constantBuffer => 0x8
  | .indexBuffer => 0x2
  | .renderTarget => 0x180
  | .unorderedAccess => 0x600000000
  | .depthStencilWrite => 0x600
  | .depthStencilRead => 0x200
  | .shaderResource => 0x300000000
  | .indirectArgument => 0x1
  | .copyDest => 0x1000
  | .copySource => 0x800

/-- Metal 4 `MTL4Stages` bitmask model. -/
def mtl4StageMask : SyncScope → Nat
  | .all => 0x3f
  | .vertexShading => 0x1
  | .pixelShading => 0x2
  | .depthStencil => 0x2
  | .renderTarget => 0x4
  | .computeShading => 0x8
  | .raytracing => 0x20
  | .copy => 0x10
  | .allShading => 0xf

def compileIgnisRenderGraph : RenderFeatureFlags → List HazardEdge
  | .rasterOnly =>
      [ { beforePass := 0, afterPass := 1, resourceId := 0
          kind := .layoutOnly, beforeSync := .all, afterSync := .renderTarget
          beforeAccess := .noAccess, afterAccess := .renderTarget
          beforeLayout := .undefined, afterLayout := .renderTarget }
      , { beforePass := 1, afterPass := 2, resourceId := 0
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .pixelShading
          beforeAccess := .renderTarget, afterAccess := .shaderResource
          beforeLayout := .renderTarget, afterLayout := .present } ]
  | .rasterPostCompute =>
      [ { beforePass := 0, afterPass := 1, resourceId := 0
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .computeShading
          beforeAccess := .renderTarget, afterAccess := .shaderResource
          beforeLayout := .renderTarget, afterLayout := .shaderRead }
      , { beforePass := 1, afterPass := 2, resourceId := 1
          kind := .writeAfterWrite, beforeSync := .computeShading, afterSync := .computeShading
          beforeAccess := .unorderedAccess, afterAccess := .unorderedAccess
          beforeLayout := .unorderedAccess, afterLayout := .unorderedAccess }
      , { beforePass := 2, afterPass := 3, resourceId := 1
          kind := .readAfterWrite, beforeSync := .computeShading, afterSync := .pixelShading
          beforeAccess := .unorderedAccess, afterAccess := .shaderResource
          beforeLayout := .unorderedAccess, afterLayout := .present } ]
  | .meshletRaytracePresent =>
      [ { beforePass := 0, afterPass := 1, resourceId := 0
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .raytracing
          beforeAccess := .renderTarget, afterAccess := .shaderResource
          beforeLayout := .renderTarget, afterLayout := .shaderRead }
      , { beforePass := 1, afterPass := 2, resourceId := 2
          kind := .readAfterWrite, beforeSync := .raytracing, afterSync := .pixelShading
          beforeAccess := .unorderedAccess, afterAccess := .shaderResource
          beforeLayout := .unorderedAccess, afterLayout := .shaderRead }
      , { beforePass := 2, afterPass := 3, resourceId := 3
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .all
          beforeAccess := .renderTarget, afterAccess := .shaderResource
          beforeLayout := .renderTarget, afterLayout := .present } ]
  | .asyncComputeShadowCopy =>
      [ { beforePass := 0, afterPass := 1, resourceId := 4
          kind := .readAfterWrite, beforeSync := .copy, afterSync := .computeShading
          beforeAccess := .copyDest, afterAccess := .shaderResource
          beforeLayout := .copyDest, afterLayout := .shaderRead }
      , { beforePass := 1, afterPass := 2, resourceId := 4
          kind := .writeAfterRead, beforeSync := .computeShading, afterSync := .copy
          beforeAccess := .shaderResource, afterAccess := .copyDest
          beforeLayout := .shaderRead, afterLayout := .copyDest } ]
  | .splitEventGbufferLighting =>
      [ { beforePass := 0, afterPass := 4, resourceId := 5
          kind := .readAfterWrite, beforeSync := .depthStencil, afterSync := .raytracing
          beforeAccess := .depthStencilWrite, afterAccess := .shaderResource
          beforeLayout := .shaderRead, afterLayout := .shaderRead }
      , { beforePass := 1, afterPass := 4, resourceId := 6
          kind := .writeAfterWrite, beforeSync := .computeShading, afterSync := .computeShading
          beforeAccess := .unorderedAccess, afterAccess := .unorderedAccess
          beforeLayout := .unorderedAccess, afterLayout := .unorderedAccess } ]
  | .tbdrTileFusion =>
      [ { beforePass := 0, afterPass := 1, resourceId := 7
          kind := .layoutOnly, beforeSync := .all, afterSync := .renderTarget
          beforeAccess := .noAccess, afterAccess := .renderTarget
          beforeLayout := .undefined, afterLayout := .renderTarget }
      , { beforePass := 1, afterPass := 2, resourceId := 7
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .pixelShading
          beforeAccess := .renderTarget, afterAccess := .shaderResource
          beforeLayout := .renderTarget, afterLayout := .shaderRead } ]

def isSplitEligible (ol nl : LayoutKind) (bp ap : Nat) : Bool :=
  ol == nl && decide (bp + 3 <= ap)

def lowerEdge : GpuBackend → HazardEdge → NativeBarrier
  | .d3d12, e =>
      .d3d12 e.beforePass e.afterPass e.resourceId
        (d3d12SyncBits e.beforeSync) (d3d12SyncBits e.afterSync)
        (d3d12AccessBits e.beforeAccess) (d3d12AccessBits e.afterAccess)
        e.beforeLayout e.afterLayout
  | .vulkan14, e =>
      if isSplitEligible e.beforeLayout e.afterLayout e.beforePass e.afterPass then
        .vulkan14SplitEvent e.beforePass e.afterPass e.resourceId
          (vkStageMask e.beforeSync) (vkStageMask e.afterSync)
          (vkAccessMask e.beforeAccess) (vkAccessMask e.afterAccess)
          e.beforeLayout e.afterLayout
      else
        .vulkan14ImageBarrier2 e.beforePass e.afterPass e.resourceId
          (vkStageMask e.beforeSync) (vkStageMask e.afterSync)
          (vkAccessMask e.beforeAccess) (vkAccessMask e.afterAccess)
          e.beforeLayout e.afterLayout
  | .metal4, e =>
      if e.kind.isDataHazard then
        .metal4StageBarrier e.beforePass e.afterPass e.resourceId
          (mtl4StageMask e.beforeSync) (mtl4StageMask e.afterSync) false
      else
        .metal4StageBarrier e.beforePass e.afterPass e.resourceId 0 0 true

def lowerBarriers (backend : GpuBackend) (dag : List HazardEdge) : List NativeBarrier :=
  dag.map (lowerEdge backend)

def edgePreservedBy (e : HazardEdge) (b : NativeBarrier) : Bool :=
  match b with
  | .d3d12 bp ap rid sb sa ab aa lb la =>
      bp == e.beforePass && ap == e.afterPass && rid == e.resourceId &&
      lb == e.beforeLayout && la == e.afterLayout &&
      (!e.kind.isDataHazard ||
        (sb == d3d12SyncBits e.beforeSync && sa == d3d12SyncBits e.afterSync &&
         sb != 0 && sa != 0 &&
         (ab &&& d3d12AccessBits e.beforeAccess) == d3d12AccessBits e.beforeAccess &&
         (aa &&& d3d12AccessBits e.afterAccess) == d3d12AccessBits e.afterAccess &&
         ab != d3d12AccessBits .noAccess && aa != d3d12AccessBits .noAccess))
  | .vulkan14ImageBarrier2 bp ap rid ss ds sa da ol nl =>
      bp == e.beforePass && ap == e.afterPass && rid == e.resourceId &&
      ol == e.beforeLayout && nl == e.afterLayout &&
      (!e.kind.isDataHazard ||
        ((ss &&& vkStageMask e.beforeSync) == vkStageMask e.beforeSync &&
         (ds &&& vkStageMask e.afterSync) == vkStageMask e.afterSync &&
         ss != 0 && ds != 0 &&
         (sa &&& vkAccessMask e.beforeAccess) == vkAccessMask e.beforeAccess &&
         (da &&& vkAccessMask e.afterAccess) == vkAccessMask e.afterAccess &&
         sa != 0 && da != 0))
  | .vulkan14SplitEvent sp wp rid ss ds sa da ol nl =>
      sp == e.beforePass && wp == e.afterPass && rid == e.resourceId &&
      ol == e.beforeLayout && nl == e.afterLayout &&
      isSplitEligible ol nl sp wp &&
      (!e.kind.isDataHazard ||
        ((ss &&& vkStageMask e.beforeSync) == vkStageMask e.beforeSync &&
         (ds &&& vkStageMask e.afterSync) == vkStageMask e.afterSync &&
         ss != 0 && ds != 0 &&
         (sa &&& vkAccessMask e.beforeAccess) == vkAccessMask e.beforeAccess &&
         (da &&& vkAccessMask e.afterAccess) == vkAccessMask e.afterAccess &&
         sa != 0 && da != 0))
  | .metal4StageBarrier bp ap rid as bs elided =>
      bp == e.beforePass && ap == e.afterPass && rid == e.resourceId &&
      (if e.kind.isDataHazard then
         !elided && as == mtl4StageMask e.beforeSync && bs == mtl4StageMask e.afterSync &&
         as != 0 && bs != 0
       else
         elided == true)

def happensBeforeSoundB (dag : List HazardEdge) (barriers : List NativeBarrier) : Bool :=
  dag.all (fun e => barriers.any (fun b => edgePreservedBy e b))

abbrev HappensBeforeSound (dag : List HazardEdge) (barriers : List NativeBarrier) : Prop :=
  happensBeforeSoundB dag barriers = true

theorem d3d12SyncBits_ne_zero (s : SyncScope) : (d3d12SyncBits s != 0) = true := by
  cases s <;> decide

theorem vkStageMask_ne_zero (s : SyncScope) : (vkStageMask s != 0) = true := by
  cases s <;> decide

theorem vkStageMask_covers_self (s : SyncScope) :
    ((vkStageMask s &&& vkStageMask s) == vkStageMask s) = true := by
  cases s <;> decide

theorem d3d12AccessBits_covers_self (a : AccessScope) :
    ((d3d12AccessBits a &&& d3d12AccessBits a) == d3d12AccessBits a) = true := by
  cases a <;> decide

theorem vkAccessMask_covers_self (a : AccessScope) :
    ((vkAccessMask a &&& vkAccessMask a) == vkAccessMask a) = true := by
  cases a <;> decide

theorem d3d12AccessBits_ne_noAccess {a : AccessScope} (h : (a != .noAccess) = true) :
    (d3d12AccessBits a != d3d12AccessBits .noAccess) = true := by
  cases a with
  | noAccess => cases h
  | _ => decide

theorem vkAccessMask_ne_zero_of_ne_noAccess {a : AccessScope} (h : (a != .noAccess) = true) :
    (vkAccessMask a != 0) = true := by
  cases a with
  | noAccess => cases h
  | _ => decide

theorem mtl4StageMask_ne_zero (s : SyncScope) : (mtl4StageMask s != 0) = true := by
  cases s <;> decide

theorem layoutKind_beq_self (l : LayoutKind) : (l == l) = true := by
  cases l <;> rfl

theorem compileIgnisRenderGraph_valid (cfg : RenderFeatureFlags) :
    (compileIgnisRenderGraph cfg).all HazardEdge.hasValidAccess = true := by
  cases cfg <;> decide

/-- §38.10 canonical decidable theorem across all modeled render-graph configurations. -/
theorem tri_backend_barriers_preserve_hazards (cfg : RenderFeatureFlags) :
    let dag := compileIgnisRenderGraph cfg
    HappensBeforeSound dag (lowerBarriers .d3d12 dag) ∧
    HappensBeforeSound dag (lowerBarriers .vulkan14 dag) ∧
    HappensBeforeSound dag (lowerBarriers .metal4 dag) := by
  cases cfg <;> decide

/-- General single-edge hazard preservation across DirectX 12, Vulkan 1.4, and Metal 4
    for well-formed edges carrying valid access scopes on data hazards. -/
theorem lowerEdge_preserves_edge (backend : GpuBackend) (e : HazardEdge)
    (h_valid : e.hasValidAccess = true) :
    edgePreservedBy e (lowerEdge backend e) = true := by
  cases hk : e.kind.isDataHazard with
  | false =>
      cases backend with
      | d3d12 =>
          simp [lowerEdge, edgePreservedBy, hk]
      | vulkan14 =>
          cases hsplit : isSplitEligible e.beforeLayout e.afterLayout e.beforePass e.afterPass <;>
            simp [lowerEdge, hsplit, edgePreservedBy, hk]
      | metal4 =>
          simp [lowerEdge, edgePreservedBy, hk]
  | true =>
      have h_acc : e.beforeAccess ≠ .noAccess ∧ e.afterAccess ≠ .noAccess := by
        simpa [HazardEdge.hasValidAccess, hk] using h_valid
      have h_ba : (e.beforeAccess != .noAccess) = true := bne_iff_ne.mpr h_acc.1
      have h_aa : (e.afterAccess != .noAccess) = true := bne_iff_ne.mpr h_acc.2
      cases backend with
      | d3d12 =>
          simp [lowerEdge, edgePreservedBy, hk, d3d12SyncBits_ne_zero,
            d3d12AccessBits_ne_noAccess h_ba, d3d12AccessBits_ne_noAccess h_aa]
      | vulkan14 =>
          cases hsplit : isSplitEligible e.beforeLayout e.afterLayout e.beforePass e.afterPass <;>
            simp [lowerEdge, hsplit, edgePreservedBy, hk, vkStageMask_ne_zero,
              vkAccessMask_ne_zero_of_ne_noAccess h_ba,
              vkAccessMask_ne_zero_of_ne_noAccess h_aa]
      | metal4 =>
          simp [lowerEdge, edgePreservedBy, hk, mtl4StageMask_ne_zero]

/-- Arbitrary DAG hazard preservation theorem for any backend. -/
theorem lowerBarriers_sound (backend : GpuBackend) (dag : List HazardEdge)
    (h_valid : dag.all HazardEdge.hasValidAccess = true) :
    HappensBeforeSound dag (lowerBarriers backend dag) := by
  simp only [HappensBeforeSound, happensBeforeSoundB, List.all_eq_true]
  intro e he
  have h_e : e.hasValidAccess = true := (List.all_eq_true.mp h_valid) e he
  simp only [List.any_eq_true, lowerBarriers, List.mem_map]
  exact ⟨lowerEdge backend e, ⟨e, he, rfl⟩, lowerEdge_preserves_edge backend e h_e⟩

/-- Arbitrary DAG tri-backend equivalence corollary. -/
theorem tri_backend_barriers_preserve_any_dag (dag : List HazardEdge)
    (h_valid : dag.all HazardEdge.hasValidAccess = true) :
    HappensBeforeSound dag (lowerBarriers .d3d12 dag) ∧
    HappensBeforeSound dag (lowerBarriers .vulkan14 dag) ∧
    HappensBeforeSound dag (lowerBarriers .metal4 dag) :=
  ⟨lowerBarriers_sound .d3d12 dag h_valid,
   lowerBarriers_sound .vulkan14 dag h_valid,
   lowerBarriers_sound .metal4 dag h_valid⟩

/-- Regression check: a stage-only Vulkan barrier (`srcAccessMask = 0, dstAccessMask = 0`)
    is rejected on the same-layout compute WAW edge in `rasterPostCompute`. -/
theorem stage_only_compute_waw_rejected :
    let computeWaw : HazardEdge :=
      { beforePass := 1, afterPass := 2, resourceId := 1
      , kind := .writeAfterWrite
      , beforeSync := .computeShading, afterSync := .computeShading
      , beforeAccess := .unorderedAccess, afterAccess := .unorderedAccess
      , beforeLayout := .unorderedAccess, afterLayout := .unorderedAccess }
    let stageOnlyBarrier : NativeBarrier :=
      .vulkan14ImageBarrier2 1 2 1 0x800 0x800 0 0 .unorderedAccess .unorderedAccess
    edgePreservedBy computeWaw stageOnlyBarrier = false := by
  decide

/-- Regression check: a Vulkan barrier carrying only `SHADER_STORAGE_READ` (`0x200000000`)
    without `SHADER_STORAGE_WRITE` (`0x400000000`) is rejected on a UAV WAW hazard. -/
theorem read_only_access_compute_waw_rejected :
    let computeWaw : HazardEdge :=
      { beforePass := 1, afterPass := 2, resourceId := 1
      , kind := .writeAfterWrite
      , beforeSync := .computeShading, afterSync := .computeShading
      , beforeAccess := .unorderedAccess, afterAccess := .unorderedAccess
      , beforeLayout := .unorderedAccess, afterLayout := .unorderedAccess }
    let readOnlyBarrier : NativeBarrier :=
      .vulkan14ImageBarrier2 1 2 1 0x800 0x800 0x200000000 0x200000000
        .unorderedAccess .unorderedAccess
    edgePreservedBy computeWaw readOnlyBarrier = false := by
  decide

/-- Regression check: Vulkan `.vertexShading` and `.allShading` masks include
    `VERTEX_ATTRIBUTE_INPUT` (`0x2000000000`), and a `VERTEX_SHADER`-only barrier (`0x8`)
    omitting vertex-attribute input is rejected on a vertex-buffer read hazard. -/
theorem vk_shading_masks_cover_vertex_attribute_input :
    ((vkStageMask .vertexShading &&& 0x2000000000) == 0x2000000000) = true ∧
    ((vkStageMask .allShading &&& 0x2000000000) == 0x2000000000) = true := by
  decide

theorem vertex_shader_only_missing_attribute_input_rejected :
    let vbRead : HazardEdge :=
      { beforePass := 0, afterPass := 1, resourceId := 9
      , kind := .readAfterWrite
      , beforeSync := .copy, afterSync := .vertexShading
      , beforeAccess := .copyDest, afterAccess := .vertexBuffer
      , beforeLayout := .copyDest, afterLayout := .shaderRead }
    let missingAttrInput : NativeBarrier :=
      .vulkan14ImageBarrier2 0 1 9 0x900000000 0x8 0x1000 0x4 .copyDest .shaderRead
    edgePreservedBy vbRead missingAttrInput = false := by
  decide

end Vesper.IgnisTriBackendEquiv

#print axioms Vesper.IgnisTriBackendEquiv.tri_backend_barriers_preserve_hazards
#print axioms Vesper.IgnisTriBackendEquiv.lowerEdge_preserves_edge
#print axioms Vesper.IgnisTriBackendEquiv.lowerBarriers_sound
#print axioms Vesper.IgnisTriBackendEquiv.tri_backend_barriers_preserve_any_dag
#print axioms Vesper.IgnisTriBackendEquiv.stage_only_compute_waw_rejected
#print axioms Vesper.IgnisTriBackendEquiv.read_only_access_compute_waw_rejected
#print axioms Vesper.IgnisTriBackendEquiv.vk_shading_masks_cover_vertex_attribute_input
#print axioms Vesper.IgnisTriBackendEquiv.vertex_shader_only_missing_attribute_input_rejected
