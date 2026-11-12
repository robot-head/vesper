-- §39.4 document-algebra model for the actual Stylus Doc projection.
-- Tagged comments are conserved for EVERY group-choice schedule. The default
-- renderer below uses a local width chooser; Rust's bounded suffix lookahead
-- is intentionally not claimed to refine this model. This is not a lexer,
-- source semantics, parser, compiler or certified-autofix correctness proof.
-- Explicit noncomputable definitions omit compiler-only unsafe auxiliaries;
-- the strict kernel auditor replays this logical model without executable code.
noncomputable section
namespace Vesper.StylusSafety

-- Binary append avoids Lean's nested-inductive auxiliary recursor, unsupported
-- by the pinned strict replay facility. Public concat normalizes ordered lists
-- to this ordinary algebra; consumers retain Doc.concat : List Doc -> Doc.
inductive Doc where
  | nil
  | text (value : String)
  | comment (value : String)
  | hardLine
  | softLineOrSpace
  | softLineOrEmpty
  | nest (indent : Nat) (inner : Doc)
  | append (left right : Doc)
  | group (inner : Doc)

noncomputable def Doc.concat (children : List Doc) : Doc :=
  children.foldr Doc.append Doc.nil

inductive Mode where
  | flat
  | broken

-- Text and comment payloads stay distinct, including comment-looking text.
-- A line piece records the indentation appended after its newline.
inductive Piece where
  | text (value : String)
  | comment (value : String)
  | line (indent : Nat)

@[simp] noncomputable def commentsOf : List Piece -> List String
  | [] => []
  | .comment value :: rest => value :: commentsOf rest
  | _ :: rest => commentsOf rest

@[simp] theorem commentsOf_append (left right : List Piece) :
    commentsOf (left ++ right) = commentsOf left ++ commentsOf right := by
  induction left with
  | nil => rfl
  | cons head tail ih =>
      cases head with
      | text value => exact ih
      | line indent => exact ih
      | comment value => exact congrArg (List.cons value) ih

@[simp] noncomputable def docComments : Doc -> List String
  | .nil | .text _ | .hardLine | .softLineOrSpace | .softLineOrEmpty => []
  | .comment value => [value]
  | .nest _ inner | .group inner => docComments inner
  | .append left right => docComments left ++ docComments right

@[simp] noncomputable def docCommentsList : List Doc -> List String
  | [] => []
  | head :: tail => docComments head ++ docCommentsList tail

theorem docComments_concat (children : List Doc) :
    docComments (Doc.concat children) = docCommentsList children := by
  induction children with
  | nil => rfl
  | cons head tail ih =>
      exact congrArg (fun xs : List String => docComments head ++ xs) ih

-- Use primitive scalar-list operations: Lean's optimized String.contains,
-- startsWith and splitOn use propext internally in this toolchain. These
-- equivalent model helpers keep the theorem's dependency set empty.
noncomputable def containsScalar (value : String) (wanted : Char) : Bool :=
  value.toList.any (fun ch => ch == wanted)

noncomputable def isLineComment (value : String) : Bool :=
  match value.toList with
  | '/' :: '/' :: _ => true
  | _ => false

-- Mirrors the cached flat-width algebra. Newlines/CR and line comments cannot
-- have finite flat width. Nat abstracts successful checked machine arithmetic.
noncomputable def flatWidth : Doc -> Option Nat
  | .nil | .softLineOrEmpty => some 0
  | .softLineOrSpace => some 1
  | .hardLine => none
  | .text value =>
      if containsScalar value '\n' || containsScalar value '\r' then none else some value.length
  | .comment value =>
      if containsScalar value '\n' || containsScalar value '\r' || isLineComment value
      then none else some value.length
  | .nest _ inner | .group inner => flatWidth inner
  | .append left right => do
      let l <- flatWidth left
      let r <- flatWidth right
      pure (l + r)
termination_by structural doc => doc

-- Group ordinal permits an arbitrary mode schedule even when repeated shared
-- nodes have identical text/width/column but a different continuation. Thus the
-- conservation theorem does not depend on any particular lookahead algorithm.
abbrev GroupChoice := Nat -> Nat -> Nat -> Doc -> Mode

structure Layout where
  pieces : List Piece
  column : Nat
  nextGroup : Nat

noncomputable def advanceColumn (column : Nat) (value : String) : Nat :=
  value.toList.foldl (fun col ch => if ch == '\n' then 0 else col + 1) column

noncomputable def layoutWith (choose : GroupChoice) (width indent column next : Nat)
    (mode : Mode) : Doc -> Layout
  | .nil => ⟨[], column, next⟩
  | .text value => ⟨[.text value], advanceColumn column value, next⟩
  | .comment value => ⟨[.comment value], advanceColumn column value, next⟩
  | .hardLine => ⟨[.line indent], indent, next⟩
  | .softLineOrSpace => match mode with
      | .flat => ⟨[.text " "], column + 1, next⟩
      | .broken => ⟨[.line indent], indent, next⟩
  | .softLineOrEmpty => match mode with
      | .flat => ⟨[], column, next⟩
      | .broken => ⟨[.line indent], indent, next⟩
  | .nest spaces inner => layoutWith choose width (indent + spaces) column next mode inner
  | .append left right =>
      let first := layoutWith choose width indent column next mode left
      let rest := layoutWith choose width indent first.column first.nextGroup mode right
      ⟨first.pieces ++ rest.pieces, rest.column, rest.nextGroup⟩
  | .group inner =>
      layoutWith choose width indent column (next + 1) (choose next width column inner) inner
termination_by structural doc => doc

theorem layoutWith_comments (choose : GroupChoice) (width indent column next : Nat)
    (mode : Mode) (doc : Doc) :
    commentsOf (layoutWith choose width indent column next mode doc).pieces = docComments doc := by
  cases doc with
  | nil => rfl
  | text value => rfl
  | comment value => rfl
  | hardLine => rfl
  | softLineOrSpace => cases mode <;> rfl
  | softLineOrEmpty => cases mode <;> rfl
  | nest spaces inner =>
      exact layoutWith_comments choose width (indent + spaces) column next mode inner
  | append left right =>
      exact (commentsOf_append _ _).trans
        ((congrArg
          (fun xs : List String => xs ++ commentsOf (layoutWith choose width indent
            (layoutWith choose width indent column next mode left).column
            (layoutWith choose width indent column next mode left).nextGroup mode right).pieces)
          (layoutWith_comments choose width indent column next mode left)).trans
          (congrArg (fun ys : List String => docComments left ++ ys)
            (layoutWith_comments choose width indent
              (layoutWith choose width indent column next mode left).column
              (layoutWith choose width indent column next mode left).nextGroup mode right)))
  | group inner =>
      exact layoutWith_comments choose width indent column (next + 1)
        (choose next width column inner) inner
termination_by structural doc

noncomputable def renderWith (choose : GroupChoice) (width col : Nat) (mode : Mode) (doc : Doc) : List Piece :=
  (layoutWith choose width 0 col 0 mode doc).pieces

theorem comments_renderWith (choose : GroupChoice) (doc : Doc)
    (width col : Nat) (mode : Mode) :
    commentsOf (renderWith choose width col mode doc) = docComments doc :=
  layoutWith_comments choose width 0 col 0 mode doc

-- Even two different schedules or inherited modes preserve the same payloads.
theorem comments_renderWith_invariant (left right : GroupChoice) (doc : Doc)
    (width col : Nat) (leftMode rightMode : Mode) :
    commentsOf (renderWith left width col leftMode doc) =
    commentsOf (renderWith right width col rightMode doc) :=
  (comments_renderWith left doc width col leftMode).trans
    (comments_renderWith right doc width col rightMode).symm

-- Local-width representative of the Doc algebra. The generic theorem above
-- is stronger: comment order/payloads are independent of this group policy.
noncomputable def widthChoice : GroupChoice := fun _ width col doc =>
  match flatWidth doc with
  | none => .broken
  | some needed => if needed <= width - col then .flat else .broken

noncomputable def render (width col : Nat) (mode : Mode) (doc : Doc) : List Piece :=
  renderWith widthChoice width col mode doc

theorem comments_render_invariant (doc : Doc) (width col : Nat) :
    commentsOf (render width col .flat doc) =
    commentsOf (render width col .broken doc) := by
  exact (comments_renderWith widthChoice doc width col .flat).trans
    (comments_renderWith widthChoice doc width col .broken).symm

-- Generic §39.4 subset/disjointness implication. This theorem does not assert
-- that a production autofix satisfies its explicit subset premise.
theorem projection_narrowing_preserves_disjointness
    (leavesNew leavesOld callerOther : Nat -> Bool)
    (h_subset : forall f, leavesNew f = true -> leavesOld f = true)
    (h_disj : forall f, ¬(leavesOld f = true /\ callerOther f = true)) :
    forall f, ¬(leavesNew f = true /\ callerOther f = true) := by
  intro f h
  exact h_disj f ⟨h_subset f h.1, h.2⟩

-- Closed checks expose the layout distinction, not merely a length/count.
example : render 100 0 .flat (.concat [.comment "/* first */", .softLineOrSpace,
    .text "42", .comment "/* last */"]) =
    [.comment "/* first */", .text " ", .text "42", .comment "/* last */"] := rfl
example : render 100 0 .broken (.concat [.comment "/* first */", .softLineOrSpace,
    .text "42", .comment "/* last */"]) =
    [.comment "/* first */", .line 0, .text "42", .comment "/* last */"] := rfl

-- Indentation, Unicode scalar columns, embedded LF and CR are represented.
example : render 0 9 .flat (.nest 4 (.concat [.text "é🦀", .hardLine,
    .comment "/* nested */"])) =
    [.text "é🦀", .line 4, .comment "/* nested */"] := rfl
example : advanceColumn 17 "é\n🦀" = 1 := rfl
example : advanceColumn 17 "é🦀" = 19 := rfl
example : flatWidth (.text "é🦀") = some 2 := rfl
example : flatWidth (.text "a\rb") = none := rfl
example : flatWidth (.comment "// line") = none := rfl

-- Group policy changes concrete whitespace; the theorem preserves exact ordered
-- payloads regardless. A line comment forces the default group to broken mode.
example : render 8 0 .broken (.group (.concat [.text "ab", .softLineOrSpace,
    .comment "/*c*/"])) = [.text "ab", .text " ", .comment "/*c*/"] := rfl
example : render 7 0 .flat (.group (.concat [.text "ab", .softLineOrSpace,
    .comment "/*c*/"])) = [.text "ab", .line 0, .comment "/*c*/"] := rfl
example : render 100 0 .flat (.group (.concat [.comment "// é🦀", .softLineOrSpace,
    .text "42"])) = [.comment "// é🦀", .line 0, .text "42"] := rfl

-- These tags do not assert that text payloads have been lexed. The source bridge
-- separately checks actual source comment tokens against projected comment tags.
example : commentsOf (render 1 0 .flat (.concat [.text "// comment-looking text",
    .comment "/* actual tag */"])) = ["/* actual tag */"] := rfl
example : commentsOf (render 0 0 .broken (.concat [.comment "/* repeated */",
    .comment "/* repeated */"])) = ["/* repeated */", "/* repeated */"] := rfl

theorem payload_order_matters :
    commentsOf (render 0 0 .flat (.concat [.comment "a", .comment "b"])) ≠
    commentsOf (render 0 0 .flat (.concat [.comment "b", .comment "a"])) := by
  decide

-- Without h_subset the disjointness conclusion is false, even though the old
-- leaves are disjoint from the caller's leaves. This is not an autofix witness.
theorem narrowing_requires_subset :
    ¬(forall (leavesNew leavesOld callerOther : Nat -> Bool),
      (forall f, ¬(leavesOld f = true /\ callerOther f = true)) ->
      forall f, ¬(leavesNew f = true /\ callerOther f = true)) := by
  intro claimed
  have old_disjoint : forall (_ : Nat), ¬(false = true /\ true = true) := by
    intro _ h
    exact Bool.false_ne_true h.1
  exact claimed (fun _ => true) (fun _ => false) (fun _ => true)
    old_disjoint 0 ⟨rfl, rfl⟩

#print axioms payload_order_matters
#print axioms narrowing_requires_subset

#print axioms docComments_concat
#print axioms commentsOf_append
#print axioms layoutWith_comments
#print axioms comments_render_invariant
#print axioms comments_renderWith
#print axioms comments_renderWith_invariant
#print axioms projection_narrowing_preserves_disjointness
end Vesper.StylusSafety
end
