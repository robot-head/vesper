import MetatheoryFull
import Std

/-! Finite model syntax. No source compiler or runtime observation is asserted. -/
namespace Vesper.ResourceModel42

abbrev Mode := MetatheoryFull.Mode
abbrev Site := Nat

inductive Floor where
  | copy | affine
  deriving DecidableEq
def Floor.toMode : Floor → Mode
  | .copy => .copy | .affine => .affine

inductive Ty where
  | unit | nat | ticket
  | pair (floor : Floor) (left : Ty) (leftMode : Mode) (right : Ty) (rightMode : Mode)
  deriving DecidableEq
def Ty.erase : Ty → MetatheoryFull.Ty
  | .unit => .unit | .nat => .nat | .ticket => .hdl
  | .pair f a ma b mb => .pair f.toMode a.erase ma b.erase mb
def Ty.kind (t : Ty) : Mode := t.erase.kind
def Ty.nodes : Ty → Nat
  | .pair _ a _ b _ => 1 + a.nodes + b.nodes | _ => 1
def Ty.depth : Ty → Nat
  | .pair _ a _ b _ => 1 + max a.depth b.depth | _ => 1
inductive Ty.WF : Ty → Prop where
  | unit : WF .unit | nat : WF .nat | ticket : WF .ticket
  | pair {f a ma b mb} : WF a → WF b → MetatheoryFull.Mode.Le a.kind ma →
      MetatheoryFull.Mode.Le b.kind mb → WF (.pair f a ma b mb)

structure Limits where
  rawNodes : Nat := 256
  context : Nat := 64
  rawDepth : Nat := 64
  typeNodes : Nat := 255
  typeDepth : Nat := 16
  valueNodes : Nat := 255
  valueDepth : Nat := 16
  initialValueNodes : Nat := 4096
  inputs : Nat := 16
  expressionNodes : Nat := 65536
  runtimeDepth : Nat := 256
  resources : Nat := 64
  events : Nat := 128
  fuel : Nat := 4096
  deriving DecidableEq
instance : Inhabited Limits := ⟨{}⟩
def Limits.hard : Limits := ⟨1024,256,128,1023,32,1023,32,16384,64,1048576,512,256,512,65536⟩
def Limits.valid (l : Limits) : Bool :=
  [decide (l.rawNodes ≤ hard.rawNodes),
   decide (l.context ≤ hard.context),
   decide (l.rawDepth ≤ hard.rawDepth),
   decide (l.typeNodes ≤ hard.typeNodes),
   decide (l.typeDepth ≤ hard.typeDepth),
   decide (l.valueNodes ≤ hard.valueNodes),
   decide (l.valueDepth ≤ hard.valueDepth),
   decide (l.initialValueNodes ≤ hard.initialValueNodes),
   decide (l.inputs ≤ hard.inputs),
   decide (l.expressionNodes ≤ hard.expressionNodes),
   decide (l.runtimeDepth ≤ hard.runtimeDepth),
   decide (l.resources ≤ hard.resources),
   decide (l.events ≤ hard.events),
   decide (l.fuel ≤ hard.fuel)].all id

inductive TypeError where
  | nodes | depth | fieldMode
  deriving DecidableEq
structure TypeInfo where
  private mk ::
  ty : Ty
  nodes : Nat
  depth : Nat
  nodes_eq : nodes = ty.nodes
  depth_eq : depth = ty.depth
  wf : ty.WF

def TypeInfo.makePair (l : Limits) (f : Floor) (a : TypeInfo) (ma : Mode)
    (b : TypeInfo) (mb : Mode) : Except TypeError {i : TypeInfo // i.ty = .pair f a.ty ma b.ty mb} := do
  let n := 1 + a.nodes + b.nodes
  let d := 1 + max a.depth b.depth
  if n > l.typeNodes then throw .nodes
  if d > l.typeDepth then throw .depth
  if ha : MetatheoryFull.Mode.Le a.ty.kind ma then
    if hb : MetatheoryFull.Mode.Le b.ty.kind mb then
      -- Construct the expanded pair type only after both metric checks.
      pure ⟨⟨.pair f a.ty ma b.ty mb,n,d,by simp [n,Ty.nodes,a.nodes_eq,b.nodes_eq],
        by simp [d,Ty.depth,a.depth_eq,b.depth_eq],.pair a.wf b.wf ha hb⟩,rfl⟩
    else throw .fieldMode
  else throw .fieldMode
def TypeInfo.pair (l : Limits) (f : Floor) (a : TypeInfo) (ma : Mode)
    (b : TypeInfo) (mb : Mode) : Except TypeError TypeInfo :=
  (makePair l f a ma b mb).map Subtype.val
theorem TypeInfo.pair_ty {l f a ma b mb i} (h : pair l f a ma b mb = .ok i) :
    i.ty = .pair f a.ty ma b.ty mb := by
  unfold pair at h
  cases hm : makePair l f a ma b mb with
  | error e => simp [hm,Except.map] at h
  | ok value => simp [hm,Except.map] at h; subst i; exact value.property

def TypeInfo.inspect (l : Limits) : Nat → (t : Ty) → Except TypeError {i : TypeInfo // i.ty = t}
  | 0, _ => .error .depth
  | fuel + 1, t => do
    if l.typeNodes = 0 then throw .nodes
    if l.typeDepth = 0 then throw .depth
    match t with
    | .unit => pure ⟨⟨.unit,1,1,rfl,rfl,.unit⟩,rfl⟩
    | .nat => pure ⟨⟨.nat,1,1,rfl,rfl,.nat⟩,rfl⟩
    | .ticket => pure ⟨⟨.ticket,1,1,rfl,rfl,.ticket⟩,rfl⟩
    | .pair f a ma b mb =>
      let ia ← inspect l fuel a
      let ib ← inspect l fuel b
      let result ← makePair l f ia.val ma ib.val mb
      pure ⟨result.val,by simpa [ia.property,ib.property] using result.property⟩
def TypeInfo.ofTy (l : Limits) (t : Ty) : Except TypeError TypeInfo :=
  (inspect l l.typeDepth t).map Subtype.val
theorem TypeInfo.ofTy_ty {l t i} (h : ofTy l t = .ok i) : i.ty = t := by
  unfold ofTy at h
  cases hm : inspect l l.typeDepth t with
  | error e => simp [hm,Except.map] at h
  | ok value => simp [hm,Except.map] at h; subst i; exact value.property

inductive SourceAtom where
  | unit (site : Site)
  | nat (site : Site) (n : Nat)
  | var (site : Site) (index : Nat)
  deriving DecidableEq
def SourceAtom.site : SourceAtom → Site
  | .unit s | .nat s _ | .var s _ => s
inductive SourceExpr where
  | atom (a : SourceAtom)
  | pair (site : Site) (floor : Floor) (a : SourceAtom) (ma : Mode) (b : SourceAtom) (mb : Mode)
  | letE (site : Site) (initializer body : SourceExpr)
  | unpack (site : Site) (a : SourceAtom) (body : SourceExpr)
  | ifz (site : Site) (a : SourceAtom) (zero nonzero : SourceExpr)
  | mint (site : Site)
  | consume (site : Site) (a : SourceAtom)
  deriving DecidableEq
def SourceExpr.site : SourceExpr → Site
  | .atom a => a.site
  | .pair s _ _ _ _ _ | .letE s _ _ | .unpack s _ _ | .ifz s _ _ _
  | .mint s | .consume s _ => s
def SourceAtom.erase : SourceAtom → MetatheoryFull.Expr
  | .unit _ => .unitV | .nat _ n => .natV n | .var _ i => .var i
def SourceExpr.erase : SourceExpr → MetatheoryFull.Expr
  | .atom a => a.erase
  | .pair _ _ a _ b _ => .pair a.erase b.erase
  | .letE _ a b => .letE a.erase b.erase
  | .unpack _ a b => .letPair a.erase b.erase
  | .ifz _ a z n => .ifz a.erase z.erase n.erase
  | .mint _ => .mint
  | .consume _ a => .consume a.erase
structure RawProgram where
  inputTypes : Array Ty
  body : SourceExpr

inductive ValidationError where
  | invalidLimits | inputCount | noncopyInput (index : Nat)
  | inputType (index : Nat) (error : TypeError)
  | rawNodes | rawDepth | context | site (site : Site) | duplicateSite (site : Site)
  | variableBound (site : Site)
  deriving DecidableEq
structure SourceStats where
  nodes : Nat := 0
  depth : Nat := 0
  maxContext : Nat := 0
  mints : Nat := 0
  consumes : Nat := 0
  deriving DecidableEq
structure SourceWalk where
  seen : Array Bool
  stats : SourceStats
def SourceWalk.visit (l : Limits) (s : Site) (d ctx : Nat) (w : SourceWalk) :
    Except ValidationError SourceWalk := do
  if w.stats.nodes ≥ l.rawNodes then throw .rawNodes
  if d > l.rawDepth then throw .rawDepth
  if ctx > l.context then throw .context
  if s ≥ l.rawNodes then throw (.site s)
  match w.seen[s]? with
  | none => throw (.site s)
  | some true => throw (.duplicateSite s)
  | some false => pure ⟨w.seen.set! s true,
      ⟨w.stats.nodes + 1,max d w.stats.depth,max ctx w.stats.maxContext,
        w.stats.mints,w.stats.consumes⟩⟩
def validateAtom (l : Limits) (d ctx : Nat) (a : SourceAtom) (w : SourceWalk) :
    Except ValidationError SourceWalk := do
  let w ← w.visit l a.site d ctx
  match a with
  | .var s i => if i ≥ l.context then throw (.variableBound s) else pure w
  | _ => pure w
def validateExpr (l : Limits) : Nat → Nat → Nat → SourceExpr → SourceWalk →
    Except ValidationError SourceWalk
  | 0, _, _, _, _ => .error .rawDepth
  | fuel + 1, d, ctx, e, w => do
    match e with
    | .atom a => validateAtom l d ctx a w
    | .pair s _ a _ b _ =>
      let w ← w.visit l s d ctx
      let w ← validateAtom l (d+1) ctx a w
      validateAtom l (d+1) ctx b w
    | .letE s a b =>
      let w ← w.visit l s d ctx
      let w ← validateExpr l fuel (d+1) ctx a w
      validateExpr l fuel (d+1) (ctx+1) b w
    | .unpack s a b =>
      let w ← w.visit l s d ctx
      let w ← validateAtom l (d+1) ctx a w
      validateExpr l fuel (d+1) (ctx+2) b w
    | .ifz s a z n =>
      let w ← w.visit l s d ctx
      let w ← validateAtom l (d+1) ctx a w
      let w ← validateExpr l fuel (d+1) ctx z w
      validateExpr l fuel (d+1) ctx n w
    | .mint s =>
      let w ← w.visit l s d ctx
      pure {w with stats := {w.stats with mints := w.stats.mints+1}}
    | .consume s a =>
      let w ← w.visit l s d ctx
      let w ← validateAtom l (d+1) ctx a w
      pure {w with stats := {w.stats with consumes := w.stats.consumes+1}}

def validateTypeList (l : Limits) : (types : List Ty) → Nat →
    Except ValidationError {infos : List TypeInfo // infos.map TypeInfo.ty = types ∧
      ∀ i ∈ infos, i.ty.kind = .copy}
  | [], _ => .ok ⟨[],by simp⟩
  | t :: ts, index => do
    match ht : TypeInfo.ofTy l t with
    | .error error => throw (.inputType index error)
    | .ok info =>
      if hc : info.ty.kind = .copy then
        let tail ← validateTypeList l ts (index+1)
        pure ⟨info :: tail.val,by
          constructor
          · simp [TypeInfo.ofTy_ty ht,tail.property.1]
          · intro i hi
            rcases List.mem_cons.mp hi with rfl | h
            · exact hc
            · exact tail.property.2 i h⟩
      else throw (.noncopyInput index)
structure ValidatedData (raw : RawProgram) where
  inputInfo : Array TypeInfo
  inputInfo_types : inputInfo.toList.map TypeInfo.ty = raw.inputTypes.toList
  inputs_copy : ∀ i ∈ inputInfo.toList, i.ty.kind = .copy
  stats : SourceStats
def validateRaw (l : Limits) (raw : RawProgram) : Except ValidationError (ValidatedData raw) := do
  if !l.valid then throw .invalidLimits
  if raw.inputTypes.size > l.inputs || raw.inputTypes.size > l.context then throw .inputCount
  let infos ← validateTypeList l raw.inputTypes.toList 0
  let walk ← validateExpr l l.rawDepth 1 raw.inputTypes.size raw.body
    ⟨Array.replicate l.rawNodes false,{}⟩
  pure ⟨infos.val.toArray,by simpa using infos.property.1,
    by simpa using infos.property.2,walk.stats⟩
structure ValidatedProgram where
  private mk ::
  raw : RawProgram
  limits : Limits
  data : ValidatedData raw
  validation_ok : validateRaw limits raw = .ok data
def ValidatedProgram.inputInfo (p : ValidatedProgram) : Array TypeInfo := p.data.inputInfo
def ValidatedProgram.stats (p : ValidatedProgram) : SourceStats := p.data.stats
def ValidatedProgram.inputInfo_types (p : ValidatedProgram) :
    p.inputInfo.toList.map TypeInfo.ty = p.raw.inputTypes.toList := p.data.inputInfo_types
def ValidatedProgram.inputs_copy (p : ValidatedProgram) :
    ∀ i ∈ p.inputInfo.toList, i.ty.kind = .copy := p.data.inputs_copy
def validate (raw : RawProgram) (l : Limits := {}) : Except ValidationError ValidatedProgram :=
  match h : validateRaw l raw with
  | .error error => .error error
  | .ok data => .ok ⟨raw,l,data,h⟩

inductive RuntimeValue where
  | unit | nat (n : Nat) | hdl (id : Nat) | pair (left right : RuntimeValue)
  deriving DecidableEq
inductive RuntimeAtom where
  | var (index : Nat) | val (value : RuntimeValue)
  deriving DecidableEq
inductive RuntimeExpr where
  | atom (a : RuntimeAtom)
  | pair (a b : RuntimeAtom)
  | letE (initializer body : RuntimeExpr)
  | unpack (a : RuntimeAtom) (body : RuntimeExpr)
  | ifz (a : RuntimeAtom) (zero nonzero : RuntimeExpr)
  | mint (site : Site)
  | consume (site : Site) (a : RuntimeAtom)
  deriving DecidableEq
def RuntimeValue.erase : RuntimeValue → MetatheoryFull.Expr
  | .unit => .unitV | .nat n => .natV n | .hdl r => .hdlV r
  | .pair a b => .pair a.erase b.erase
def RuntimeAtom.erase : RuntimeAtom → MetatheoryFull.Expr
  | .var i => .var i | .val v => v.erase
def RuntimeExpr.erase : RuntimeExpr → MetatheoryFull.Expr
  | .atom a => a.erase | .pair a b => .pair a.erase b.erase
  | .letE a b => .letE a.erase b.erase | .unpack a b => .letPair a.erase b.erase
  | .ifz a z n => .ifz a.erase z.erase n.erase
  | .mint _ => .mint | .consume _ a => .consume a.erase
def SourceAtom.toRuntime : SourceAtom → RuntimeAtom
  | .unit _ => .val .unit | .nat _ n => .val (.nat n) | .var _ i => .var i
def SourceExpr.toRuntime : SourceExpr → RuntimeExpr
  | .atom a => .atom a.toRuntime | .pair _ _ a _ b _ => .pair a.toRuntime b.toRuntime
  | .letE _ a b => .letE a.toRuntime b.toRuntime
  | .unpack _ a b => .unpack a.toRuntime b.toRuntime
  | .ifz _ a z n => .ifz a.toRuntime z.toRuntime n.toRuntime
  | .mint s => .mint s | .consume s a => .consume s a.toRuntime
def RuntimeAtom.subst (k : Nat) (v : RuntimeValue) : RuntimeAtom → RuntimeAtom
  | .var i => if i = k then .val v else if k < i then .var (i-1) else .var i
  | .val w => .val w
def RuntimeExpr.subst (k : Nat) (v : RuntimeValue) : RuntimeExpr → RuntimeExpr
  | .atom a => .atom (a.subst k v) | .pair a b => .pair (a.subst k v) (b.subst k v)
  | .letE a b => .letE (a.subst k v) (b.subst (k+1) v)
  | .unpack a b => .unpack (a.subst k v) (b.subst (k+2) v)
  | .ifz a z n => .ifz (a.subst k v) (z.subst k v) (n.subst k v)
  | .mint s => .mint s | .consume s a => .consume s (a.subst k v)
def RuntimeExpr.asValue : RuntimeExpr → Option RuntimeValue
  | .atom (.val v) => some v
  | .pair (.val a) (.val b) => some (.pair a b)
  | _ => none
-- Input 0 becomes de Bruijn 0: the last input's pure let is outermost.
def closeInputs (values : Array RuntimeValue) (body : RuntimeExpr) : RuntimeExpr :=
  values.toList.foldl (fun e v => .letE (.atom (.val v)) e) body

structure ValueStats where
  nodes : Nat
  depth : Nat
  deriving DecidableEq
structure ExprStats where
  nodes : Nat
  depth : Nat
  maxValueNodes : Nat
  maxValueDepth : Nat
  value : Option ValueStats
  deriving DecidableEq
inductive CapacityError where
  | expressionNodes | runtimeDepth | valueNodes | valueDepth | initialValues | inputCount
  deriving DecidableEq
def checkValueStats (l : Limits) (s : ValueStats) : Except CapacityError ValueStats := do
  if s.nodes > l.valueNodes then throw .valueNodes
  if s.depth > l.valueDepth then throw .valueDepth
  pure s
def measureValue (l : Limits) : Nat → RuntimeValue → Except CapacityError ValueStats
  | 0, _ => .error .valueDepth
  | fuel+1, .pair a b => do
    let a ← measureValue l fuel a
    let b ← measureValue l fuel b
    checkValueStats l ⟨1+a.nodes+b.nodes,1+max a.depth b.depth⟩
  | _+1, _ => checkValueStats l ⟨1,1⟩
def RuntimeValue.measure (l : Limits) (v : RuntimeValue) : Except CapacityError ValueStats :=
  measureValue l l.valueDepth v
def checkExprStats (l : Limits) (s : ExprStats) : Except CapacityError ExprStats := do
  if s.nodes > l.expressionNodes then throw .expressionNodes
  if s.depth > l.runtimeDepth then throw .runtimeDepth
  if s.maxValueNodes > l.valueNodes then throw .valueNodes
  if s.maxValueDepth > l.valueDepth then throw .valueDepth
  pure s
def valueExprStats (s : ValueStats) : ExprStats := ⟨s.nodes,s.depth,s.nodes,s.depth,some s⟩
def variableExprStats : ExprStats := ⟨1,1,0,0,none⟩
def pairStats (l : Limits) (a b : ExprStats) : Except CapacityError ExprStats := do
  let value := match a.value,b.value with
    | some a,some b => some (ValueStats.mk (1+a.nodes+b.nodes) (1+max a.depth b.depth))
    | _,_ => none
  checkExprStats l ⟨1+a.nodes+b.nodes,1+max a.depth b.depth,
    max (max a.maxValueNodes b.maxValueNodes) (value.map ValueStats.nodes |>.getD 0),
    max (max a.maxValueDepth b.maxValueDepth) (value.map ValueStats.depth |>.getD 0),value⟩
def binaryStats (l : Limits) (a b : ExprStats) : Except CapacityError ExprStats :=
  checkExprStats l ⟨1+a.nodes+b.nodes,1+max a.depth b.depth,
    max a.maxValueNodes b.maxValueNodes,max a.maxValueDepth b.maxValueDepth,none⟩
def branchStats (l : Limits) (a z n : ExprStats) : Except CapacityError ExprStats :=
  checkExprStats l ⟨1+a.nodes+z.nodes+n.nodes,1+max a.depth (max z.depth n.depth),
    max a.maxValueNodes (max z.maxValueNodes n.maxValueNodes),
    max a.maxValueDepth (max z.maxValueDepth n.maxValueDepth),none⟩

abbrev SubstitutionPlan := List (Nat × ValueStats)
def SubstitutionPlan.lift (plan : SubstitutionPlan) (n : Nat) : SubstitutionPlan :=
  plan.map (fun (k,v) => (k+n,v))
def plannedVariable : SubstitutionPlan → Nat → ExprStats
  | [], _ => variableExprStats
  | (k,v)::rest, i => if i = k then valueExprStats v else
      plannedVariable rest (if k < i then i-1 else i)
def measureAtom (l : Limits) (plan : SubstitutionPlan) : RuntimeAtom → Except CapacityError ExprStats
  | .var i => checkExprStats l (plannedVariable plan i)
  | .val v => do let s ← v.measure l; checkExprStats l (valueExprStats s)
def measureExpr (l : Limits) : Nat → SubstitutionPlan → RuntimeExpr → Except CapacityError ExprStats
  | 0, _, _ => .error .runtimeDepth
  | fuel+1, plan, e => do
    match e with
    | .atom a => measureAtom l plan a
    | .pair a b => pairStats l (← measureAtom l plan a) (← measureAtom l plan b)
    | .letE a b => do
      let a ← measureExpr l fuel plan a
      let b ← measureExpr l fuel (plan.lift 1) b
      binaryStats l a b
    | .unpack a b => do
      let a ← measureAtom l plan a
      let b ← measureExpr l fuel (plan.lift 2) b
      binaryStats l a b
    | .ifz a z n => do
      let a ← measureAtom l plan a
      let z ← measureExpr l fuel plan z
      let n ← measureExpr l fuel plan n
      branchStats l a z n
    | .mint _ => checkExprStats l ⟨1,1,0,0,none⟩
    | .consume _ a => do
      let a ← measureAtom l plan a
      checkExprStats l ⟨1+a.nodes,1+a.depth,a.maxValueNodes,a.maxValueDepth,none⟩
def RuntimeExpr.measure (l : Limits) (e : RuntimeExpr) : Except CapacityError ExprStats :=
  measureExpr l l.runtimeDepth [] e
def RuntimeExpr.preflightSubst (l : Limits) (k : Nat) (v : RuntimeValue) (e : RuntimeExpr) :
    Except CapacityError ExprStats := do
  let s ← v.measure l
  measureExpr l l.runtimeDepth [(k,s)] e
-- Check BOTH temporary and final terms before either substitution is copied.
def RuntimeExpr.preflightUnpack (l : Limits) (left right : RuntimeValue) (body : RuntimeExpr) :
    Except CapacityError (ExprStats × ExprStats) := do
  let sr ← right.measure l
  let sl ← left.measure l
  let intermediate ← measureExpr l l.runtimeDepth [(0,sr)] body
  let result ← measureExpr l l.runtimeDepth [(0,sr),(0,sl)] body
  pure (intermediate,result)

inductive RuntimeContext where
  | hole | letC (inner : RuntimeContext) (body : RuntimeExpr)
  deriving DecidableEq
def RuntimeContext.plug : RuntimeContext → RuntimeExpr → RuntimeExpr
  | .hole,e => e | .letC inner body,e => .letE (inner.plug e) body
def RuntimeContext.erase : RuntimeContext → MetatheoryFull.EC
  | .hole => .hole | .letC inner body => .letC inner.erase body.erase
def measurePlug (l : Limits) : Nat → RuntimeContext → ExprStats → Except CapacityError ExprStats
  | 0, _, _ => .error .runtimeDepth
  | _+1, .hole, s => checkExprStats l s
  | fuel+1, .letC inner body, s => do
    binaryStats l (← measurePlug l fuel inner s) (← body.measure l)
def RuntimeContext.preflightPlug (l : Limits) (context : RuntimeContext) (hole : ExprStats) :
    Except CapacityError ExprStats := measurePlug l l.runtimeDepth context hole
def preflightInputs (l : Limits) (values : Array RuntimeValue) (body : RuntimeExpr) :
    Except CapacityError ExprStats := do
  if values.size > l.inputs then throw .inputCount
  let mut result ← body.measure l
  let mut total := 0
  for value in values do
    let stats ← value.measure l
    total := total + stats.nodes
    if total > l.initialValueNodes then throw .initialValues
    result ← binaryStats l (valueExprStats stats) result
  pure result

end Vesper.ResourceModel42
