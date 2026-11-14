import ResourceModel42.Syntax

/-! Independent logical representation metrics. These specifications do not
inspect a measurement result or define an exceeded bound by an error tag. -/
namespace Vesper.ResourceModel42.MetricsProofs

def valueMetrics : RuntimeValue → ValueStats
  | .unit | .nat _ | .hdl _ => ⟨1,1⟩
  | .pair a b =>
    let a := valueMetrics a
    let b := valueMetrics b
    ⟨1+a.nodes+b.nodes,1+max a.depth b.depth⟩
def valueExpr (v : ValueStats) : ExprStats := ⟨v.nodes,v.depth,v.nodes,v.depth,some v⟩
def atomMetrics : RuntimeAtom → ExprStats
  | .var _ => ⟨1,1,0,0,none⟩
  | .val v => valueExpr (valueMetrics v)
def pairMetrics (a b : ExprStats) : ExprStats :=
  let closed := match a.value,b.value with
    | some a,some b => some (ValueStats.mk (1+a.nodes+b.nodes) (1+max a.depth b.depth))
    | _,_ => none
  ⟨1+a.nodes+b.nodes,1+max a.depth b.depth,
    max (max a.maxValueNodes b.maxValueNodes) (closed.map ValueStats.nodes |>.getD 0),
    max (max a.maxValueDepth b.maxValueDepth) (closed.map ValueStats.depth |>.getD 0),closed⟩
def binaryMetrics (a b : ExprStats) : ExprStats :=
  ⟨1+a.nodes+b.nodes,1+max a.depth b.depth,max a.maxValueNodes b.maxValueNodes,
    max a.maxValueDepth b.maxValueDepth,none⟩
def branchMetrics (a z n : ExprStats) : ExprStats :=
  ⟨1+a.nodes+z.nodes+n.nodes,1+max a.depth (max z.depth n.depth),
    max a.maxValueNodes (max z.maxValueNodes n.maxValueNodes),
    max a.maxValueDepth (max z.maxValueDepth n.maxValueDepth),none⟩
def exprMetrics : RuntimeExpr → ExprStats
  | .atom a => atomMetrics a
  | .pair a b => pairMetrics (atomMetrics a) (atomMetrics b)
  | .letE a b => binaryMetrics (exprMetrics a) (exprMetrics b)
  | .unpack a b => binaryMetrics (atomMetrics a) (exprMetrics b)
  | .ifz a z n => branchMetrics (atomMetrics a) (exprMetrics z) (exprMetrics n)
  | .mint _ => ⟨1,1,0,0,none⟩
  | .consume _ a =>
    let a := atomMetrics a
    ⟨1+a.nodes,1+a.depth,a.maxValueNodes,a.maxValueDepth,none⟩

def ValueWithin (l : Limits) (s : ValueStats) : Prop :=
  s.nodes ≤ l.valueNodes ∧ s.depth ≤ l.valueDepth
def Within (l : Limits) (s : ExprStats) : Prop :=
  s.nodes ≤ l.expressionNodes ∧ s.depth ≤ l.runtimeDepth ∧
  s.maxValueNodes ≤ l.valueNodes ∧ s.maxValueDepth ≤ l.valueDepth
def ValueOverAt (l : Limits) (fuel : Nat) (s : ValueStats) : CapacityError → Prop
  | .valueNodes => l.valueNodes < s.nodes
  | .valueDepth => min fuel l.valueDepth < s.depth
  | _ => False
def ExprOverAt (l : Limits) (fuel : Nat) (s : ExprStats) : CapacityError → Prop
  | .expressionNodes => l.expressionNodes < s.nodes
  | .runtimeDepth => min fuel l.runtimeDepth < s.depth
  | .valueNodes => l.valueNodes < s.maxValueNodes
  | .valueDepth => l.valueDepth < s.maxValueDepth
  | _ => False
def ValueExceeds (l : Limits) (v : RuntimeValue) (reason : CapacityError) : Prop :=
  ValueOverAt l l.valueDepth (valueMetrics v) reason
def ExprExceeds (l : Limits) (e : RuntimeExpr) (reason : CapacityError) : Prop :=
  ExprOverAt l l.runtimeDepth (exprMetrics e) reason

theorem value_depth_positive (v : RuntimeValue) : 0 < (valueMetrics v).depth := by
  cases v <;> simp [valueMetrics] <;> omega
theorem atom_depth_positive (a : RuntimeAtom) : 0 < (atomMetrics a).depth := by
  cases a with
  | var _ => simp [atomMetrics]
  | val v => exact value_depth_positive v
theorem expr_depth_positive (e : RuntimeExpr) : 0 < (exprMetrics e).depth := by
  cases e <;> simp [exprMetrics,pairMetrics,binaryMetrics,branchMetrics]
  · exact atom_depth_positive _
  all_goals omega

theorem checkValueStats_ok {l s result} (h : checkValueStats l s = .ok result) :
    result = s ∧ ValueWithin l s := by
  by_cases hn : l.valueNodes < s.nodes <;> by_cases hd : l.valueDepth < s.depth <;>
    simp_all [checkValueStats, ValueWithin, bind, pure, Except.pure, Except.bind]

theorem checkValueStats_error {l s reason} (h : checkValueStats l s = .error reason)
    (fuel : Nat) : ValueOverAt l fuel s reason := by
  cases reason <;>
    by_cases hn : l.valueNodes < s.nodes <;> by_cases hd : l.valueDepth < s.depth <;>
    simp_all [checkValueStats, ValueOverAt, bind, pure, Except.pure, Except.bind] <;> (try simp_all [ValueOverAt,ExprOverAt]) <;> omega

theorem checkExprStats_ok {l s result} (h : checkExprStats l s = .ok result) :
    result = s ∧ Within l s := by
  by_cases hn : l.expressionNodes < s.nodes <;> by_cases hd : l.runtimeDepth < s.depth <;>
    by_cases hvn : l.valueNodes < s.maxValueNodes <;> by_cases hvd : l.valueDepth < s.maxValueDepth <;>
    simp_all [checkExprStats, Within, bind, pure, Except.pure, Except.bind]

theorem checkExprStats_error {l s reason} (h : checkExprStats l s = .error reason)
    (fuel : Nat) : ExprOverAt l fuel s reason := by
  cases reason <;>
    by_cases hn : l.expressionNodes < s.nodes <;> by_cases hd : l.runtimeDepth < s.depth <;>
    by_cases hvn : l.valueNodes < s.maxValueNodes <;> by_cases hvd : l.valueDepth < s.maxValueDepth <;>
    simp_all [checkExprStats, ExprOverAt, bind, pure, Except.pure, Except.bind] <;> (try simp_all [ValueOverAt,ExprOverAt]) <;> omega

theorem value_pair_left_over {l fuel a b reason}
    (h : ValueOverAt l fuel (valueMetrics a) reason) :
    ValueOverAt l (fuel+1) (valueMetrics (.pair a b)) reason := by
  cases reason <;> simp_all [ValueOverAt,valueMetrics] <;> omega
theorem value_pair_right_over {l fuel a b reason}
    (h : ValueOverAt l fuel (valueMetrics b) reason) :
    ValueOverAt l (fuel+1) (valueMetrics (.pair a b)) reason := by
  cases reason <;> simp_all [ValueOverAt,valueMetrics] <;> omega

def ValueResult (l : Limits) (fuel : Nat) (v : RuntimeValue) : Except CapacityError ValueStats → Prop
  | .ok s => s = valueMetrics v ∧ ValueWithin l s ∧ s.depth ≤ fuel
  | .error reason => ValueOverAt l fuel (valueMetrics v) reason

theorem measureValue_spec (l : Limits) (fuel : Nat) (v : RuntimeValue) :
    ValueResult l fuel v (measureValue l fuel v) := by
  induction fuel generalizing v with
  | zero =>
    simp only [measureValue,ValueResult,ValueOverAt,Nat.zero_min]
    exact value_depth_positive v
  | succ fuel ih =>
    cases v with
    | unit | nat _ | hdl _ =>
      simp only [measureValue]
      cases hc : checkValueStats l ⟨1,1⟩ with
      | error reason => exact checkValueStats_error hc _
      | ok result =>
        obtain ⟨rfl,within⟩ := checkValueStats_ok hc
        exact ⟨rfl,within,by change 1 ≤ fuel+1; omega⟩
    | pair a b =>
      have ha := ih a
      have hb := ih b
      cases hma : measureValue l fuel a with
      | error reason =>
        simp only [hma,ValueResult] at ha
        simpa [measureValue,hma,Except.bind,ValueResult] using
          (value_pair_left_over (b := b) ha)
      | ok sa =>
        simp only [hma,ValueResult] at ha
        obtain ⟨rfl,hwa,hda⟩ := ha
        cases hmb : measureValue l fuel b with
        | error reason =>
          simp only [hmb,ValueResult] at hb
          simpa [measureValue,hma,hmb,Except.bind,ValueResult] using
            (value_pair_right_over (a := a) hb)
        | ok sb =>
          simp only [hmb,ValueResult] at hb
          obtain ⟨rfl,hwb,hdb⟩ := hb
          simp only [measureValue,hma,hmb,bind,Except.bind]
          cases hc : checkValueStats l
              ⟨1+(valueMetrics a).nodes+(valueMetrics b).nodes,
                1+max (valueMetrics a).depth (valueMetrics b).depth⟩ with
          | error reason => simpa [ValueResult,valueMetrics] using (checkValueStats_error hc (fuel+1))
          | ok result =>
            obtain ⟨rfl,within⟩ := checkValueStats_ok hc
            exact ⟨rfl,within,by simp only; omega⟩

theorem measureValue_ok {l v result} (h : RuntimeValue.measure l v = .ok result) :
    result = valueMetrics v ∧ ValueWithin l result := by
  have spec := measureValue_spec l l.valueDepth v
  rw [RuntimeValue.measure] at h
  rw [h] at spec
  exact ⟨spec.1,spec.2.1⟩
theorem measureValue_error {l v reason} (h : RuntimeValue.measure l v = .error reason) :
    ValueExceeds l v reason := by
  have spec := measureValue_spec l l.valueDepth v
  rw [RuntimeValue.measure] at h
  rw [h] at spec
  exact spec

abbrev ValuePlan := List (Nat × RuntimeValue)
def metricsPlan (plan : ValuePlan) : SubstitutionPlan :=
  plan.map (fun (k,v) => (k,valueMetrics v))
def liftPlan (plan : ValuePlan) (n : Nat) : ValuePlan :=
  plan.map (fun (k,v) => (k+n,v))
def applyAtomPlan : ValuePlan → RuntimeAtom → RuntimeAtom
  | [],a => a
  | (k,v)::rest,a => applyAtomPlan rest (a.subst k v)
def applyExprPlan : ValuePlan → RuntimeExpr → RuntimeExpr
  | [],e => e
  | (k,v)::rest,e => applyExprPlan rest (e.subst k v)

theorem metricsPlan_lift (plan : ValuePlan) (n : Nat) :
    metricsPlan (liftPlan plan n) = (metricsPlan plan).lift n := by
  induction plan with
  | nil => rfl
  | cons p rest ih => cases p; simp [metricsPlan,liftPlan,SubstitutionPlan.lift]
theorem applyAtomPlan_val (plan : ValuePlan) (v : RuntimeValue) :
    applyAtomPlan plan (.val v) = .val v := by
  induction plan with
  | nil => rfl
  | cons p rest ih => cases p; simpa [applyAtomPlan,RuntimeAtom.subst] using ih
theorem applyExprPlan_atom (plan : ValuePlan) (a : RuntimeAtom) :
    applyExprPlan plan (.atom a) = .atom (applyAtomPlan plan a) := by
  induction plan generalizing a with
  | nil => rfl
  | cons p rest ih => cases p; simp [applyExprPlan,RuntimeExpr.subst,applyAtomPlan,ih]
theorem applyExprPlan_pair (plan : ValuePlan) (a b : RuntimeAtom) :
    applyExprPlan plan (.pair a b) = .pair (applyAtomPlan plan a) (applyAtomPlan plan b) := by
  induction plan generalizing a b with
  | nil => rfl
  | cons p rest ih => cases p; simp [applyExprPlan,RuntimeExpr.subst,applyAtomPlan,ih]
theorem applyExprPlan_letE (plan : ValuePlan) (a b : RuntimeExpr) :
    applyExprPlan plan (.letE a b) =
      .letE (applyExprPlan plan a) (applyExprPlan (liftPlan plan 1) b) := by
  induction plan generalizing a b with
  | nil => rfl
  | cons p rest ih => cases p; simp [applyExprPlan,RuntimeExpr.subst,liftPlan,ih]
theorem applyExprPlan_unpack (plan : ValuePlan) (a : RuntimeAtom) (b : RuntimeExpr) :
    applyExprPlan plan (.unpack a b) =
      .unpack (applyAtomPlan plan a) (applyExprPlan (liftPlan plan 2) b) := by
  induction plan generalizing a b with
  | nil => rfl
  | cons p rest ih => cases p; simp [applyExprPlan,RuntimeExpr.subst,applyAtomPlan,liftPlan,ih]
theorem applyExprPlan_ifz (plan : ValuePlan) (a : RuntimeAtom) (z n : RuntimeExpr) :
    applyExprPlan plan (.ifz a z n) =
      .ifz (applyAtomPlan plan a) (applyExprPlan plan z) (applyExprPlan plan n) := by
  induction plan generalizing a z n with
  | nil => rfl
  | cons p rest ih => cases p; simp [applyExprPlan,RuntimeExpr.subst,applyAtomPlan,ih]
theorem applyExprPlan_mint (plan : ValuePlan) (site : Site) :
    applyExprPlan plan (.mint site) = .mint site := by
  induction plan with
  | nil => rfl
  | cons p rest ih => cases p; simpa [applyExprPlan,RuntimeExpr.subst] using ih
theorem applyExprPlan_consume (plan : ValuePlan) (site : Site) (a : RuntimeAtom) :
    applyExprPlan plan (.consume site a) = .consume site (applyAtomPlan plan a) := by
  induction plan generalizing a with
  | nil => rfl
  | cons p rest ih => cases p; simp [applyExprPlan,RuntimeExpr.subst,applyAtomPlan,ih]

theorem plannedVariable_metrics (plan : ValuePlan) (i : Nat) :
    plannedVariable (metricsPlan plan) i = atomMetrics (applyAtomPlan plan (.var i)) := by
  induction plan generalizing i with
  | nil => rfl
  | cons p rest ih =>
    obtain ⟨k,v⟩ := p
    by_cases same : i = k
    · subst i
      simp [metricsPlan,plannedVariable,applyAtomPlan,RuntimeAtom.subst,
        applyAtomPlan_val,atomMetrics,valueExpr,valueExprStats]
    · by_cases below : k < i <;>
        simpa [metricsPlan,plannedVariable,applyAtomPlan,RuntimeAtom.subst,same,below] using
          (ih (if k < i then i-1 else i))

theorem pair_values_metrics (a b : ValueStats) :
    pairMetrics (valueExpr a) (valueExpr b) =
      valueExpr ⟨1+a.nodes+b.nodes,1+max a.depth b.depth⟩ := by
  cases a; cases b
  simp [pairMetrics,valueExpr]
  congr 1 <;> omega

theorem asValue_metrics {e : RuntimeExpr} {v : RuntimeValue}
    (h : e.asValue = some v) : exprMetrics e = valueExpr (valueMetrics v) := by
  cases e with
  | atom a => cases a <;> simp [RuntimeExpr.asValue] at h; subst v; rfl
  | pair a b =>
    cases a <;> cases b <;> simp [RuntimeExpr.asValue] at h
    subst v
    exact pair_values_metrics _ _
  | letE _ _ | unpack _ _ | ifz _ _ _ | mint _ | consume _ _ =>
    simp [RuntimeExpr.asValue] at h

theorem value_over_atom {l v reason} (h : ValueExceeds l v reason) (fuel : Nat) :
    ExprOverAt l fuel (atomMetrics (.val v)) reason := by
  cases reason <;> simp_all [ValueExceeds,ValueOverAt,ExprOverAt,atomMetrics,valueExpr]

def AtomResult (l : Limits) (fuel : Nat) (plan : ValuePlan) (a : RuntimeAtom) :
    Except CapacityError ExprStats → Prop
  | .ok s => s = atomMetrics (applyAtomPlan plan a) ∧ Within l s
  | .error reason => ExprOverAt l fuel (atomMetrics (applyAtomPlan plan a)) reason

theorem measureAtom_spec (l : Limits) (fuel : Nat) (plan : ValuePlan) (a : RuntimeAtom) :
    AtomResult l fuel plan a (measureAtom l (metricsPlan plan) a) := by
  cases a with
  | var i =>
    simp only [measureAtom,plannedVariable_metrics]
    cases hc : checkExprStats l (atomMetrics (applyAtomPlan plan (.var i))) with
    | error reason => exact checkExprStats_error hc _
    | ok result =>
      obtain ⟨rfl,within⟩ := checkExprStats_ok hc
      exact ⟨rfl,within⟩
  | val v =>
    simp only [measureAtom]
    cases hv : RuntimeValue.measure l v with
    | error reason =>
      have over := value_over_atom (measureValue_error hv) fuel
      simpa [hv,Except.bind,AtomResult,applyAtomPlan_val] using over
    | ok stats =>
      obtain ⟨rfl,within⟩ := measureValue_ok hv
      simp only [bind,Except.bind]
      cases hc : checkExprStats l (valueExprStats (valueMetrics v)) with
      | error reason =>
        simpa [AtomResult,applyAtomPlan_val,atomMetrics,valueExpr,valueExprStats]
          using (checkExprStats_error hc fuel)
      | ok result =>
        obtain ⟨rfl,within⟩ := checkExprStats_ok hc
        exact ⟨by simp [applyAtomPlan_val,atomMetrics,valueExprStats,valueExpr],within⟩

theorem binary_left_over {l fuel a b reason}
    (h : ExprOverAt l fuel a reason) : ExprOverAt l (fuel+1) (binaryMetrics a b) reason := by
  cases reason <;> simp_all [ExprOverAt,binaryMetrics] <;> omega
theorem binary_right_over {l fuel a b reason}
    (h : ExprOverAt l fuel b reason) : ExprOverAt l (fuel+1) (binaryMetrics a b) reason := by
  cases reason <;> simp_all [ExprOverAt,binaryMetrics] <;> omega
theorem pair_left_over {l fuel a b reason}
    (h : ExprOverAt l fuel a reason) : ExprOverAt l (fuel+1) (pairMetrics a b) reason := by
  cases reason <;> simp_all [ExprOverAt,pairMetrics] <;> omega
theorem pair_right_over {l fuel a b reason}
    (h : ExprOverAt l fuel b reason) : ExprOverAt l (fuel+1) (pairMetrics a b) reason := by
  cases reason <;> simp_all [ExprOverAt,pairMetrics] <;> omega
theorem branch_condition_over {l fuel a z n reason}
    (h : ExprOverAt l fuel a reason) : ExprOverAt l (fuel+1) (branchMetrics a z n) reason := by
  cases reason <;> simp_all [ExprOverAt,branchMetrics] <;> omega
theorem branch_zero_over {l fuel a z n reason}
    (h : ExprOverAt l fuel z reason) : ExprOverAt l (fuel+1) (branchMetrics a z n) reason := by
  cases reason <;> simp_all [ExprOverAt,branchMetrics] <;> omega
theorem branch_nonzero_over {l fuel a z n reason}
    (h : ExprOverAt l fuel n reason) : ExprOverAt l (fuel+1) (branchMetrics a z n) reason := by
  cases reason <;> simp_all [ExprOverAt,branchMetrics] <;> omega

def ExprResult (l : Limits) (fuel : Nat) (plan : ValuePlan) (e : RuntimeExpr) :
    Except CapacityError ExprStats → Prop
  | .ok s => s = exprMetrics (applyExprPlan plan e) ∧ Within l s
  | .error reason => ExprOverAt l fuel (exprMetrics (applyExprPlan plan e)) reason

def StatsResult (l : Limits) (fuel : Nat) (expected : ExprStats) :
    Except CapacityError ExprStats → Prop
  | .ok s => s = expected ∧ Within l s
  | .error reason => ExprOverAt l fuel expected reason

theorem checkStats_spec (l : Limits) (fuel : Nat) (s : ExprStats) :
    StatsResult l fuel s (checkExprStats l s) := by
  cases hc : checkExprStats l s with
  | error reason => exact checkExprStats_error hc fuel
  | ok result =>
    obtain ⟨rfl,within⟩ := checkExprStats_ok hc
    exact ⟨rfl,within⟩

theorem stats_bind {l : Limits} {fa fb : Nat} {a b : ExprStats}
    {ra : Except CapacityError ExprStats} {next : ExprStats → Except CapacityError ExprStats}
    (ha : StatsResult l fa a ra)
    (liftError : ∀ r, ExprOverAt l fa a r → ExprOverAt l fb b r)
    (hb : StatsResult l fb b (next a)) :
    StatsResult l fb b (ra >>= next) := by
  cases ra with
  | error reason => exact liftError reason ha
  | ok s =>
    obtain ⟨rfl,_⟩ := ha
    exact hb

def consumeMetrics (a : ExprStats) : ExprStats :=
  ⟨1+a.nodes,1+a.depth,a.maxValueNodes,a.maxValueDepth,none⟩
theorem consume_over {l fuel a reason}
    (h : ExprOverAt l fuel a reason) : ExprOverAt l (fuel+1) (consumeMetrics a) reason := by
  cases reason <;> simp_all [ExprOverAt,consumeMetrics] <;> omega

theorem measureExpr_spec (l : Limits) (fuel : Nat) (plan : ValuePlan) (e : RuntimeExpr) :
    ExprResult l fuel plan e (measureExpr l fuel (metricsPlan plan) e) := by
  induction fuel generalizing plan e with
  | zero =>
    change min 0 l.runtimeDepth < (exprMetrics (applyExprPlan plan e)).depth
    simpa using expr_depth_positive (applyExprPlan plan e)
  | succ fuel ih =>
    cases e with
    | atom a =>
      simpa [ExprResult,AtomResult,measureExpr,applyExprPlan_atom,exprMetrics]
        using measureAtom_spec l (fuel+1) plan a
    | pair a b =>
      change StatsResult l (fuel+1)
        (exprMetrics (applyExprPlan plan (.pair a b))) _
      rw [applyExprPlan_pair]
      simp only [exprMetrics,measureExpr]
      apply stats_bind (measureAtom_spec l fuel plan a)
        (fun _ h => pair_left_over h)
      apply stats_bind (measureAtom_spec l fuel plan b)
        (fun _ h => pair_right_over h)
      exact checkStats_spec l (fuel+1) _
    | letE a b =>
      change StatsResult l (fuel+1)
        (exprMetrics (applyExprPlan plan (.letE a b))) _
      rw [applyExprPlan_letE]
      simp only [exprMetrics,measureExpr,← metricsPlan_lift]
      apply stats_bind (ih plan a) (fun _ h => binary_left_over h)
      apply stats_bind (ih (liftPlan plan 1) b) (fun _ h => binary_right_over h)
      exact checkStats_spec l (fuel+1) _
    | unpack a b =>
      change StatsResult l (fuel+1)
        (exprMetrics (applyExprPlan plan (.unpack a b))) _
      rw [applyExprPlan_unpack]
      simp only [exprMetrics,measureExpr,← metricsPlan_lift]
      apply stats_bind (measureAtom_spec l fuel plan a) (fun _ h => binary_left_over h)
      apply stats_bind (ih (liftPlan plan 2) b) (fun _ h => binary_right_over h)
      exact checkStats_spec l (fuel+1) _
    | ifz a z n =>
      change StatsResult l (fuel+1)
        (exprMetrics (applyExprPlan plan (.ifz a z n))) _
      rw [applyExprPlan_ifz]
      simp only [exprMetrics,measureExpr]
      apply stats_bind (measureAtom_spec l fuel plan a) (fun _ h => branch_condition_over h)
      apply stats_bind (ih plan z) (fun _ h => branch_zero_over h)
      apply stats_bind (ih plan n) (fun _ h => branch_nonzero_over h)
      exact checkStats_spec l (fuel+1) _
    | mint site =>
      simpa [ExprResult,measureExpr,applyExprPlan_mint,exprMetrics]
        using checkStats_spec l (fuel+1) (exprMetrics (.mint site))
    | consume site a =>
      change StatsResult l (fuel+1)
        (exprMetrics (applyExprPlan plan (.consume site a))) _
      rw [applyExprPlan_consume]
      simp only [exprMetrics,measureExpr]
      apply stats_bind (measureAtom_spec l fuel plan a) (fun _ h => consume_over h)
      exact checkStats_spec l (fuel+1) _

theorem measureExpr_ok {l e result} (h : RuntimeExpr.measure l e = .ok result) :
    result = exprMetrics e ∧ Within l result := by
  have spec := measureExpr_spec l l.runtimeDepth [] e
  simp only [metricsPlan,List.map_nil] at spec
  rw [RuntimeExpr.measure] at h
  rw [h] at spec
  exact spec

theorem measureExpr_error {l e reason} (h : RuntimeExpr.measure l e = .error reason) :
    ExprExceeds l e reason := by
  have spec := measureExpr_spec l l.runtimeDepth [] e
  simp only [metricsPlan,List.map_nil] at spec
  rw [RuntimeExpr.measure] at h
  rw [h] at spec
  exact spec

/-- A substitution can refuse its actual incoming value before inspecting the
body, including when that value will be unused. -/
theorem preflightSubst_ok {l k v e result}
    (h : RuntimeExpr.preflightSubst l k v e = .ok result) :
    result = exprMetrics (e.subst k v) ∧ Within l result := by
  unfold RuntimeExpr.preflightSubst at h
  cases hv : RuntimeValue.measure l v with
  | error reason => simp [hv,bind,Except.bind] at h
  | ok stats =>
    obtain ⟨rfl,_⟩ := measureValue_ok hv
    have spec := measureExpr_spec l l.runtimeDepth [(k,v)] e
    simp only [metricsPlan,List.map_cons,List.map_nil] at spec
    simp only [hv,bind,Except.bind] at h
    rw [h] at spec
    exact spec

theorem preflightSubst_error {l k v e reason}
    (h : RuntimeExpr.preflightSubst l k v e = .error reason) :
    ValueExceeds l v reason ∨ ExprExceeds l (e.subst k v) reason := by
  unfold RuntimeExpr.preflightSubst at h
  cases hv : RuntimeValue.measure l v with
  | error r =>
    simp only [hv,bind,Except.bind,Except.error.injEq] at h
    subst reason
    exact .inl (measureValue_error hv)
  | ok stats =>
    obtain ⟨rfl,_⟩ := measureValue_ok hv
    have spec := measureExpr_spec l l.runtimeDepth [(k,v)] e
    simp only [metricsPlan,List.map_cons,List.map_nil] at spec
    simp only [hv,bind,Except.bind] at h
    rw [h] at spec
    exact .inr spec

theorem preflightUnpack_ok {l left right body intermediate result}
    (h : RuntimeExpr.preflightUnpack l left right body = .ok (intermediate,result)) :
    intermediate = exprMetrics (body.subst 0 right) ∧ Within l intermediate ∧
    result = exprMetrics ((body.subst 0 right).subst 0 left) ∧ Within l result := by
  unfold RuntimeExpr.preflightUnpack at h
  cases hr : RuntimeValue.measure l right with
  | error reason => simp [hr,bind,Except.bind] at h
  | ok sr =>
    obtain ⟨rfl,_⟩ := measureValue_ok hr
    cases hl : RuntimeValue.measure l left with
    | error reason => simp [hr,hl,bind,Except.bind] at h
    | ok sl =>
      obtain ⟨rfl,_⟩ := measureValue_ok hl
      have mid := measureExpr_spec l l.runtimeDepth [(0,right)] body
      have final := measureExpr_spec l l.runtimeDepth [(0,right),(0,left)] body
      simp only [metricsPlan,List.map_cons,List.map_nil] at mid final
      cases hm : measureExpr l l.runtimeDepth [(0,valueMetrics right)] body with
      | error reason => simp [hr,hl,hm,bind,Except.bind] at h
      | ok sm =>
        rw [hm] at mid
        cases hf : measureExpr l l.runtimeDepth [(0,valueMetrics right),(0,valueMetrics left)] body with
        | error reason => simp [hr,hl,hm,hf,bind,Except.bind] at h
        | ok sf =>
          rw [hf] at final
          simp [hr,hl,hm,hf,bind,Except.bind,pure,Except.pure] at h
          obtain ⟨rfl,rfl⟩ := h
          exact ⟨mid.1,mid.2,final.1,final.2⟩

theorem preflightUnpack_error {l left right body reason}
    (h : RuntimeExpr.preflightUnpack l left right body = .error reason) :
    ValueExceeds l right reason ∨ ValueExceeds l left reason ∨
    ExprExceeds l (body.subst 0 right) reason ∨
    ExprExceeds l ((body.subst 0 right).subst 0 left) reason := by
  unfold RuntimeExpr.preflightUnpack at h
  cases hr : RuntimeValue.measure l right with
  | error r =>
    simp only [hr,bind,Except.bind,Except.error.injEq] at h
    subst reason
    exact .inl (measureValue_error hr)
  | ok sr =>
    obtain ⟨rfl,_⟩ := measureValue_ok hr
    cases hl : RuntimeValue.measure l left with
    | error r =>
      simp only [hr,hl,bind,Except.bind,Except.error.injEq] at h
      subst reason
      exact .inr (.inl (measureValue_error hl))
    | ok sl =>
      obtain ⟨rfl,_⟩ := measureValue_ok hl
      have mid := measureExpr_spec l l.runtimeDepth [(0,right)] body
      have final := measureExpr_spec l l.runtimeDepth [(0,right),(0,left)] body
      simp only [metricsPlan,List.map_cons,List.map_nil] at mid final
      cases hm : measureExpr l l.runtimeDepth [(0,valueMetrics right)] body with
      | error r =>
        rw [hm] at mid
        simp only [hr,hl,hm,bind,Except.bind,Except.error.injEq] at h
        subst reason
        exact .inr (.inr (.inl mid))
      | ok sm =>
        cases hf : measureExpr l l.runtimeDepth [(0,valueMetrics right),(0,valueMetrics left)] body with
        | error r =>
          rw [hf] at final
          simp only [hr,hl,hm,hf,bind,Except.bind,Except.error.injEq] at h
          subst reason
          exact .inr (.inr (.inr final))
        | ok sf => simp [hr,hl,hm,hf,bind,Except.bind,pure,Except.pure] at h

theorem configured_over_at {l e reason} (h : ExprExceeds l e reason) (fuel : Nat) :
    ExprOverAt l fuel (exprMetrics e) reason := by
  cases reason <;> simp_all [ExprExceeds,ExprOverAt] <;> omega

theorem measureExpr_at (l : Limits) (e : RuntimeExpr) (fuel : Nat) :
    StatsResult l fuel (exprMetrics e) (RuntimeExpr.measure l e) := by
  cases hm : RuntimeExpr.measure l e with
  | error reason => exact configured_over_at (measureExpr_error hm) fuel
  | ok result => exact measureExpr_ok hm

theorem measurePlug_spec (l : Limits) (fuel : Nat) (context : RuntimeContext) (hole : RuntimeExpr) :
    StatsResult l fuel (exprMetrics (context.plug hole))
      (measurePlug l fuel context (exprMetrics hole)) := by
  induction fuel generalizing context with
  | zero =>
    change min 0 l.runtimeDepth < (exprMetrics (context.plug hole)).depth
    simpa using expr_depth_positive (context.plug hole)
  | succ fuel ih =>
    cases context with
    | hole => exact checkStats_spec l (fuel+1) _
    | letC inner body =>
      simp only [measurePlug,RuntimeContext.plug,exprMetrics]
      apply stats_bind (ih inner) (fun _ h => binary_left_over h)
      apply stats_bind (measureExpr_at l body fuel) (fun _ h => binary_right_over h)
      exact checkStats_spec l (fuel+1) _

theorem preflightPlug_ok {l context hole result}
    (h : RuntimeContext.preflightPlug l context (exprMetrics hole) = .ok result) :
    result = exprMetrics (context.plug hole) ∧ Within l result := by
  have spec := measurePlug_spec l l.runtimeDepth context hole
  rw [RuntimeContext.preflightPlug] at h
  rw [h] at spec
  exact spec

theorem preflightPlug_error {l context hole reason}
    (h : RuntimeContext.preflightPlug l context (exprMetrics hole) = .error reason) :
    ExprExceeds l (context.plug hole) reason := by
  have spec := measurePlug_spec l l.runtimeDepth context hole
  rw [RuntimeContext.preflightPlug] at h
  rw [h] at spec
  exact spec

def closeList (values : List RuntimeValue) (body : RuntimeExpr) : RuntimeExpr :=
  values.foldl (fun e v => .letE (.atom (.val v)) e) body

def initialNodes (values : List RuntimeValue) : Nat :=
  (values.map (fun v => (valueMetrics v).nodes)).sum

def InputsOver (l : Limits) (total : Nat) (values : List RuntimeValue)
    (body : RuntimeExpr) : CapacityError → Prop
  | .initialValues => l.initialValueNodes < total + initialNodes values
  | .inputCount => False
  | reason => ExprExceeds l (closeList values body) reason

def inputStep (l : Limits) (value : RuntimeValue) (state : MProd ExprStats Nat) :
    Except CapacityError (ForInStep (MProd ExprStats Nat)) := do
  let stats ← value.measure l
  let total := state.snd + stats.nodes
  if total > l.initialValueNodes then throw .initialValues
  let result ← binaryStats l (valueExprStats stats) state.fst
  pure (.yield ⟨result,total⟩)

def inputLoop (l : Limits) : List RuntimeValue → MProd ExprStats Nat →
    Except CapacityError (MProd ExprStats Nat)
  | [],state => .ok state
  | value::rest,state => do
    let stats ← value.measure l
    let total := state.snd + stats.nodes
    if total > l.initialValueNodes then throw .initialValues
    let result ← binaryStats l (valueExprStats stats) state.fst
    inputLoop l rest ⟨result,total⟩

theorem input_forIn (l : Limits) (values : List RuntimeValue) (state : MProd ExprStats Nat) :
    forIn values state (inputStep l) = inputLoop l values state := by
  induction values generalizing state with
  | nil => rfl
  | cons value rest ih =>
    simp only [List.forIn_cons,inputStep,inputLoop]
    cases hv : RuntimeValue.measure l value with
    | error reason => simp [bind,Except.bind]
    | ok stats =>
      by_cases over : l.initialValueNodes < state.snd+stats.nodes
      · simp [over,bind,Except.bind]
      · simp only [over,↓reduceIte,bind,Except.bind,pure,Except.pure]
        cases hb : binaryStats l (valueExprStats stats) state.fst with
        | error reason => simp
        | ok result => simpa [hb,bind,Except.bind] using ih (MProd.mk result _)

theorem input_preflight_eq (l : Limits) (values : Array RuntimeValue) (body : RuntimeExpr) :
    preflightInputs l values body = (do
      if values.size > l.inputs then throw .inputCount
      let stats ← body.measure l
      let result ← inputLoop l values.toList ⟨stats,0⟩
      pure result.fst) := by
  unfold preflightInputs
  have same : (fun value state => do
      let stats ← value.measure l
      let total := state.snd + stats.nodes
      if total > l.initialValueNodes then throw .initialValues
      let result ← binaryStats l (valueExprStats stats) state.fst
      pure PUnit.unit
      pure (ForInStep.yield (MProd.mk result total))) = inputStep l := by
    funext value state
    cases hv : RuntimeValue.measure l value with
    | error reason => simp [inputStep,hv,bind,Except.bind]
    | ok stats =>
      by_cases over : l.initialValueNodes < state.snd+stats.nodes <;>
        simp [inputStep,hv,over,bind,Except.bind,pure,Except.pure]
  simp only [same,← Array.forIn_toList,input_forIn]

theorem closeList_cons (v : RuntimeValue) (vs : List RuntimeValue) (body : RuntimeExpr) :
    closeList (v::vs) body = closeList vs (.letE (.atom (.val v)) body) := rfl

theorem initialNodes_cons (v : RuntimeValue) (vs : List RuntimeValue) :
    initialNodes (v::vs) = (valueMetrics v).nodes + initialNodes vs := rfl

theorem wrapper_over {l body reason} (h : ExprExceeds l body reason) (v : RuntimeValue) :
    ExprExceeds l (.letE (.atom (.val v)) body) reason := by
  cases reason <;> simp_all [ExprExceeds,ExprOverAt,exprMetrics,binaryMetrics] <;> omega

theorem closeList_over {l body reason} (h : ExprExceeds l body reason) (vs : List RuntimeValue) :
    ExprExceeds l (closeList vs body) reason := by
  induction vs generalizing body with
  | nil => exact h
  | cons v rest ih => exact ih (wrapper_over h v)

theorem input_value_over {l v reason} (h : ValueExceeds l v reason) (body : RuntimeExpr) :
    ExprExceeds l (.letE (.atom (.val v)) body) reason := by
  have hv := value_over_atom h l.runtimeDepth
  cases reason <;> simp_all [ExprExceeds,ExprOverAt,exprMetrics,binaryMetrics] <;> omega

def InputLoopResult (l : Limits) (values : List RuntimeValue) (body : RuntimeExpr) (total : Nat) :
    Except CapacityError (MProd ExprStats Nat) → Prop
  | .ok state => state.fst = exprMetrics (closeList values body) ∧ Within l state.fst ∧
      state.snd = total + initialNodes values ∧ state.snd ≤ l.initialValueNodes
  | .error reason => InputsOver l total values body reason

theorem actual_over_inputs {l total values body reason}
    (h : ExprExceeds l (closeList values body) reason) : InputsOver l total values body reason := by
  cases reason <;> simp_all [InputsOver,ExprExceeds,ExprOverAt]

theorem inputLoop_spec (l : Limits) (values : List RuntimeValue) (body : RuntimeExpr) (total : Nat)
    (within : Within l (exprMetrics body)) (budget : total ≤ l.initialValueNodes) :
    InputLoopResult l values body total (inputLoop l values ⟨exprMetrics body,total⟩) := by
  induction values generalizing body total with
  | nil => exact ⟨rfl,within,by simp [initialNodes],budget⟩
  | cons v rest ih =>
    cases hv : RuntimeValue.measure l v with
    | error reason =>
      have over := closeList_over (input_value_over (measureValue_error hv) body) rest
      have actual : InputsOver l total (v::rest) body reason :=
        actual_over_inputs (by simpa [closeList_cons] using over)
      simpa [inputLoop,hv,bind,Except.bind,InputLoopResult] using actual
    | ok stats =>
      obtain ⟨rfl,_⟩ := measureValue_ok hv
      by_cases over : l.initialValueNodes < total+(valueMetrics v).nodes
      · simp only [inputLoop,hv,over,↓reduceIte,bind,Except.bind,InputLoopResult,InputsOver]
        simp only [initialNodes_cons]
        omega
      · simp only [inputLoop,hv,over,↓reduceIte,bind,Except.bind,pure,Except.pure]
        cases hb : binaryStats l (valueExprStats (valueMetrics v)) (exprMetrics body) with
        | error reason =>
          have excess := checkExprStats_error hb l.runtimeDepth
          have final := closeList_over (body := .letE (.atom (.val v)) body) excess rest
          have actual : InputsOver l total (v::rest) body reason :=
            actual_over_inputs (by simpa [closeList_cons] using final)
          exact actual
        | ok result =>
          obtain ⟨rfl,bounded⟩ := checkExprStats_ok hb
          have spec := ih (.letE (.atom (.val v)) body) (total+(valueMetrics v).nodes) bounded (by omega)
          change InputLoopResult l (v::rest) body total
            (inputLoop l rest ⟨exprMetrics (.letE (.atom (.val v)) body),total+(valueMetrics v).nodes⟩)
          cases hr : inputLoop l rest
            ⟨exprMetrics (.letE (.atom (.val v)) body),total+(valueMetrics v).nodes⟩ with
          | error reason =>
            rw [hr] at spec
            cases reason <;>
              simp [InputLoopResult,InputsOver,initialNodes_cons,closeList_cons,Nat.add_assoc] at spec ⊢
            all_goals exact spec
          | ok state =>
            rw [hr] at spec
            obtain ⟨exactStats,exactWithin,exactTotal,totalWithin⟩ := spec
            exact ⟨exactStats,exactWithin,by simpa [initialNodes_cons,Nat.add_assoc] using exactTotal,totalWithin⟩

/-- Initial admission errors describe actual requested input count, aggregate
value expansion, or the whole expression with every pure input let included. -/
def InitialExceeds (l : Limits) (values : Array RuntimeValue) (body : RuntimeExpr) :
    CapacityError → Prop
  | .inputCount => l.inputs < values.size
  | .initialValues => l.initialValueNodes < initialNodes values.toList
  | reason => ExprExceeds l (closeInputs values body) reason

def InitialResult (l : Limits) (values : Array RuntimeValue) (body : RuntimeExpr) :
    Except CapacityError ExprStats → Prop
  | .ok result => result = exprMetrics (closeInputs values body) ∧ Within l result ∧
      values.size ≤ l.inputs ∧ initialNodes values.toList ≤ l.initialValueNodes
  | .error reason => InitialExceeds l values body reason

theorem preflightInputs_spec (l : Limits) (values : Array RuntimeValue) (body : RuntimeExpr) :
    InitialResult l values body (preflightInputs l values body) := by
  rw [input_preflight_eq]
  by_cases count : l.inputs < values.size
  · simp [count,bind,Except.bind,InitialResult,InitialExceeds]
  · simp only [count,↓reduceIte,bind,Except.bind,pure,Except.pure]
    cases hb : RuntimeExpr.measure l body with
    | error reason =>
      have actual := closeList_over (measureExpr_error hb) values.toList
      cases reason <;>
        simp [InitialResult,InitialExceeds,closeInputs,closeList,ExprExceeds,ExprOverAt] at actual ⊢
      all_goals exact actual
    | ok stats =>
      obtain ⟨rfl,bounded⟩ := measureExpr_ok hb
      have spec := inputLoop_spec l values.toList body 0 bounded (by omega)
      change InitialResult l values body (do
        let state ← inputLoop l values.toList ⟨exprMetrics body,0⟩
        pure state.fst)
      cases hi : inputLoop l values.toList ⟨exprMetrics body,0⟩ with
      | error reason =>
        rw [hi] at spec
        cases reason <;>
          simp [InitialResult,InitialExceeds,InputLoopResult,InputsOver,closeInputs,closeList] at spec ⊢
        all_goals exact spec
      | ok result =>
        rw [hi] at spec
        obtain ⟨exactStats,exactWithin,exactTotal,totalWithin⟩ := spec
        simp only [bind,Except.bind,InitialResult]
        exact ⟨exactStats,exactWithin,by omega,by omega⟩

theorem preflightInputs_ok {l values body result}
    (h : preflightInputs l values body = .ok result) :
    result = exprMetrics (closeInputs values body) ∧ Within l result ∧
    values.size ≤ l.inputs ∧ initialNodes values.toList ≤ l.initialValueNodes := by
  have spec := preflightInputs_spec l values body
  rw [h] at spec
  exact spec

theorem preflightInputs_error {l values body reason}
    (h : preflightInputs l values body = .error reason) :
    InitialExceeds l values body reason := by
  have spec := preflightInputs_spec l values body
  rw [h] at spec
  exact spec

/-- Independent admissible metrics exclude every corresponding refusal. -/
theorem valueWithin_not_exceeds {l v reason} (h : ValueWithin l (valueMetrics v)) :
    ¬ ValueExceeds l v reason := by
  cases reason <;> simp_all [ValueExceeds,ValueOverAt,ValueWithin] <;> omega

theorem within_not_exceeds {l e reason} (h : Within l (exprMetrics e)) :
    ¬ ExprExceeds l e reason := by
  cases reason <;> simp_all [ExprExceeds,ExprOverAt,Within] <;> omega

theorem measureValue_complete {l v} (within : ValueWithin l (valueMetrics v)) :
    RuntimeValue.measure l v = .ok (valueMetrics v) := by
  cases h : RuntimeValue.measure l v with
  | error reason => exact False.elim (valueWithin_not_exceeds within (measureValue_error h))
  | ok stats => exact congrArg Except.ok (measureValue_ok h).1

theorem measureExpr_complete {l e} (within : Within l (exprMetrics e)) :
    RuntimeExpr.measure l e = .ok (exprMetrics e) := by
  cases h : RuntimeExpr.measure l e with
  | error reason => exact False.elim (within_not_exceeds within (measureExpr_error h))
  | ok stats => exact congrArg Except.ok (measureExpr_ok h).1

theorem preflightSubst_complete {l k v e}
    (incoming : ValueWithin l (valueMetrics v)) (candidate : Within l (exprMetrics (e.subst k v))) :
    RuntimeExpr.preflightSubst l k v e = .ok (exprMetrics (e.subst k v)) := by
  cases h : RuntimeExpr.preflightSubst l k v e with
  | error reason =>
    rcases preflightSubst_error h with hv | he
    · exact False.elim (valueWithin_not_exceeds incoming hv)
    · exact False.elim (within_not_exceeds candidate he)
  | ok stats => exact congrArg Except.ok (preflightSubst_ok h).1

theorem preflightUnpack_complete {l left right body}
    (leftWithin : ValueWithin l (valueMetrics left)) (rightWithin : ValueWithin l (valueMetrics right))
    (intermediate : Within l (exprMetrics (body.subst 0 right)))
    (candidate : Within l (exprMetrics ((body.subst 0 right).subst 0 left))) :
    RuntimeExpr.preflightUnpack l left right body =
      .ok (exprMetrics (body.subst 0 right),exprMetrics ((body.subst 0 right).subst 0 left)) := by
  cases h : RuntimeExpr.preflightUnpack l left right body with
  | error reason =>
    rcases preflightUnpack_error h with hr | hl | hm | hf
    · exact False.elim (valueWithin_not_exceeds rightWithin hr)
    · exact False.elim (valueWithin_not_exceeds leftWithin hl)
    · exact False.elim (within_not_exceeds intermediate hm)
    · exact False.elim (within_not_exceeds candidate hf)
  | ok pair =>
    obtain ⟨a,b⟩ := pair
    obtain ⟨ha,_,hb,_⟩ := preflightUnpack_ok h
    cases ha
    cases hb
    rfl

theorem preflightPlug_complete {l context hole}
    (within : Within l (exprMetrics (context.plug hole))) :
    RuntimeContext.preflightPlug l context (exprMetrics hole) = .ok (exprMetrics (context.plug hole)) := by
  cases h : RuntimeContext.preflightPlug l context (exprMetrics hole) with
  | error reason => exact False.elim (within_not_exceeds within (preflightPlug_error h))
  | ok stats => exact congrArg Except.ok (preflightPlug_ok h).1

theorem preflightInputs_complete {l values body}
    (count : values.size ≤ l.inputs)
    (aggregate : initialNodes values.toList ≤ l.initialValueNodes)
    (within : Within l (exprMetrics (closeInputs values body))) :
    preflightInputs l values body = .ok (exprMetrics (closeInputs values body)) := by
  cases h : preflightInputs l values body with
  | error reason =>
    have actual := preflightInputs_error h
    cases reason with
    | inputCount => simp only [InitialExceeds] at actual; omega
    | initialValues => simp only [InitialExceeds] at actual; omega
    | expressionNodes | runtimeDepth | valueNodes | valueDepth =>
      exact False.elim (within_not_exceeds within actual)
  | ok stats => exact congrArg Except.ok (preflightInputs_ok h).1

end Vesper.ResourceModel42.MetricsProofs
