-- Appendix B.5: Mechanized brand geometry colinearity and disjointness proofs.
--
-- What is proved, and about which model:
-- * `OnLeftOuterVWall` models the 2:1 V-cone outer boundary ray
--   `y - 34 = 2 * (x - 30)` across the left half `[30, 128]` of the 256x256
--   canonical prism grid. `canonical_prism_outer_vertices_colinear` checks that
--   every outer left vertex of the Root Chevron `(30, 34)`, `(50, 74)` and
--   Center Apex Blade `(113, 200)`, `(128, 230)` lies on this ray.
-- * `prism_three_blades_pairwise_disjoint` proves that for any positive channel
--   gap `gap > 0` fitting inside the cone (`halfC + gap < 98`), the horizontal
--   bands of the Port, Center, and Starboard projection blades are pairwise
--   disjoint.
--
-- These theorems depend only on core Lean 4 (`omega`), with no `sorry` and no
-- extra axioms.

namespace Vesper.BrandGeometry

def OnLeftOuterVWall (x y : Nat) : Prop :=
  y - 34 = 2 * (x - 30) ∧ 30 ≤ x ∧ x ≤ 128

theorem canonical_prism_outer_vertices_colinear :
    OnLeftOuterVWall 30 34 ∧
    OnLeftOuterVWall 50 74 ∧
    OnLeftOuterVWall 113 200 ∧
    OnLeftOuterVWall 128 230 := by
  unfold OnLeftOuterVWall
  omega

theorem prism_three_blades_pairwise_disjoint
    (halfC gap x : Nat)
    (h_gap : gap > 0)
    (h_fit : halfC + gap < 98) :
    let portMax   := 128 - halfC - gap
    let centerMin := 128 - halfC
    let centerMax := 128 + halfC
    let starMin   := 128 + halfC + gap
    ¬(x ≤ portMax ∧ centerMin ≤ x ∧ x ≤ centerMax) ∧
    ¬(centerMin ≤ x ∧ x ≤ centerMax ∧ starMin ≤ x) ∧
    ¬(x ≤ portMax ∧ starMin ≤ x) := by
  dsimp only
  refine ⟨?_, ?_, ?_⟩ <;> omega

end Vesper.BrandGeometry
