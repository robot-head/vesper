//! The gyroscopic term `−ω × Iω`, integrated implicitly.
//!
//! Explicit integration of `I ω̇ = −ω × Iω` gains energy and flips an
//! intermediate-axis spinner the wrong way at game time steps. This is the
//! implicit backward-Euler step with one Newton iteration (Catto, "Physics
//! for Game Programmers: Numerical Methods", GDC 2015), in the principal
//! frame where `I` is diagonal:
//!
//! `f(ω') = I (ω' − ω) + h ω' × Iω' = 0`, `J = I + h (skew(ω) I − skew(Iω))`,
//! `ω' = ω − J⁻¹ f(ω)` with `f(ω) = h ω × Iω`.
//!
//! The 3×3 solve is Cramer's rule in binary32 through `det`, in a fixed
//! order, so the result is bit-identical on every target. A body with a
//! locked axis (zero inverse moment, infinite inertia) is skipped: its
//! gyroscopic response is not modelled.

use super::{State, V3, cross, det, rotate, rotate_inv};

/// Apply one implicit gyroscopic step of length `h` to `s.w`.
pub(super) fn implicit_gyroscopic(s: &mut State, h: f32) {
    if !s.inv_inertia.iter().all(|v| *v > 0.0) {
        return;
    }
    let q = s.principal();
    let w = rotate_inv(q, s.w);
    let inertia = [
        det::div(1.0, s.inv_inertia[0]),
        det::div(1.0, s.inv_inertia[1]),
        det::div(1.0, s.inv_inertia[2]),
    ];
    let iw = [
        det::mul(inertia[0], w[0]),
        det::mul(inertia[1], w[1]),
        det::mul(inertia[2], w[2]),
    ];
    let f = cross(w, iw);
    let f = [det::mul(h, f[0]), det::mul(h, f[1]), det::mul(h, f[2])];
    // J = I + h (skew(w) I − skew(Iw)); skew(a) b = a × b, so column k of
    // skew(w) I is w × (I_k e_k).
    let mut j = [[0.0f32; 3]; 3];
    for (k, ik) in inertia.iter().enumerate() {
        let mut e = [0.0f32; 3];
        e[k] = *ik;
        let a = cross(w, e);
        let mut u = [0.0f32; 3];
        u[k] = 1.0;
        let b = cross(iw, u);
        for r in 0..3 {
            j[r][k] = det::mul(h, det::sub(a[r], b[r]));
        }
        j[k][k] = det::add(j[k][k], *ik);
    }
    let Some(delta) = solve3(&j, f) else {
        return;
    };
    let w2 = [
        det::sub(w[0], delta[0]),
        det::sub(w[1], delta[1]),
        det::sub(w[2], delta[2]),
    ];
    if w2.iter().all(|v| v.is_finite()) {
        s.w = rotate(q, w2);
    }
}

fn det3(m: &[[f32; 3]; 3]) -> f32 {
    let c0 = det::sub(det::mul(m[1][1], m[2][2]), det::mul(m[1][2], m[2][1]));
    let c1 = det::sub(det::mul(m[1][0], m[2][2]), det::mul(m[1][2], m[2][0]));
    let c2 = det::sub(det::mul(m[1][0], m[2][1]), det::mul(m[1][1], m[2][0]));
    det::add(
        det::sub(det::mul(m[0][0], c0), det::mul(m[0][1], c1)),
        det::mul(m[0][2], c2),
    )
}

/// `m x = b` by Cramer's rule; `None` for a singular or non-finite system.
fn solve3(m: &[[f32; 3]; 3], b: V3) -> Option<V3> {
    let d = det3(m);
    if !(d.is_finite() && d != 0.0) {
        return None;
    }
    let mut x = [0.0f32; 3];
    for (k, xk) in x.iter_mut().enumerate() {
        let mut mk = *m;
        for r in 0..3 {
            mk[r][k] = b[r];
        }
        *xk = det::div(det3(&mk), d);
    }
    Some(x)
}
