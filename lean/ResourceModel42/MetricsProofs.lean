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

end Vesper.ResourceModel42.MetricsProofs
