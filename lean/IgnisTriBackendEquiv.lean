-- Vesper Ignis (§38.10): Tri-backend barrier hazard preservation across
-- DirectX 12 Enhanced Barriers, Vulkan 1.4 Synchronization2 (plus split
-- VkEvents), and Metal 4 stage-to-stage barriers (including TBDR layout-only
-- elision).
--
-- What is proved, and about which model:
-- * `compileIgnisRenderGraph` constructs finite hazard DAGs across six
--   `RenderFeatureFlags` profiles (raster-only, raster+post-compute,
--   meshlet+raytrace+present, async compute shadow copy, split-event G-Buffer
--   lighting, and TBDR tile fusion).
-- * `lowerBarriers` maps each `HazardEdge` to DirectX 12 enhanced barriers,
--   Vulkan 1.4 `VkImageMemoryBarrier2` or paired split `VkEvent` handles (when
--   layouts match and pass separation is at least 3 passes), or Metal 4 stage
--   barriers (eliding layout-only edges while preserving all data hazards).
-- * `tri_backend_barriers_preserve_hazards` discharges the §38.10 manuscript
--   theorem by `cases cfg <;> decide`.
-- * `lowerEdge_preserves_edge`, `lowerBarriers_sound`, and
--   `tri_backend_barriers_preserve_any_dag` generalize hazard preservation to
--   arbitrary `List HazardEdge` DAGs.
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
  beforeLayout : LayoutKind
  afterLayout : LayoutKind
  deriving DecidableEq, Repr

inductive NativeBarrier where
  | d3d12 (beforePass afterPass resourceId : Nat) (syncBefore syncAfter : Nat)
      (layoutBefore layoutAfter : LayoutKind)
  | vulkan14ImageBarrier2 (beforePass afterPass resourceId : Nat)
      (srcStageMask dstStageMask : Nat) (oldLayout newLayout : LayoutKind)
  | vulkan14SplitEvent (signalPass waitPass resourceId : Nat)
      (srcStageMask dstStageMask : Nat) (oldLayout newLayout : LayoutKind)
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

/-- Vulkan 1.4 `VkPipelineStageFlags2` lower-bit stage masks. -/
def vkStageMask : SyncScope → Nat
  | .all => 0x10000
  | .vertexShading => 0x8
  | .pixelShading => 0x80
  | .depthStencil => 0x300
  | .renderTarget => 0x400
  | .computeShading => 0x800
  | .raytracing => 0x200000
  | .copy => 0x1000
  | .allShading => 0x888

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
          beforeLayout := .undefined, afterLayout := .renderTarget }
      , { beforePass := 1, afterPass := 2, resourceId := 0
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .pixelShading
          beforeLayout := .renderTarget, afterLayout := .present } ]
  | .rasterPostCompute =>
      [ { beforePass := 0, afterPass := 1, resourceId := 0
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .computeShading
          beforeLayout := .renderTarget, afterLayout := .shaderRead }
      , { beforePass := 1, afterPass := 2, resourceId := 1
          kind := .writeAfterWrite, beforeSync := .computeShading, afterSync := .computeShading
          beforeLayout := .unorderedAccess, afterLayout := .unorderedAccess }
      , { beforePass := 2, afterPass := 3, resourceId := 1
          kind := .readAfterWrite, beforeSync := .computeShading, afterSync := .pixelShading
          beforeLayout := .unorderedAccess, afterLayout := .present } ]
  | .meshletRaytracePresent =>
      [ { beforePass := 0, afterPass := 1, resourceId := 0
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .raytracing
          beforeLayout := .renderTarget, afterLayout := .shaderRead }
      , { beforePass := 1, afterPass := 2, resourceId := 2
          kind := .readAfterWrite, beforeSync := .raytracing, afterSync := .pixelShading
          beforeLayout := .unorderedAccess, afterLayout := .shaderRead }
      , { beforePass := 2, afterPass := 3, resourceId := 3
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .all
          beforeLayout := .renderTarget, afterLayout := .present } ]
  | .asyncComputeShadowCopy =>
      [ { beforePass := 0, afterPass := 1, resourceId := 4
          kind := .readAfterWrite, beforeSync := .copy, afterSync := .computeShading
          beforeLayout := .copyDest, afterLayout := .shaderRead }
      , { beforePass := 1, afterPass := 2, resourceId := 4
          kind := .writeAfterRead, beforeSync := .computeShading, afterSync := .copy
          beforeLayout := .shaderRead, afterLayout := .copyDest } ]
  | .splitEventGbufferLighting =>
      [ { beforePass := 0, afterPass := 4, resourceId := 5
          kind := .readAfterWrite, beforeSync := .depthStencil, afterSync := .raytracing
          beforeLayout := .shaderRead, afterLayout := .shaderRead }
      , { beforePass := 1, afterPass := 4, resourceId := 6
          kind := .writeAfterWrite, beforeSync := .computeShading, afterSync := .computeShading
          beforeLayout := .unorderedAccess, afterLayout := .unorderedAccess } ]
  | .tbdrTileFusion =>
      [ { beforePass := 0, afterPass := 1, resourceId := 7
          kind := .layoutOnly, beforeSync := .all, afterSync := .renderTarget
          beforeLayout := .undefined, afterLayout := .renderTarget }
      , { beforePass := 1, afterPass := 2, resourceId := 7
          kind := .readAfterWrite, beforeSync := .renderTarget, afterSync := .pixelShading
          beforeLayout := .renderTarget, afterLayout := .shaderRead } ]

def isSplitEligible (ol nl : LayoutKind) (bp ap : Nat) : Bool :=
  ol == nl && decide (bp + 3 <= ap)

def lowerEdge : GpuBackend → HazardEdge → NativeBarrier
  | .d3d12, e =>
      .d3d12 e.beforePass e.afterPass e.resourceId
        (d3d12SyncBits e.beforeSync) (d3d12SyncBits e.afterSync)
        e.beforeLayout e.afterLayout
  | .vulkan14, e =>
      if isSplitEligible e.beforeLayout e.afterLayout e.beforePass e.afterPass then
        .vulkan14SplitEvent e.beforePass e.afterPass e.resourceId
          (vkStageMask e.beforeSync) (vkStageMask e.afterSync)
          e.beforeLayout e.afterLayout
      else
        .vulkan14ImageBarrier2 e.beforePass e.afterPass e.resourceId
          (vkStageMask e.beforeSync) (vkStageMask e.afterSync)
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
  | .d3d12 bp ap rid sb sa lb la =>
      bp == e.beforePass && ap == e.afterPass && rid == e.resourceId &&
      lb == e.beforeLayout && la == e.afterLayout &&
      (!e.kind.isDataHazard ||
        (sb == d3d12SyncBits e.beforeSync && sa == d3d12SyncBits e.afterSync &&
         sb != 0 && sa != 0))
  | .vulkan14ImageBarrier2 bp ap rid ss ds ol nl =>
      bp == e.beforePass && ap == e.afterPass && rid == e.resourceId &&
      ol == e.beforeLayout && nl == e.afterLayout &&
      (!e.kind.isDataHazard ||
        (ss == vkStageMask e.beforeSync && ds == vkStageMask e.afterSync &&
         ss != 0 && ds != 0))
  | .vulkan14SplitEvent sp wp rid ss ds ol nl =>
      sp == e.beforePass && wp == e.afterPass && rid == e.resourceId &&
      ol == e.beforeLayout && nl == e.afterLayout &&
      isSplitEligible ol nl sp wp &&
      (!e.kind.isDataHazard ||
        (ss == vkStageMask e.beforeSync && ds == vkStageMask e.afterSync &&
         ss != 0 && ds != 0))
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

theorem mtl4StageMask_ne_zero (s : SyncScope) : (mtl4StageMask s != 0) = true := by
  cases s <;> decide

theorem layoutKind_beq_self (l : LayoutKind) : (l == l) = true := by
  cases l <;> rfl

/-- §38.10 canonical decidable theorem across all modeled render-graph configurations. -/
theorem tri_backend_barriers_preserve_hazards (cfg : RenderFeatureFlags) :
    let dag := compileIgnisRenderGraph cfg
    HappensBeforeSound dag (lowerBarriers .d3d12 dag) ∧
    HappensBeforeSound dag (lowerBarriers .vulkan14 dag) ∧
    HappensBeforeSound dag (lowerBarriers .metal4 dag) := by
  cases cfg <;> decide

/-- General single-edge hazard preservation across DirectX 12, Vulkan 1.4, and Metal 4. -/
theorem lowerEdge_preserves_edge (backend : GpuBackend) (e : HazardEdge) :
    edgePreservedBy e (lowerEdge backend e) = true := by
  cases backend with
  | d3d12 =>
      cases hk : e.kind.isDataHazard <;>
        simp [lowerEdge, edgePreservedBy, hk, layoutKind_beq_self, d3d12SyncBits_ne_zero]
  | vulkan14 =>
      cases hsplit : isSplitEligible e.beforeLayout e.afterLayout e.beforePass e.afterPass with
      | false =>
          cases hk : e.kind.isDataHazard <;>
            simp [lowerEdge, hsplit, edgePreservedBy, hk, layoutKind_beq_self, vkStageMask_ne_zero]
      | true =>
          cases hk : e.kind.isDataHazard <;>
            simp [lowerEdge, hsplit, edgePreservedBy, hk, layoutKind_beq_self, vkStageMask_ne_zero]
  | metal4 =>
      cases hk : e.kind.isDataHazard <;>
        simp [lowerEdge, hk, edgePreservedBy, mtl4StageMask_ne_zero]

/-- Arbitrary DAG hazard preservation theorem for any backend. -/
theorem lowerBarriers_sound (backend : GpuBackend) (dag : List HazardEdge) :
    HappensBeforeSound dag (lowerBarriers backend dag) := by
  simp only [HappensBeforeSound, happensBeforeSoundB, List.all_eq_true]
  intro e he
  simp only [List.any_eq_true, lowerBarriers, List.mem_map]
  exact ⟨lowerEdge backend e, ⟨e, he, rfl⟩, lowerEdge_preserves_edge backend e⟩

/-- Arbitrary DAG tri-backend equivalence corollary. -/
theorem tri_backend_barriers_preserve_any_dag (dag : List HazardEdge) :
    HappensBeforeSound dag (lowerBarriers .d3d12 dag) ∧
    HappensBeforeSound dag (lowerBarriers .vulkan14 dag) ∧
    HappensBeforeSound dag (lowerBarriers .metal4 dag) :=
  ⟨lowerBarriers_sound .d3d12 dag,
   lowerBarriers_sound .vulkan14 dag,
   lowerBarriers_sound .metal4 dag⟩

end Vesper.IgnisTriBackendEquiv
