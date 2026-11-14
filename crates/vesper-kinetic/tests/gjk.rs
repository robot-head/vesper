//! GJK distance and EPA penetration against closed-form references.

#![allow(clippy::needless_range_loop)]

use vesper_kinetic::gjk::{GjkError, Shape, distance, intersects, separation};

fn boxv(min: [f64; 3], max: [f64; 3]) -> Vec<[f64; 3]> {
    let mut v = Vec::new();
    for i in 0..8 {
        v.push([
            if i & 1 == 0 { min[0] } else { max[0] },
            if i & 2 == 0 { min[1] } else { max[1] },
            if i & 4 == 0 { min[2] } else { max[2] },
        ]);
    }
    v
}

fn close(a: f64, b: f64, tol: f64) -> bool {
    (a - b).abs() <= tol
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }
    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.next()
    }
}

/// Exact distance between two axis-aligned boxes.
fn box_box_distance(a: ([f64; 3], [f64; 3]), b: ([f64; 3], [f64; 3])) -> f64 {
    let mut s = 0.0;
    for k in 0..3 {
        let gap = (b.0[k] - a.1[k]).max(a.0[k] - b.1[k]).max(0.0);
        s += gap * gap;
    }
    s.sqrt()
}

#[test]
fn sphere_pairs_match_closed_form() {
    let a = Shape::Sphere {
        center: [0.0; 3],
        radius: 1.0,
    };
    for (cb, rb) in [
        ([3.0, 0.0, 0.0], 1.0),
        ([1.0, 2.0, 2.0], 0.5),
        ([0.0, -2.5, 0.0], 1.0),
    ] {
        let b = Shape::Sphere {
            center: cb,
            radius: rb,
        };
        let d = (cb[0] * cb[0] + cb[1] * cb[1] + cb[2] * cb[2]).sqrt();
        let s = separation(&a, &b).unwrap();
        assert!(close(s.distance, d - 1.0 - rb, 1e-12), "{s:?}");
        for k in 0..3 {
            assert!(close(s.normal[k], cb[k] / d, 1e-12));
        }
    }
    // Penetrating, off-centre: depth = r_a + r_b − d along the centre line.
    let b = Shape::Sphere {
        center: [0.3, 0.4, 0.0],
        radius: 1.0,
    };
    let s = separation(&a, &b).unwrap();
    assert!(close(s.distance, 0.5 - 2.0, 1e-9), "{s:?}");
    assert!(close(s.normal[0], 0.6, 1e-9) && close(s.normal[1], 0.8, 1e-9));
    // Concentric: depth is the summed radius.
    let b = Shape::Sphere {
        center: [0.0; 3],
        radius: 0.25,
    };
    assert!(close(separation(&a, &b).unwrap().distance, -1.25, 1e-12));
}

#[test]
fn box_pairs_match_closed_form_distances() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for _ in 0..300 {
        let mk = |rng: &mut Rng| {
            let c = [
                rng.range(-4.0, 4.0),
                rng.range(-4.0, 4.0),
                rng.range(-4.0, 4.0),
            ];
            let h = [
                rng.range(0.1, 1.5),
                rng.range(0.1, 1.5),
                rng.range(0.1, 1.5),
            ];
            (
                [c[0] - h[0], c[1] - h[1], c[2] - h[2]],
                [c[0] + h[0], c[1] + h[1], c[2] + h[2]],
            )
        };
        let (ba, bb) = (mk(&mut rng), mk(&mut rng));
        let (va, vb) = (boxv(ba.0, ba.1), boxv(bb.0, bb.1));
        let (sa, sb) = (
            Shape::Polytope { vertices: &va },
            Shape::Polytope { vertices: &vb },
        );
        let want = box_box_distance(ba, bb);
        let got = distance(&sa, &sb).unwrap();
        assert!(close(got, want, 1e-9), "box distance {got} vs {want}");
        if want > 0.0 {
            let s = separation(&sa, &sb).unwrap();
            let w = [
                s.point_b[0] - s.point_a[0],
                s.point_b[1] - s.point_a[1],
                s.point_b[2] - s.point_a[2],
            ];
            let wl = (w[0] * w[0] + w[1] * w[1] + w[2] * w[2]).sqrt();
            assert!(close(wl, want, 1e-9), "witness gap {wl} vs {want}");
        } else {
            // Penetration depth of overlapping AABBs: the smallest overlap
            // over the three axes.
            let depth = (0..3)
                .map(|k| (ba.1[k] - bb.0[k]).min(bb.1[k] - ba.0[k]))
                .fold(f64::INFINITY, f64::min);
            let s = separation(&sa, &sb).unwrap();
            assert!(
                close(-s.distance, depth, 1e-9),
                "depth {} vs {depth}",
                -s.distance
            );
            assert!(intersects(&sa, &sb).unwrap());
        }
    }
}

#[test]
fn sphere_box_inside_and_outside() {
    let mut rng = Rng(0x0F0F_1E1E_2D2D_3C3C);
    let v = boxv([-1.0, -0.5, -2.0], [1.0, 0.5, 2.0]);
    let b = Shape::Polytope { vertices: &v };
    for _ in 0..300 {
        let p = [
            rng.range(-3.0, 3.0),
            rng.range(-2.0, 2.0),
            rng.range(-4.0, 4.0),
        ];
        let r = rng.range(0.05, 0.6);
        let s = Shape::Sphere {
            center: p,
            radius: r,
        };
        let lo = [-1.0, -0.5, -2.0];
        let hi = [1.0, 0.5, 2.0];
        let inside = (0..3).all(|k| p[k] > lo[k] && p[k] < hi[k]);
        let want = if inside {
            let to_face = (0..3)
                .map(|k| (p[k] - lo[k]).min(hi[k] - p[k]))
                .fold(f64::INFINITY, f64::min);
            -(to_face + r)
        } else {
            let mut d2 = 0.0;
            for k in 0..3 {
                let c = p[k].clamp(lo[k], hi[k]);
                d2 += (p[k] - c) * (p[k] - c);
            }
            d2.sqrt() - r
        };
        let got = separation(&s, &b).unwrap().distance;
        assert!(close(got, want, 1e-9), "p={p:?} r={r}: {got} vs {want}");
    }
}

/// Closest distance between segments, by dense sampling refined in closed
/// form (the reference for capsule pairs).
fn segment_distance(p0: [f64; 3], p1: [f64; 3], q0: [f64; 3], q1: [f64; 3]) -> f64 {
    let sub = |a: [f64; 3], b: [f64; 3]| [a[0] - b[0], a[1] - b[1], a[2] - b[2]];
    let dot = |a: [f64; 3], b: [f64; 3]| a[0] * b[0] + a[1] * b[1] + a[2] * b[2];
    let d1 = sub(p1, p0);
    let d2 = sub(q1, q0);
    let at = |s: f64, t: f64| {
        let a = [p0[0] + d1[0] * s, p0[1] + d1[1] * s, p0[2] + d1[2] * s];
        let b = [q0[0] + d2[0] * t, q0[1] + d2[1] * t, q0[2] + d2[2] * t];
        let w = sub(a, b);
        dot(w, w).sqrt()
    };
    // For each s, the best t is a clamped projection; minimise over s.
    let best_t = |s: f64| {
        let a = [p0[0] + d1[0] * s, p0[1] + d1[1] * s, p0[2] + d1[2] * s];
        (dot(sub(a, q0), d2) / dot(d2, d2)).clamp(0.0, 1.0)
    };
    let mut best = f64::INFINITY;
    let mut best_s = 0.0;
    for i in 0..=2000 {
        let s = i as f64 / 2000.0;
        let d = at(s, best_t(s));
        if d < best {
            best = d;
            best_s = s;
        }
    }
    // Golden-section refinement around the best sample.
    let (mut lo, mut hi) = ((best_s - 1e-3).max(0.0), (best_s + 1e-3).min(1.0));
    for _ in 0..100 {
        let m1 = lo + (hi - lo) * 0.382;
        let m2 = lo + (hi - lo) * 0.618;
        if at(m1, best_t(m1)) < at(m2, best_t(m2)) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    best.min(at(lo, best_t(lo)))
}

#[test]
fn capsule_pairs_match_segment_distances() {
    let mut rng = Rng(0xCAFE_F00D_DEAD_BEEF);
    for _ in 0..150 {
        let mut pt = || {
            [
                rng.range(-3.0, 3.0),
                rng.range(-3.0, 3.0),
                rng.range(-3.0, 3.0),
            ]
        };
        let (a0, a1, b0, b1) = (pt(), pt(), pt(), pt());
        let (ra, rb) = (0.2, 0.3);
        let ca = Shape::Capsule {
            a: a0,
            b: a1,
            radius: ra,
        };
        let cb = Shape::Capsule {
            a: b0,
            b: b1,
            radius: rb,
        };
        let seg = segment_distance(a0, a1, b0, b1);
        let s = separation(&ca, &cb).unwrap();
        if seg > 1e-6 {
            assert!(
                close(s.distance, seg - ra - rb, 1e-7),
                "{} vs {}",
                s.distance,
                seg - ra - rb
            );
        }
    }
    // Crossing segments: the cores intersect, depth is the summed radius.
    let ca = Shape::Capsule {
        a: [-1.0, 0.0, 0.0],
        b: [1.0, 0.0, 0.0],
        radius: 0.2,
    };
    let cb = Shape::Capsule {
        a: [0.0, -1.0, 0.0],
        b: [0.0, 1.0, 0.0],
        radius: 0.3,
    };
    let s = separation(&ca, &cb).unwrap();
    assert!(close(s.distance, -0.5, 1e-12), "{s:?}");
    assert!(
        close(s.normal[2].abs(), 1.0, 1e-12),
        "normal {:?}",
        s.normal
    );
    // Parallel capsules 1.0 apart.
    let cb = Shape::Capsule {
        a: [-0.5, 1.0, 0.0],
        b: [3.0, 1.0, 0.0],
        radius: 0.3,
    };
    assert!(close(distance(&ca, &cb).unwrap(), 0.5, 1e-12));
}

#[test]
fn malformed_shapes_are_typed_errors() {
    let ok = Shape::Sphere {
        center: [0.0; 3],
        radius: 1.0,
    };
    for bad in [
        Shape::Sphere {
            center: [f64::NAN, 0.0, 0.0],
            radius: 1.0,
        },
        Shape::Sphere {
            center: [0.0; 3],
            radius: -1.0,
        },
        Shape::Capsule {
            a: [0.0; 3],
            b: [f64::INFINITY, 0.0, 0.0],
            radius: 0.1,
        },
        Shape::Polytope { vertices: &[] },
    ] {
        assert_eq!(separation(&ok, &bad), Err(GjkError::InvalidShape));
        assert_eq!(separation(&bad, &ok), Err(GjkError::InvalidShape));
    }
    // Two coplanar overlapping squares have no penetration direction.
    let sq = [
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        [1.0, 1.0, 0.0],
    ];
    let sq2 = sq.map(|p| [p[0] + 0.5, p[1] + 0.5, 0.0]);
    assert_eq!(
        separation(
            &Shape::Polytope { vertices: &sq },
            &Shape::Polytope { vertices: &sq2 }
        ),
        Err(GjkError::Degenerate)
    );
}

#[test]
fn tiny_separated_shapes_keep_their_distance() {
    let a = Shape::Sphere {
        center: [0.0; 3],
        radius: 1e-11,
    };
    let b = Shape::Sphere {
        center: [1e-10, 0.0, 0.0],
        radius: 1e-11,
    };
    let s = separation(&a, &b).unwrap();
    assert!((s.distance - 8e-11).abs() < 1e-24, "{s:?}");
    assert_eq!(s.normal, [1.0, 0.0, 0.0]);
    assert!(!intersects(&a, &b).unwrap());
}

#[test]
fn thin_hulls_are_not_mistaken_for_flat_ones() {
    // A long needle whose extreme points win every axis and diagonal
    // direction, overlapping itself.
    let mut v = vec![[1.0, 2.0, 4.0], [-1.0, -2.0, -4.0]];
    for k in 0..3 {
        for s in [0.001, -0.001] {
            let mut p = [0.0; 3];
            p[k] = s;
            v.push(p);
        }
    }
    let n = Shape::Polytope { vertices: &v };
    let s = separation(&n, &n).unwrap();
    assert!(s.distance < 0.0, "{s:?}");
    assert!(intersects(&n, &n).unwrap());
}

/// Two nearly aligned stacked boxes a hair apart (from a stacking
/// simulation): GJK used to cycle on the coplanar faces.
#[test]
fn gjk_terminates_on_nearly_aligned_stacked_boxes() {
    let a: Vec<[f64; 3]> = vec![
        [-0.4893936750384527, 1.000667126788747, -0.5005802245754127],
        [0.5106062815091107, 1.000792684022942, -0.5003135032532205],
        [
            -0.48951940990978887,
            2.000666897045825,
            -0.49991413404511936,
        ],
        [0.5104805466377745, 2.00079245428002, -0.4996474127229271],
        [-0.489660312666894, 1.00000100275123, 0.49941951801615914],
        [0.5103396438806693, 1.0001265599854252, 0.4996862393383514],
        [-0.4897860475382302, 2.000000773008308, 0.5000856085464526],
        [0.5102139090093332, 2.000126330242503, 0.5003523298686448],
    ];
    let b: Vec<[f64; 3]> = vec![
        [
            -0.47982185532880955,
            2.0006819923457813,
            -0.4997925076357459,
        ],
        [0.5201769435440694, 2.002182260655659, -0.49940334466666797],
        [-0.4813220790532645, 3.0006808604161006, -0.499907341749251],
        [0.5186767198196144, 3.002181128725979, -0.49951817878017307],
        [-0.4802111901393623, 2.0007962424898413, 0.5002074100469063],
        [0.5197876087335166, 2.0022965107997197, 0.5005965730159843],
        [-0.48171141386381733, 3.000795110560161, 0.5000925759334012],
        [0.5182873850090617, 3.002295378870039, 0.5004817389024792],
    ];
    let sep = separation(
        &Shape::Polytope { vertices: &a },
        &Shape::Polytope { vertices: &b },
    )
    .unwrap();
    assert!(sep.distance.abs() < 1e-3, "{sep:?}");
    assert!(sep.normal[1] > 0.99, "{sep:?}");
}
