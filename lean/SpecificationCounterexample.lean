import Std

/- A counterexample to satisfying both the originally supplied, unqualified K-Adt rule (before its approved amendment)
   and its Total Linear Exhaustion theorem. This is a small closed fragment:
   constructing a linear ticket, moving it into an overriden copy field, and
   discarding the resulting copy wrapper. No effects or aliasing are involved. -/
namespace Vesper.SpecificationCounterexample

inductive Mode where
  | copy | affine | linear
  deriving DecidableEq, Repr

def join : Mode → Mode → Mode
  | .linear, _ | _, .linear => .linear
  | .affine, _ | _, .affine => .affine
  | _, _ => .copy

def meet : Mode → Mode → Mode
  | .copy, _ | _, .copy => .copy
  | .affine, _ | _, .affine => .affine
  | _, _ => .linear

inductive Ty where
  | ticket | wrapper | unit

def kind : Ty → Mode
  | .ticket => .linear
  -- copy struct Wrapper { ticket: Ticket @@ copy }
  -- K-Adt: declared floor ⊔ (component kind ⊓ override)
  | .wrapper => join .copy (meet .linear .copy)
  | .unit => .copy

def discardable : Mode → Prop
  | .copy | .affine => True
  | .linear => False

inductive Expr where
  | newTicket
  | wrap (payload : Expr)
  | discard (value : Expr)

inductive Typed : Expr → Ty → Prop where
  | newTicket : Typed .newTicket .ticket
  | wrap : Typed e .ticket → Typed (.wrap e) .wrapper
  | discard : Typed e t → discardable (kind t) → Typed (.discard e) .unit

-- The wrap transition transfers a ticket into a field; it does not unpack it.
-- Discarding a copy wrapper is weakening, not explicit linear destructuring.
def constructedTickets : Expr → Nat
  | .newTicket => 1
  | .wrap e | .discard e => constructedTickets e

def unpackedTickets : Expr → Nat
  | .newTicket => 0
  | .wrap e | .discard e => unpackedTickets e

def program : Expr := .discard (.wrap .newTicket)

theorem wrapper_is_copy : kind .wrapper = .copy := rfl

theorem program_is_closed_and_unit : Typed program .unit :=
  Typed.discard (Typed.wrap Typed.newTicket) (by trivial)

theorem a_linear_obligation_is_lost :
    constructedTickets program = 1 ∧ unpackedTickets program = 0 := by
  constructor <;> rfl

theorem unrestricted_field_rule_does_not_imply_exhaustion :
    ¬ (∀ e, Typed e .unit → constructedTickets e = unpackedTickets e) := by
  intro h
  have contradiction := h program program_is_closed_and_unit
  change 1 = 0 at contradiction
  omega

/- The compiler's E0105 rejects this wrapper declaration. That extra premise
   repairs this counterexample, but it is a change to the supplied K-Adt rule.
   A compiler cannot implement the rule without restrictions and also prove
   the stated exhaustion guarantee. -/
end Vesper.SpecificationCounterexample
