//! Differential checks of the Vesper port against the frozen Rust seed copy
//! (`vesper-kinetic-seed`, here `vesper_kinetic_rust`) that the flip gate
//! runs (crates/vesper-kinetic/flip-gate.json, SH6 lane 9, #1404). Only the
//! port-backed package runs this file; the seed has no Rust build to compare.

use vesper_kinetic::gjk::{GjkError, Shape, separation};
use vesper_kinetic::lbvh::{Aabb, Lbvh};
use vesper_kinetic::sap::{SweepAndPrune, sortable};
use vesper_kinetic::{Colorer, ConstraintBodies};

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn unit(&mut self) -> f64 {
        f64::from(self.next()) / f64::from(u32::MAX)
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }
}

/// The port's colourings (`Colorer`, `MultiColorer`) against the frozen Rust
/// build, visiting order, colours and layers, from small graphs (the
/// comparison sort) to large ones (the port's radix sort of packed keys) and
/// body counts too wide to pack (SH6 lane 9, #1404).
#[test]
fn colourings_match_the_rust_build() {
    use vesper_kinetic_rust as rust;
    let mut rng = Lcg(7);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for round in 0..300u32 {
        let bodies = match round % 4 {
            0 => 2 + rng.next() % 30,
            1 => 64 + rng.next() % 4096,
            2 => 1 << (12 + rng.next() % 9),
            _ => 3_000_000 + rng.next() % 1_000_000,
        };
        let count = match round % 3 {
            0 => rng.next() % 300,
            1 => 256 + rng.next() % 2048,
            _ => rng.next() % 5000,
        } as usize;
        let mut cs = Vec::with_capacity(count);
        for _ in 0..count {
            let a = rng.next() % bodies;
            let b = match rng.next() % 4 {
                0 => None,
                1 => Some((a + 1) % bodies),
                _ => Some(rng.next() % bodies),
            };
            let b = b.filter(|&b| b != a);
            cs.push(ConstraintBodies { a, b });
        }
        let rust_cs: Vec<_> = cs
            .iter()
            .map(|c| rust::ConstraintBodies { a: c.a, b: c.b })
            .collect();
        let mut p = Colorer::new(bodies as usize, count);
        let mut r = rust::Colorer::new(bodies as usize, count);
        let (pr, rr) = (p.color(&cs), r.color(&rust_cs));
        cases += 1;
        assert_eq!(pr.is_ok(), rr.is_ok(), "round {round}: {pr:?} / {rr:?}");
        if pr.is_ok()
            && (p.visiting_order() != r.visiting_order()
                || p.layer_count() != r.layer_count()
                || (0..count).any(|i| p.color_of(i) != r.color_of(i))
                || (0..p.layer_count()).any(|l| p.layer(l) != r.layer(l)))
        {
            differences += 1;
        }

        // MultiColorer over sets of one to four distinct bodies.
        let mut flat = Vec::new();
        let mut offsets = vec![0u32];
        for _ in 0..count {
            let arity = 1 + rng.next() % 4;
            let start = flat.len();
            while flat.len() - start < arity as usize {
                let body = rng.next() % bodies;
                if !flat[start..].contains(&body) {
                    flat.push(body);
                }
                if bodies < 4 && flat.len() - start == bodies as usize {
                    break;
                }
            }
            offsets.push(flat.len() as u32);
        }
        let mut pm = vesper_kinetic::MultiColorer::new(bodies as usize, count);
        let mut rm = rust::MultiColorer::new(bodies as usize, count);
        let (pr, rr) = (pm.color(&flat, &offsets), rm.color(&flat, &offsets));
        cases += 1;
        assert_eq!(pr.is_ok(), rr.is_ok(), "round {round}: {pr:?} / {rr:?}");
        if pr.is_ok()
            && (pm.visiting_order() != rm.visiting_order()
                || pm.layer_count() != rm.layer_count()
                || (0..count).any(|i| pm.color_of(i) != rm.color_of(i))
                || (0..pm.layer_count()).any(|l| pm.layer(l) != rm.layer(l)))
        {
            differences += 1;
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

fn rand_vec(rng: &mut Lcg, span: f64) -> [f64; 3] {
    [
        rng.range(-span, span),
        rng.range(-span, span),
        rng.range(-span, span),
    ]
}

fn rand_polytope(rng: &mut Lcg, center: [f64; 3], n: usize, scale: f64) -> Vec<[f64; 3]> {
    (0..n)
        .map(|_| {
            let v = rand_vec(rng, scale);
            [center[0] + v[0], center[1] + v[1], center[2] + v[2]]
        })
        .collect()
}

fn to_rust_shape<'a>(s: &Shape<'a>) -> vesper_kinetic_rust::gjk::Shape<'a> {
    match *s {
        Shape::Sphere { center, radius } => {
            vesper_kinetic_rust::gjk::Shape::Sphere { center, radius }
        }
        Shape::Capsule { a, b, radius } => {
            vesper_kinetic_rust::gjk::Shape::Capsule { a, b, radius }
        }
        Shape::Polytope { vertices } => vesper_kinetic_rust::gjk::Shape::Polytope { vertices },
    }
}

fn to_rust_err(e: GjkError) -> vesper_kinetic_rust::gjk::GjkError {
    match e {
        GjkError::InvalidShape => vesper_kinetic_rust::gjk::GjkError::InvalidShape,
        GjkError::NoConvergence => vesper_kinetic_rust::gjk::GjkError::NoConvergence,
        GjkError::Degenerate => vesper_kinetic_rust::gjk::GjkError::Degenerate,
    }
}

fn vec_bits_eq(a: [f64; 3], b: [f64; 3]) -> bool {
    a[0].to_bits() == b[0].to_bits()
        && a[1].to_bits() == b[1].to_bits()
        && a[2].to_bits() == b[2].to_bits()
}

/// The port's GJK distance and EPA penetration queries against the frozen
/// Rust seed engine across spheres, capsules, polytopes and error cases.
#[test]
fn gjk_matches_the_rust_build() {
    use vesper_kinetic_rust::gjk as rust;
    let mut rng = Lcg(0x616B_0001);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for round in 0..500u32 {
        let pts_a;
        let pts_b;
        let shift = if round % 3 == 0 { 0.4 } else { 2.2 };
        let ca = rand_vec(&mut rng, 0.5);
        let cb = [ca[0] + shift, ca[1] + rng.range(-0.5, 0.5), ca[2]];
        let sa = match round % 3 {
            0 => Shape::Sphere {
                center: ca,
                radius: rng.range(0.1, 1.5),
            },
            1 => Shape::Capsule {
                a: ca,
                b: [ca[0] + 0.3, ca[1] - 0.2, ca[2] + 0.4],
                radius: rng.range(0.05, 0.9),
            },
            _ => {
                let n = 4 + (rng.next() % 10) as usize;
                pts_a = rand_polytope(&mut rng, ca, n, 1.2);
                Shape::Polytope { vertices: &pts_a }
            }
        };
        let sb = match (round / 3) % 3 {
            0 => Shape::Sphere {
                center: cb,
                radius: rng.range(0.1, 1.5),
            },
            1 => Shape::Capsule {
                a: cb,
                b: [cb[0] - 0.2, cb[1] + 0.3, cb[2] - 0.1],
                radius: rng.range(0.05, 0.9),
            },
            _ => {
                let n = 4 + (rng.next() % 10) as usize;
                pts_b = rand_polytope(&mut rng, cb, n, 1.2);
                Shape::Polytope { vertices: &pts_b }
            }
        };
        let ra = to_rust_shape(&sa);
        let rb = to_rust_shape(&sb);
        let pr = separation(&sa, &sb);
        let rr = rust::separation(&ra, &rb);
        cases += 1;
        match (pr, rr) {
            (Ok(ps), Ok(rs)) => {
                if ps.distance.to_bits() != rs.distance.to_bits()
                    || !vec_bits_eq(ps.normal, rs.normal)
                    || !vec_bits_eq(ps.point_a, rs.point_a)
                    || !vec_bits_eq(ps.point_b, rs.point_b)
                {
                    differences += 1;
                }
            }
            (Err(pe), Err(re)) => {
                if to_rust_err(pe) != re {
                    differences += 1;
                }
            }
            _ => differences += 1,
        }
    }
    let bad = Shape::Sphere {
        center: [0.0, f64::NAN, 0.0],
        radius: 1.0,
    };
    let good = Shape::Sphere {
        center: [0.0; 3],
        radius: 1.0,
    };
    cases += 1;
    if separation(&bad, &good).err().map(to_rust_err)
        != rust::separation(&to_rust_shape(&bad), &to_rust_shape(&good)).err()
    {
        differences += 1;
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

fn rand_boxes(
    rng: &mut Lcg,
    n: usize,
    world: f64,
    max_half: f64,
) -> (Vec<Aabb>, Vec<vesper_kinetic_rust::lbvh::Aabb>) {
    let mut p = Vec::with_capacity(n);
    let mut r = Vec::with_capacity(n);
    for _ in 0..n {
        let c = [
            rng.range(-world, world) as f32,
            rng.range(-world, world) as f32,
            rng.range(-world, world) as f32,
        ];
        let h = [
            rng.range(0.0, max_half) as f32,
            rng.range(0.0, max_half) as f32,
            rng.range(0.0, max_half) as f32,
        ];
        let min = [c[0] - h[0], c[1] - h[1], c[2] - h[2]];
        let max = [c[0] + h[0], c[1] + h[1], c[2] + h[2]];
        p.push(Aabb { min, max });
        r.push(vesper_kinetic_rust::lbvh::Aabb { min, max });
    }
    (p, r)
}

/// The port's SAP broadphase (`sortable`, `build`, `overlapping_pairs`)
/// against the frozen Rust seed engine.
#[test]
fn sap_matches_the_rust_build() {
    use vesper_kinetic_rust::sap as rust;
    let mut rng = Lcg(0x5A90_0002);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for &x in &[
        -f32::INFINITY,
        -1e9,
        -1.0,
        -0.0,
        0.0,
        1.0,
        1e9,
        f32::INFINITY,
    ] {
        cases += 1;
        if sortable(x) != rust::sortable(x) {
            differences += 1;
        }
    }
    let mut psap = SweepAndPrune::with_capacity(512);
    let mut rsap = rust::SweepAndPrune::with_capacity(512);
    let mut po = Vec::new();
    let mut ro = Vec::new();
    for round in 0..120u32 {
        let n = match round {
            0 => 0,
            1 => 1,
            2 => 2,
            _ => 1 + (rng.next() % 512) as usize,
        };
        let (pb, rb) = rand_boxes(&mut rng, n, 25.0, 2.5);
        let pr = psap.build(&pb);
        let rr = rsap.build(&rb);
        cases += 1;
        if pr.is_ok() != rr.is_ok() {
            differences += 1;
            continue;
        }
        if pr.is_ok() {
            if psap.axis() != rsap.axis() || psap.order() != rsap.order() {
                differences += 1;
            }
            let pq = psap.overlapping_pairs(&mut po, 20_000);
            let rq = rsap.overlapping_pairs(&mut ro, 20_000);
            cases += 1;
            if pq.is_ok() != rq.is_ok() || (pq.is_ok() && po != ro) {
                differences += 1;
            }
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

/// The port's LBVH (`morton64`, `build`, `overlapping_pairs`) against the
/// Rust build (not yet flipped; checks 0 differences for the speed edits).
#[test]
fn lbvh_matches_the_rust_build() {
    use vesper_kinetic::lbvh::{demorton64, morton64};
    use vesper_kinetic_rust::lbvh as rust;
    let mut rng = Lcg(0x1B04_0003);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for _ in 0..200 {
        let x = rng.next() & 0x1f_ffff;
        let y = rng.next() & 0x1f_ffff;
        let z = rng.next() & 0x1f_ffff;
        cases += 1;
        if morton64(x, y, z) != rust::morton64(x, y, z)
            || demorton64(morton64(x, y, z)) != rust::demorton64(rust::morton64(x, y, z))
        {
            differences += 1;
        }
    }
    let mut pl = Lbvh::with_capacity(512);
    let mut rl = rust::Lbvh::with_capacity(512);
    let mut po = Vec::new();
    let mut ro = Vec::new();
    for round in 0..80u32 {
        let n = match round {
            0 => 0,
            1 => 1,
            _ => 1 + (rng.next() % 512) as usize,
        };
        let (pb, rb) = rand_boxes(&mut rng, n, 25.0, 2.5);
        let pr = pl.build(&pb);
        let rr = rl.build(&rb);
        cases += 1;
        if pr.is_ok() != rr.is_ok() {
            differences += 1;
            continue;
        }
        if pr.is_ok() {
            let pq = pl.overlapping_pairs(&mut po, 20_000);
            let rq = rl.overlapping_pairs(&mut ro, 20_000);
            cases += 1;
            if pq.is_ok() != rq.is_ok() || (pq.is_ok() && po != ro) {
                differences += 1;
            }
        }
    }
    // Benchmark-sized trees, and small pair bounds (the port's pair scratch
    // then holds fewer entries than the tree has nodes, so the query tests
    // every subtree instead of pruning the ones below the query leaf).
    let mut pl = Lbvh::with_capacity(4096);
    let mut rl = rust::Lbvh::with_capacity(4096);
    for round in 0..12u32 {
        let n = 2048 + (rng.next() % 2049) as usize;
        let (pb, rb) = rand_boxes(&mut rng, n, 64.0, 0.8);
        cases += 1;
        if pl.build(&pb).is_ok() != rl.build(&rb).is_ok() {
            differences += 1;
            continue;
        }
        for max in [usize::MAX, 64, 1 + (rng.next() % 4096) as usize] {
            let max = if round % 2 == 0 { max } else { max.min(n / 2) };
            let pq = pl.overlapping_pairs(&mut po, max);
            let rq = rl.overlapping_pairs(&mut ro, max);
            cases += 1;
            if format!("{pq:?}") != format!("{rq:?}") || (pq.is_ok() && po != ro) {
                differences += 1;
            }
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

/// The port's fast math (`sin`, `cos`, `atan2`, `exp`, `log` and their
/// binary32 forms) against the frozen Rust build, bit for bit: random
/// arguments over the fast reduction's range and beyond it (the correctly
/// rounded fallback), every binade's edges, random bit patterns, and the
/// special values (SH6 lane 9, #1404). A NaN result only has to be a NaN:
/// Rust leaves the payload of an operation on two NaNs unspecified (LLVM
/// may swap the operands of `x + y`), so the Rust build's own payload is not
/// stable either.
#[test]
fn fastmath_matches_the_rust_build() {
    use vesper_kinetic::fastmath as port;
    use vesper_kinetic_rust::fastmath as rust;
    let mut rng = Lcg(0x1B04_0004);
    let mut args: Vec<f64> = vec![
        0.0,
        -0.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        -f64::NAN,
        f64::MIN_POSITIVE,
        -f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::MAX,
        f64::MIN,
        std::f64::consts::FRAC_PI_4,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::PI,
        1_647_099.0,
        -1_647_099.0,
        709.782_712_893_384,
        -745.133_219_101_941_1,
        1.5,
        -1.5,
    ];
    for e in -1074i64..=1023 {
        // 2^e from its bits (powi underflows to 0 for the smallest e).
        let p = f64::from_bits(if e >= -1022 {
            ((e + 1023) as u64) << 52
        } else {
            1 << (e + 1074)
        });
        args.extend([
            p,
            -p,
            f64::from_bits(p.to_bits() + 1),
            f64::from_bits(p.to_bits() - 1),
        ]);
    }
    for _ in 0..20_000 {
        args.push(rng.range(-200.0, 200.0));
        args.push(rng.range(-2.0e6, 2.0e6));
        args.push(f64::from_bits(
            (u64::from(rng.next()) << 33) | u64::from(rng.next()),
        ));
    }
    let mut differences = 0usize;
    let mut cases = 0usize;
    let mut check = |what: &str, x: f64, port: f64, rust: f64| {
        cases += 1;
        if port.to_bits() != rust.to_bits() && !(port.is_nan() && rust.is_nan()) {
            differences += 1;
            println!(
                "{what}({x:e} = {:#x}): port {:#x}, rust {:#x}",
                x.to_bits(),
                port.to_bits(),
                rust.to_bits()
            );
        }
    };
    for (i, &x) in args.iter().enumerate() {
        let other = args[(i * 7 + 3) % args.len()];
        check("sin", x, port::sin(x), rust::sin(x));
        check("cos", x, port::cos(x), rust::cos(x));
        check("exp", x, port::exp(x), rust::exp(x));
        check("log", x, port::log(x), rust::log(x));
        check("atan2", x, port::atan2(x, 1.5), rust::atan2(x, 1.5));
        check("atan2", x, port::atan2(x, other), rust::atan2(x, other));
        let (f, g) = (x as f32, other as f32);
        let wide = |v: f32| f64::from(v);
        check("sinf", x, wide(port::sinf(f)), wide(rust::sinf(f)));
        check("cosf", x, wide(port::cosf(f)), wide(rust::cosf(f)));
        check("expf", x, wide(port::expf(f)), wide(rust::expf(f)));
        check("logf", x, wide(port::logf(f)), wide(rust::logf(f)));
        check(
            "atan2f",
            x,
            wide(port::atan2f(f, g)),
            wide(rust::atan2f(f, g)),
        );
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

/// The port's 30-bit Morton codes (by lookup tables) against the frozen
/// Rust build (magic bits): every in-range axis value through `spread`,
/// `compact` and the masked forms, random and out-of-range coordinates and
/// codes through the checked forms, errors included (SH6 lane 9, #1404).
#[test]
fn morton_matches_the_rust_build() {
    use vesper_kinetic::morton as port;
    use vesper_kinetic_rust::morton as rust;
    let mut rng = Lcg(0x1B04_0005);
    let mut differences = 0usize;
    let mut cases = 0usize;
    let mut check = |same: bool| {
        cases += 1;
        if !same {
            differences += 1;
        }
    };
    for v in 0..=u32::from(u16::MAX) {
        check(port::spread(v) == rust::spread(v));
        check(port::compact(v) == rust::compact(v));
    }
    let edges = [0u16, 1, 511, 512, 1022, 1023, 1024, 1025, 2047, u16::MAX];
    for &x in &edges {
        for &y in &edges {
            for &z in &edges {
                check(
                    format!("{:?}", port::encode(x, y, z))
                        == format!("{:?}", rust::encode(x, y, z)),
                );
                check(port::encode_masked(x, y, z) == rust::encode_masked(x, y, z));
            }
        }
    }
    for _ in 0..200_000 {
        let r = rng.next();
        let (x, y, z) = (
            r as u16 & 0x7ff,
            (r >> 11) as u16 & 0x7ff,
            rng.next() as u16,
        );
        check(format!("{:?}", port::encode(x, y, z)) == format!("{:?}", rust::encode(x, y, z)));
        check(port::encode_masked(x, y, z) == rust::encode_masked(x, y, z));
        let code = rng.next() ^ (rng.next() << 1);
        check(format!("{:?}", port::decode(code)) == format!("{:?}", rust::decode(code)));
        check(port::decode_masked(code) == rust::decode_masked(code));
        let valid = code & port::CODE_MASK;
        check(format!("{:?}", port::decode(valid)) == format!("{:?}", rust::decode(valid)));
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

/// The port's correctly rounded `sin`, `cos`, `exp`, `log` and `atan2`
/// (Vesper bignum, 64-bit limbs) against the frozen Rust build
/// (`num-bigint`), bit for bit in both formats: the Annex F specials and
/// edges, then random inputs over the ranges the reductions and the
/// over/underflow limits split (SH6 lane 9, #1404).
#[test]
fn crmath_matches_the_rust_build() {
    use vesper_kinetic::crmath as port;
    use vesper_kinetic_rust::crmath as rust;
    let mut rng = Lcg(0x1B04_0006);
    let mut differences = 0usize;
    let mut cases = 0usize;
    // Results are correctly rounded, so they agree bit for bit; NaNs are
    // the same canonical NaN, compared as NaN.
    let mut check64 = |a: f64, b: f64| {
        cases += 1;
        if a.to_bits() != b.to_bits() && !(a.is_nan() && b.is_nan()) {
            differences += 1;
        }
    };
    let mut inputs = vec![
        0.0,
        -0.0,
        1.0,
        -1.0,
        0.5,
        2.0,
        f64::INFINITY,
        f64::NEG_INFINITY,
        f64::NAN,
        f64::MIN_POSITIVE,
        f64::from_bits(1),
        f64::MAX,
        -f64::MAX,
        std::f64::consts::PI,
        std::f64::consts::FRAC_PI_2,
        std::f64::consts::LN_2,
        709.782712893384,
        -745.1332191019411,
        1e300,
        1e-300,
    ];
    for _ in 0..1_500 {
        inputs.push(rng.range(-8.0, 8.0));
        inputs.push(rng.range(-800.0, 800.0));
        inputs.push(rng.range(-1e-3, 1e-3));
        let bits = (u64::from(rng.next()) << 32) | u64::from(rng.next());
        inputs.push(f64::from_bits(bits));
    }
    for &x in &inputs {
        check64(port::sin(x), rust::sin(x));
        check64(port::cos(x), rust::cos(x));
        check64(port::exp(x), rust::exp(x));
        check64(port::log(x), rust::log(x));
        check64(port::log(x.abs()), rust::log(x.abs()));
        check64(port::atan2(x, 1.5), rust::atan2(x, 1.5));
        check64(port::atan2(-0.75, x), rust::atan2(-0.75, x));
        let xf = x as f32;
        let yf = (x * 0.37) as f32;
        check64(f64::from(port::sinf(xf)), f64::from(rust::sinf(xf)));
        check64(f64::from(port::cosf(xf)), f64::from(rust::cosf(xf)));
        check64(f64::from(port::expf(xf)), f64::from(rust::expf(xf)));
        check64(
            f64::from(port::logf(xf.abs())),
            f64::from(rust::logf(xf.abs())),
        );
        check64(
            f64::from(port::atan2f(yf, xf)),
            f64::from(rust::atan2f(yf, xf)),
        );
    }
    for &y in &inputs[..20] {
        for &x in &inputs[..20] {
            check64(port::atan2(y, x), rust::atan2(y, x));
            check64(
                f64::from(port::atan2f(y as f32, x as f32)),
                f64::from(rust::atan2f(y as f32, x as f32)),
            );
        }
    }
    println!("flip-gate differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

#[test]
fn sleep_matches_the_rust_build() {
    use vesper_kinetic::{
        ColliderShape as PShape, RigidBody as PBody, RigidCollider as PCollider,
        RigidConfig as PConfig, RigidMaterial as PMat, RigidWorld as PWorld, SleepConfig as PSleep,
    };
    use vesper_kinetic_rust::{
        ColliderShape as RShape, RigidBody as RBody, RigidCollider as RCollider,
        RigidConfig as RConfig, RigidMaterial as RMat, RigidWorld as RWorld, SleepConfig as RSleep,
    };

    let mut rng = Lcg(0x736c_6565_7031_u64);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for _ in 0..64 {
        let count = 8 + (rng.next() % 8) as usize;
        let mut pbodies = Vec::with_capacity(count);
        let mut rbodies = Vec::with_capacity(count);
        for i in 0..count {
            let x = [
                (i as f32) * 0.1 + rng.range(-0.05, 0.05) as f32,
                0.5 + (i as f32) * 1.01,
                rng.range(-0.05, 0.05) as f32,
            ];
            let v = [
                rng.range(-0.02, 0.02) as f32,
                rng.range(-0.02, 0.02) as f32,
                rng.range(-0.02, 0.02) as f32,
            ];
            let mut pb = PBody::new(x, 1.0, [2.0, 2.0, 2.0], 0);
            pb.vel = v;
            pbodies.push(pb);
            let mut rb = RBody::new(x, 1.0, [2.0, 2.0, 2.0], 0);
            rb.vel = v;
            rbodies.push(rb);
        }
        let mut pw = PWorld::new(
            PConfig {
                dt: 1.0 / 60.0,
                substeps: 4,
                ..PConfig::default()
            },
            &pbodies,
            &[PCollider {
                shape: PShape::Box {
                    half_extents: [0.5, 0.5, 0.5],
                },
                material: PMat::default(),
            }],
        )
        .unwrap();
        pw.set_sleeping(Some(PSleep {
            linear_threshold: 0.2,
            angular_threshold: 0.2,
            ticks: 4,
        }))
        .unwrap();

        let mut rw = RWorld::new(
            RConfig {
                dt: 1.0 / 60.0,
                substeps: 4,
                ..RConfig::default()
            },
            &rbodies,
            &[RCollider {
                shape: RShape::Box {
                    half_extents: [0.5, 0.5, 0.5],
                },
                material: RMat::default(),
            }],
        )
        .unwrap();
        rw.set_sleeping(Some(RSleep {
            linear_threshold: 0.2,
            angular_threshold: 0.2,
            ticks: 4,
        }))
        .unwrap();

        for step in 0..30 {
            if step == 15 {
                pw.wake(0);
                rw.wake(0);
            }
            pw.step().unwrap();
            rw.step().unwrap();
            cases += 1;
            if pw.hash().blake3 != rw.hash().blake3 || pw.sleeping_count() != rw.sleeping_count() {
                differences += 1;
            }
        }
    }
    println!("sleep differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

#[test]
#[ignore = "ABBA benchmark; run in release with --ignored --nocapture"]
fn abba_sleep() {
    use std::hint::black_box;
    use vesper_bench::{BenchCase, Invocation, RunConfig, run_abba};
    use vesper_kinetic::{
        ColliderShape as PShape, RigidBody as PBody, RigidCollider as PCollider,
        RigidConfig as PConfig, RigidMaterial as PMat, RigidWorld as PWorld, SleepConfig as PSleep,
    };
    use vesper_kinetic_rust::{
        ColliderShape as RShape, RigidBody as RBody, RigidCollider as RCollider,
        RigidConfig as RConfig, RigidMaterial as RMat, RigidWorld as RWorld, SleepConfig as RSleep,
    };

    let body_count = 64usize;
    let mut pbodies = Vec::with_capacity(body_count);
    let mut rbodies = Vec::with_capacity(body_count);
    for i in 0..body_count {
        let x = [(i as f32) * 2.0, 0.5, 0.0];
        pbodies.push(PBody::new(x, 1.0, [2.0, 2.0, 2.0], 0));
        rbodies.push(RBody::new(x, 1.0, [2.0, 2.0, 2.0], 0));
    }
    let mut pw = PWorld::new(
        PConfig {
            dt: 1.0 / 60.0,
            substeps: 2,
            ..PConfig::default()
        },
        &pbodies,
        &[PCollider {
            shape: PShape::Box {
                half_extents: [0.5, 0.5, 0.5],
            },
            material: PMat::default(),
        }],
    )
    .unwrap();
    pw.set_sleeping(Some(PSleep {
        linear_threshold: 0.1,
        angular_threshold: 0.1,
        ticks: 1000,
    }))
    .unwrap();

    let mut rw = RWorld::new(
        RConfig {
            dt: 1.0 / 60.0,
            substeps: 2,
            ..RConfig::default()
        },
        &rbodies,
        &[RCollider {
            shape: RShape::Box {
                half_extents: [0.5, 0.5, 0.5],
            },
            material: RMat::default(),
        }],
    )
    .unwrap();
    rw.set_sleeping(Some(RSleep {
        linear_threshold: 0.1,
        angular_threshold: 0.1,
        ticks: 1000,
    }))
    .unwrap();

    struct StepCase<F>(F);
    impl<F: FnMut() -> usize> BenchCase for StepCase<F> {
        type Input = ();
        type Output = usize;
        type Error = std::convert::Infallible;
        fn setup(&mut self, _: Invocation) -> Result<Self::Input, Self::Error> {
            Ok(())
        }
        fn run(&mut self, _: Self::Input) -> Result<Self::Output, Self::Error> {
            Ok(black_box((self.0)()))
        }
    }

    let cfg = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let mut r_case = StepCase(|| {
        rw.step().unwrap();
        rw.sleeping_count()
    });
    let mut p_case = StepCase(|| {
        pw.step().unwrap();
        pw.sleeping_count()
    });
    let report = run_abba(cfg, &mut r_case, &mut p_case).unwrap();
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let mid = ratios.len() / 2;
    let mut cdf = 0.0;
    let mut choose = 1.0;
    let mut k = 0usize;
    for j in 0..ratios.len() {
        cdf += choose * 0.5f64.powi(ratios.len() as i32);
        if cdf > 0.025 {
            break;
        }
        k = j + 1;
        choose = choose * (ratios.len() - j) as f64 / (j + 1) as f64;
    }
    let i = k.saturating_sub(1).min(mid);
    let (ci_lo, ci_hi) = (ratios[i], ratios[ratios.len() - 1 - i]);
    println!(
        "ABBA sleep: median {:.3}x [{ci_lo:.3}x, {ci_hi:.3}x]",
        ratios[mid]
    );
    assert!(
        ci_hi <= 1.50,
        "ABBA 95% upper bound {ci_hi:.3}x exceeds 1.50x"
    );
}

#[test]
fn diff_matches_the_rust_build() {
    use vesper_kinetic::diff::{
        DiffConstraint as PConstraint, DiffContacts as PContacts, DiffOptions as POptions,
        DiffOrder as POrder, DiffParticle as PParticle, DiffScene as PScene,
        gradients_with as p_grad,
    };
    use vesper_kinetic_rust::diff::{
        DiffConstraint as RConstraint, DiffContacts as RContacts, DiffOptions as ROptions,
        DiffOrder as ROrder, DiffParticle as RParticle, DiffScene as RScene,
        gradients_with as r_grad,
    };

    let mut rng = Lcg(0x6469_6666_5f31_u64);
    let mut differences = 0usize;
    let mut cases = 0usize;
    for _ in 0..64 {
        let n = 6 + (rng.next() % 6) as usize;
        let mut p_parts = Vec::with_capacity(n);
        let mut r_parts = Vec::with_capacity(n);
        let mut radii = Vec::with_capacity(n);
        let mut sp = Vec::with_capacity(n);
        let mut sv = Vec::with_capacity(n);
        for i in 0..n {
            let pos = [
                (i as f64) * 0.45 + rng.range(-0.1, 0.1),
                0.25 + (i as f64) * 0.15,
                rng.range(-0.1, 0.1),
            ];
            let vel = [
                rng.range(-1.0, 1.0),
                rng.range(-0.5, 0.5),
                rng.range(-1.0, 1.0),
            ];
            let inv_mass = if i == 0 { 0.0 } else { rng.range(0.5, 2.0) };
            p_parts.push(PParticle { pos, vel, inv_mass });
            r_parts.push(RParticle { pos, vel, inv_mass });
            radii.push(0.25);
            sp.push([
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
            ]);
            sv.push([
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
                rng.range(-1.0, 1.0),
            ]);
        }
        let mut p_cons = Vec::new();
        let mut r_cons = Vec::new();
        for i in 0..n - 1 {
            let rest_length = 0.45;
            let compliance = 1e-4;
            p_cons.push(PConstraint {
                a: i,
                b: i + 1,
                rest_length,
                compliance,
            });
            r_cons.push(RConstraint {
                a: i,
                b: i + 1,
                rest_length,
                compliance,
            });
        }
        let p_scene = PScene {
            particles: p_parts,
            constraints: p_cons,
            gravity: [0.0, -9.81, 0.0],
            dt: 1.0 / 60.0,
            substeps: 4,
            ticks: 6,
        };
        let r_scene = RScene {
            particles: r_parts,
            constraints: r_cons,
            gravity: [0.0, -9.81, 0.0],
            dt: 1.0 / 60.0,
            substeps: 4,
            ticks: 6,
        };
        let p_opts = POptions {
            order: POrder::WorldColors,
            contacts: Some(PContacts {
                radii: radii.clone(),
                compliance: vec![1e-5; n],
                margin: 0.05,
                ground_y: Some(0.0),
            }),
        };
        let r_opts = ROptions {
            order: ROrder::WorldColors,
            contacts: Some(RContacts {
                radii,
                compliance: vec![1e-5; n],
                margin: 0.05,
                ground_y: Some(0.0),
            }),
        };
        let pg = p_grad(&p_scene, &p_opts, &sp, &sv).unwrap();
        let rg = r_grad(&r_scene, &r_opts, &sp, &sv).unwrap();
        cases += 1;
        if pg.gradients.loss != rg.gradients.loss
            || pg.gradients.d_pos0 != rg.gradients.d_pos0
            || pg.gradients.d_vel0 != rg.gradients.d_vel0
            || pg.gradients.d_compliance != rg.gradients.d_compliance
            || pg.d_radius != rg.d_radius
            || pg.d_contact_compliance != rg.d_contact_compliance
        {
            differences += 1;
        }
    }
    println!("diff differential differences={differences} cases={cases}");
    assert_eq!(differences, 0);
}

#[test]
#[ignore = "ABBA benchmark; run in release with --ignored --nocapture"]
fn abba_diff() {
    use std::hint::black_box;
    use vesper_bench::{BenchCase, Invocation, RunConfig, run_abba};
    use vesper_kinetic::diff::{
        DiffConstraint as PConstraint, DiffContacts as PContacts, DiffOptions as POptions,
        DiffOrder as POrder, DiffParticle as PParticle, DiffScene as PScene,
        gradients_with as p_grad,
    };
    use vesper_kinetic_rust::diff::{
        DiffConstraint as RConstraint, DiffContacts as RContacts, DiffOptions as ROptions,
        DiffOrder as ROrder, DiffParticle as RParticle, DiffScene as RScene,
        gradients_with as r_grad,
    };

    let n = 16usize;
    let mut p_parts = Vec::with_capacity(n);
    let mut r_parts = Vec::with_capacity(n);
    let mut radii = Vec::with_capacity(n);
    let mut sp = Vec::with_capacity(n);
    let mut sv = Vec::with_capacity(n);
    for i in 0..n {
        let pos = [(i as f64) * 0.4, 0.25 + (i as f64) * 0.1, 0.0];
        let vel = [0.2, -0.1, 0.0];
        let inv_mass = if i == 0 { 0.0 } else { 1.0 };
        p_parts.push(PParticle { pos, vel, inv_mass });
        r_parts.push(RParticle { pos, vel, inv_mass });
        radii.push(0.25);
        sp.push([1.0, 0.5, -0.25]);
        sv.push([0.1, -0.2, 0.3]);
    }
    let mut p_cons = Vec::new();
    let mut r_cons = Vec::new();
    for i in 0..n - 1 {
        p_cons.push(PConstraint {
            a: i,
            b: i + 1,
            rest_length: 0.4,
            compliance: 1e-4,
        });
        r_cons.push(RConstraint {
            a: i,
            b: i + 1,
            rest_length: 0.4,
            compliance: 1e-4,
        });
    }
    let p_scene = PScene {
        particles: p_parts,
        constraints: p_cons,
        gravity: [0.0, -9.81, 0.0],
        dt: 1.0 / 60.0,
        substeps: 4,
        ticks: 10,
    };
    let r_scene = RScene {
        particles: r_parts,
        constraints: r_cons,
        gravity: [0.0, -9.81, 0.0],
        dt: 1.0 / 60.0,
        substeps: 4,
        ticks: 10,
    };
    let p_opts = POptions {
        order: POrder::WorldColors,
        contacts: Some(PContacts {
            radii: radii.clone(),
            compliance: vec![1e-5; n],
            margin: 0.05,
            ground_y: Some(0.0),
        }),
    };
    let r_opts = ROptions {
        order: ROrder::WorldColors,
        contacts: Some(RContacts {
            radii,
            compliance: vec![1e-5; n],
            margin: 0.05,
            ground_y: Some(0.0),
        }),
    };

    struct GradCase<F>(F);
    impl<F: FnMut() -> f64> BenchCase for GradCase<F> {
        type Input = ();
        type Output = f64;
        type Error = std::convert::Infallible;
        fn setup(&mut self, _: Invocation) -> Result<Self::Input, Self::Error> {
            Ok(())
        }
        fn run(&mut self, _: Self::Input) -> Result<Self::Output, Self::Error> {
            Ok(black_box((self.0)()))
        }
    }

    let cfg = RunConfig {
        epochs: 30,
        warmup_epochs: 2,
        operations_per_window: 8,
        max_total_calls: 100_000,
        max_elapsed_nanos: 600_000_000_000,
        layout_seed: 1,
    };
    let mut r_case = GradCase(|| {
        r_grad(&r_scene, &r_opts, &sp, &sv)
            .unwrap()
            .gradients
            .d_pos0[1][0]
    });
    let mut p_case = GradCase(|| {
        p_grad(&p_scene, &p_opts, &sp, &sv)
            .unwrap()
            .gradients
            .d_pos0[1][0]
    });
    let report = run_abba(cfg, &mut r_case, &mut p_case).unwrap();
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let mid = ratios.len() / 2;
    let mut cdf = 0.0;
    let mut choose = 1.0;
    let mut k = 0usize;
    for j in 0..ratios.len() {
        cdf += choose * 0.5f64.powi(ratios.len() as i32);
        if cdf > 0.025 {
            break;
        }
        k = j + 1;
        choose = choose * (ratios.len() - j) as f64 / (j + 1) as f64;
    }
    let i = k.saturating_sub(1).min(mid);
    let (ci_lo, ci_hi) = (ratios[i], ratios[ratios.len() - 1 - i]);
    println!(
        "ABBA diff: median {:.3}x [{ci_lo:.3}x, {ci_hi:.3}x]",
        ratios[mid]
    );
    assert!(
        ci_hi <= 1.50,
        "ABBA 95% upper bound {ci_hi:.3}x exceeds 1.50x"
    );
}

/// One random rigid scene, built the same way in either build: a stack of
/// boxes, spheres and capsules with random materials, inertia frames and
/// spins on the ground, and hinge, prismatic and ball joints with limits,
/// drives and friction (`variant` picks the mix; 3 also sleeps).
macro_rules! rigid_solve_scene {
    ($k:ident, $seed:expr, $variant:expr) => {{
        use $k::rigid::{
            ColliderShape, JointDrive, JointKind, RigidBody, RigidCollider, RigidConfig,
            RigidJoint, RigidMaterial, RigidWorld, SleepConfig,
        };
        let mut rng = Lcg($seed);
        let variant: u32 = $variant;
        let mut colliders = Vec::new();
        for c in 0..4u32 {
            let material = RigidMaterial {
                static_friction: rng.range(0.0, 1.0) as f32,
                dynamic_friction: rng.range(0.0, 0.8) as f32,
                restitution: if c == 1 {
                    rng.range(0.2, 0.9) as f32
                } else {
                    0.0
                },
                rolling_friction: if c == 2 {
                    rng.range(0.0, 0.05) as f32
                } else {
                    0.0
                },
            };
            let shape = match c {
                0 => ColliderShape::Box {
                    half_extents: [0.4, 0.3, 0.35],
                },
                1 => ColliderShape::Sphere { radius: 0.3 },
                2 => ColliderShape::Capsule {
                    half_height: 0.3,
                    radius: 0.2,
                },
                _ => ColliderShape::Box {
                    half_extents: [0.2, 0.2, 0.2],
                },
            };
            colliders.push(RigidCollider { shape, material });
        }
        let count = 6 + (rng.next() % 10) as usize;
        let mut bodies = Vec::with_capacity(count);
        for i in 0..count {
            let collider = rng.next() % 4;
            let pos = [
                rng.range(-0.3, 0.3) as f32,
                0.5 + i as f32 * 0.72,
                rng.range(-0.3, 0.3) as f32,
            ];
            let inv_mass = if i == 0 && variant == 2 {
                0.0
            } else {
                rng.range(0.3, 2.0) as f32
            };
            let mut b = RigidBody::new(
                pos,
                inv_mass,
                [
                    rng.range(0.5, 6.0) as f32,
                    rng.range(0.5, 6.0) as f32,
                    rng.range(0.5, 6.0) as f32,
                ],
                collider,
            );
            b.vel = [
                rng.range(-1.0, 1.0) as f32,
                rng.range(-1.0, 0.5) as f32,
                rng.range(-1.0, 1.0) as f32,
            ];
            b.ang_vel = [
                rng.range(-4.0, 4.0) as f32,
                rng.range(-4.0, 4.0) as f32,
                rng.range(-4.0, 4.0) as f32,
            ];
            if rng.next() % 3 == 0 {
                let (s, c) = (rng.range(-0.5, 0.5) as f32).sin_cos();
                b.inertia_frame = [s, 0.0, 0.0, c];
            }
            if rng.next() % 4 == 0 {
                let (s, c) = (rng.range(-1.0, 1.0) as f32).sin_cos();
                b.orientation = [0.0, s, 0.0, c];
            }
            bodies.push(b);
        }
        let mut w = RigidWorld::new(
            RigidConfig {
                dt: 1.0 / 60.0,
                substeps: 1 + rng.next() % 8,
                ..RigidConfig::default()
            },
            &bodies,
            &colliders,
        )
        .unwrap();
        w.set_gyroscopic(rng.next() % 2 == 0);
        if variant == 3 {
            w.set_sleeping(Some(SleepConfig {
                linear_threshold: 0.3,
                angular_threshold: 0.3,
                ticks: 3,
            }))
            .unwrap();
        }
        let joints = if variant == 0 { 0 } else { 2 + rng.next() % 5 };
        for _ in 0..joints {
            let a = rng.next() % count as u32;
            let b = if rng.next() % 3 == 0 {
                None
            } else {
                Some((a + 1 + rng.next() % (count as u32 - 1)) % count as u32)
            };
            let limits = if rng.next() % 2 == 0 {
                let lo = rng.range(-1.5, 0.0) as f32;
                Some([lo, lo + rng.range(0.1, 2.0) as f32])
            } else {
                None
            };
            let axis = [
                rng.range(-1.0, 1.0) as f32,
                rng.range(0.2, 1.0) as f32,
                rng.range(-1.0, 1.0) as f32,
            ];
            let kind = match (rng.next() % 3, variant) {
                (0, _) => JointKind::Ball,
                (1, _) | (_, 1) => JointKind::Hinge {
                    axis_a: axis,
                    axis_b: axis,
                    limits,
                },
                _ => JointKind::Prismatic {
                    axis_a: axis,
                    limits,
                },
            };
            let anchor_b = match b {
                None => {
                    let p = bodies[a as usize].pos;
                    [p[0], p[1] + 0.4, p[2]]
                }
                Some(_) => [0.0, -0.36, 0.0],
            };
            let Ok(i) = w.add_joint(RigidJoint {
                kind,
                compliance: if rng.next() % 2 == 0 { 0.0 } else { 1.0e-4 },
                angular_compliance: if rng.next() % 2 == 0 { 0.0 } else { 1.0e-3 },
                collide_connected: rng.next() % 2 == 0,
                ..RigidJoint::ball(a, b, [0.0, 0.36, 0.0], anchor_b)
            }) else {
                continue;
            };
            if matches!(kind, JointKind::Ball) {
                continue;
            }
            let drive = match rng.next() % 3 {
                0 => None,
                1 => Some(JointDrive::Velocity {
                    target: rng.range(-3.0, 3.0) as f32,
                    max_force: rng.range(0.1, 20.0) as f32,
                }),
                _ => Some(JointDrive::Position {
                    target: rng.range(-0.8, 0.8) as f32,
                    compliance: if rng.next() % 2 == 0 { 0.0 } else { 1.0e-3 },
                    damping: rng.range(0.0, 2.0) as f32,
                    max_force: rng.range(0.5, 50.0) as f32,
                }),
            };
            let _ = w.set_joint_drive(i, drive);
            if rng.next() % 2 == 0 {
                let _ = w.set_joint_friction(i, rng.range(0.0, 2.0) as f32);
            }
        }
        w
    }};
}

/// The port's substep solver (`RigidWorld::solve`, vesper/src/solve.ves)
/// against the frozen Rust build: body hashes, manifolds (with their
/// solver state), joint forces and coordinates after every tick of random
/// stacks with materials, inertia frames, gyroscopic spin, sleeping and
/// hinge, prismatic and ball joints with limits, drives and friction (SH6
/// lane 9, #1404).
#[test]
fn rigid_solve_matches_the_rust_build() {
    let mut differences = 0usize;
    let mut cases = 0usize;
    let mut joints = 0usize;
    let mut contacts = 0usize;
    for round in 0..96u64 {
        let seed = 0x736f_6c76_6500 + round;
        let variant = (round % 4) as u32;
        let mut pw = rigid_solve_scene!(vesper_kinetic, seed, variant);
        let mut rw = rigid_solve_scene!(vesper_kinetic_rust, seed, variant);
        for step in 0..90 {
            if step == 45 && variant == 3 {
                pw.wake(0);
                rw.wake(0);
            }
            let (p, r) = (pw.step(), rw.step());
            assert_eq!(p.is_ok(), r.is_ok(), "round {round} step {step}");
            cases += 1;
            contacts += pw.manifolds().len();
            let mut same = pw.hash().blake3 == rw.hash().blake3
                && format!("{:?}", pw.manifolds()) == format!("{:?}", rw.manifolds());
            for j in 0.. {
                match (pw.joint_force(j), rw.joint_force(j)) {
                    (None, None) => break,
                    (pf, rf) => {
                        joints += 1;
                        same &= pf.map(f32::to_bits) == rf.map(f32::to_bits);
                        same &= pw.joint_coordinate(j).map(f32::to_bits)
                            == rw.joint_coordinate(j).map(f32::to_bits);
                    }
                }
            }
            if !same {
                differences += 1;
            }
        }
    }
    println!(
        "rigid solve differential differences={differences} cases={cases} joints={joints} contacts={contacts}"
    );
    assert!(joints > 1000 && contacts > 1000);
    assert_eq!(differences, 0);
}

/// One random particle world, built the same way in either build: spheres
/// over the ground with random masses (some static), compliance,
/// distance-joint chains, a contact material with friction and restitution,
/// and the scalar or the serial Batched16 solver.
macro_rules! world_scene {
    ($k:ident, $seed:expr) => {{
        use $k::{
            Body, ContactMaterial, DistanceConstraint, SolverMode, SphereCollider, World,
            WorldConfig,
        };
        let mut rng = Lcg($seed);
        let n = 20 + (rng.next() % 120) as usize;
        let mut bodies = Vec::with_capacity(n);
        for i in 0..n {
            bodies.push(Body {
                // A jittered lattice: neighbours touch, none overlap deeply.
                pos: [
                    (i % 6) as f32 * 0.85 + rng.range(-0.05, 0.05) as f32,
                    0.45 + (i / 36) as f32 * 0.85 + rng.range(0.0, 0.05) as f32,
                    ((i / 6) % 6) as f32 * 0.85 + rng.range(-0.05, 0.05) as f32,
                ],
                vel: [
                    rng.range(-0.5, 0.5) as f32,
                    rng.range(-0.5, 0.2) as f32,
                    rng.range(-0.5, 0.5) as f32,
                ],
                inv_mass: if i % 17 == 3 {
                    0.0
                } else {
                    rng.range(0.2, 3.0) as f32
                },
                compliance_alpha: if rng.next() % 2 == 0 { 0.0 } else { 1.0e-4 },
                collider_asset_id: rng.next() % 2,
                collision_layer_mask: 1,
            });
        }
        let mut joints = Vec::new();
        // Chains of consecutive bodies, so no body has many joints.
        for _ in 0..rng.next() % 4 {
            let start = rng.next() % (n as u32 - 8);
            for a in start..start + 6 {
                let b = a + 1;
                joints.push(DistanceConstraint {
                    a,
                    b,
                    rest_length: rng.range(0.8, 0.9) as f32,
                    compliance: if rng.next() % 2 == 0 { 0.0 } else { 1.0e-3 },
                });
            }
        }
        let mut w = World::new(
            WorldConfig {
                substeps: 1 + rng.next() % 6,
                ground_y: Some(rng.range(-0.5, 0.0) as f32),
                cell_size: 1.0,
                contact_margin: 0.05,
                max_contacts: 4096,
                ..WorldConfig::default()
            },
            &bodies,
            &[
                SphereCollider { radius: 0.25 },
                SphereCollider { radius: 0.4 },
            ],
            &joints,
        )
        .unwrap();
        w.set_contact_material(ContactMaterial {
            static_friction: if rng.next() % 3 == 0 {
                0.0
            } else {
                rng.range(0.0, 1.0) as f32
            },
            dynamic_friction: if rng.next() % 3 == 0 {
                0.0
            } else {
                rng.range(0.0, 0.8) as f32
            },
            restitution: if rng.next() % 2 == 0 {
                0.0
            } else {
                rng.range(0.1, 0.9) as f32
            },
        })
        .unwrap();
        if rng.next() % 2 == 0 {
            w.set_solver_mode(SolverMode::Batched16);
        }
        w
    }};
}

/// The port's particle-world substeps (`World::advance`,
/// vesper/src/world.ves) against the frozen Rust build: the world hash after
/// every tick of random sphere piles with joints, friction, restitution and
/// both serial solvers (SH6 lane 9, #1404).
#[test]
fn world_substeps_match_the_rust_build() {
    let mut differences = 0usize;
    let mut cases = 0usize;
    let mut contacts = 0usize;
    let mut errors = 0usize;
    for round in 0..64u64 {
        let seed = 0x776f_726c_6400 + round;
        let mut pw = world_scene!(vesper_kinetic, seed);
        let mut rw = world_scene!(vesper_kinetic_rust, seed);
        for step in 0..60 {
            let (p, r) = (pw.step(), rw.step());
            match (p, r) {
                (Ok(p), Ok(r)) => {
                    cases += 1;
                    contacts += p.manifold_count as usize;
                    if p.world_state_blake3 != r.world_state_blake3 {
                        differences += 1;
                    }
                }
                (p, r) => {
                    assert_eq!(p.is_ok(), r.is_ok(), "round {round} step {step}");
                    errors += 1;
                    break;
                }
            }
        }
    }
    println!(
        "world substeps differential differences={differences} cases={cases} contacts={contacts} errors={errors}"
    );
    // Some scenes stop early where both builds refuse a step alike (a sweep
    // too large for the contact search); the ticks before that compare.
    assert!(cases > 2000 && contacts > 100_000);
    assert_eq!(differences, 0);
}

macro_rules! soft_scene {
    ($k:ident, $seed:expr) => {{
        use $k::{SoftConfig, SoftConstraint, SoftWorld};
        let mut rng = Lcg($seed);
        let cells = [1 + rng.next() % 4, 1 + rng.next() % 3, 1 + rng.next() % 4];
        let (mut particles, mut constraints) = SoftWorld::tetrahedral_box(
            [0.0, 0.2 + rng.range(0.0, 0.6) as f32, 0.0],
            0.2 + rng.range(0.0, 0.3) as f32,
            cells,
            1.0 + rng.range(0.0, 4.0) as f32,
            if rng.next() % 3 == 0 {
                0.0
            } else {
                rng.range(0.0, 1e-3) as f32
            },
            if rng.next() % 3 == 0 {
                0.0
            } else {
                rng.range(0.0, 1e-4) as f32
            },
        )
        .unwrap();
        for p in &mut particles {
            for k in 0..3 {
                p.pos[k] += rng.range(-0.02, 0.02) as f32;
                p.vel[k] = rng.range(-1.0, 1.0) as f32;
            }
            if rng.next() % 9 == 0 {
                p.inv_mass = 0.0;
            }
        }
        // A few long-range springs between random particles.
        let n = particles.len() as u32;
        for _ in 0..rng.next() % 6 {
            let a = rng.next() % n;
            let b = (a + 1 + rng.next() % (n - 1)) % n;
            constraints.push(SoftConstraint::Distance {
                a,
                b,
                rest: rng.range(0.0, 1.0) as f32,
                compliance: rng.range(0.0, 1e-2) as f32,
            });
        }
        let config = SoftConfig {
            dt: 1.0 / (60.0 + (rng.next() % 3) as f32 * 60.0),
            substeps: 1 + rng.next() % 16,
            gravity: [
                rng.range(-1.0, 1.0) as f32,
                -9.81 + rng.range(-2.0, 2.0) as f32,
                rng.range(-1.0, 1.0) as f32,
            ],
            ground: (rng.next() % 4 != 0).then(|| rng.range(-0.1, 0.3) as f32),
            ground_friction: if rng.next() % 3 == 0 {
                0.0
            } else {
                rng.range(0.0, 1.5) as f32
            },
        };
        let mut w = SoftWorld::new(config, &particles, &constraints).unwrap();
        w.set_solver_threads(1 + (rng.next() % 3) as usize).unwrap();
        w
    }};
}

/// The port's soft-body substeps (`SoftWorld::step`, vesper/src/soft.ves)
/// against the frozen Rust build: the world hashes after every tick of
/// random tetrahedral boxes with pinned particles, springs, compliance,
/// ground friction and thread counts (SH6 lane 9, #1404).
#[test]
fn soft_substeps_match_the_rust_build() {
    let mut differences = 0usize;
    let mut cases = 0usize;
    let mut errors = 0usize;
    for round in 0..96u64 {
        let seed = 0x736f_6674_0000 + round;
        let mut pw = soft_scene!(vesper_kinetic, seed);
        let mut rw = soft_scene!(vesper_kinetic_rust, seed);
        for step in 0..90 {
            match (pw.step(), rw.step()) {
                (Ok(p), Ok(r)) => {
                    cases += 1;
                    if p.blake3 != r.blake3 || p.sha256 != r.sha256 {
                        differences += 1;
                    }
                }
                (p, r) => {
                    assert_eq!(
                        format!("{p:?}"),
                        format!("{r:?}"),
                        "round {round} step {step}"
                    );
                    errors += 1;
                    break;
                }
            }
        }
    }
    println!("soft substeps differential differences={differences} cases={cases} errors={errors}");
    assert!(cases > 8000);
    assert_eq!(differences, 0);
}

const COUPLE_DIFF_ROUNDS: u64 = 64;
const COUPLE_DIFF_STEPS: usize = 40;
const COUPLE_SEED_BASE: u64 = 0x636f_7570_0000;
const COUPLE_MIN_CASES: usize = 2000;
const COUPLE_MIN_CONTACTS: usize = 5000;
const COUPLE_SHAPE_COUNT: u32 = 5;
const COUPLE_BASE_DT_HZ: f32 = 120.0;
const COUPLE_DT_STEP_HZ: f32 = 60.0;

macro_rules! couple_scene {
    ($k:ident, $seed:expr) => {{
        use $k::rigid::{
            ColliderShape, RigidBody, RigidCollider, RigidConfig, RigidMaterial, RigidWorld,
            box_inv_inertia,
        };
        use $k::soft::{SoftConfig, SoftWorld};
        use $k::{CoupledWorld, CouplingConfig};

        let mut rng = Lcg($seed);
        let dt = 1.0 / (COUPLE_BASE_DT_HZ + (rng.next() % 2) as f32 * COUPLE_DT_STEP_HZ);
        let gravity = [
            rng.range(-0.5, 0.5) as f32,
            -9.81 + rng.range(-1.0, 1.0) as f32,
            rng.range(-0.5, 0.5) as f32,
        ];

        let colliders = vec![
            RigidCollider {
                shape: ColliderShape::Box {
                    half_extents: [0.8, 0.15, 0.8],
                },
                material: RigidMaterial::default(),
            },
            RigidCollider {
                shape: ColliderShape::Sphere { radius: 0.18 },
                material: RigidMaterial::default(),
            },
            RigidCollider {
                shape: ColliderShape::Capsule {
                    half_height: 0.14,
                    radius: 0.1,
                },
                material: RigidMaterial::default(),
            },
            RigidCollider {
                shape: ColliderShape::ConvexHull {
                    vertices: vec![
                        [-0.18, -0.12, -0.18],
                        [0.18, -0.12, -0.18],
                        [0.0, -0.12, 0.2],
                        [0.0, 0.18, 0.0],
                    ],
                },
                material: RigidMaterial::default(),
            },
            RigidCollider {
                shape: ColliderShape::Segment {
                    a: [-0.15, -0.05, 0.0],
                    b: [0.15, 0.05, 0.0],
                    radius: 0.09,
                },
                material: RigidMaterial::default(),
            },
        ];

        let mut bodies = Vec::new();
        // Static platform underneath the soft jelly.
        bodies.push(RigidBody::new([0.0, 0.15, 0.0], 0.0, [0.0; 3], 0));
        let dyn_count = 1 + (rng.next() % 4) as usize;
        for b_idx in 0..dyn_count {
            let col = 1 + (rng.next() % (COUPLE_SHAPE_COUNT - 1));
            let pos = [
                rng.range(-0.18, 0.18) as f32,
                0.38 + (b_idx as f32) * 0.24,
                rng.range(-0.18, 0.18) as f32,
            ];
            let inv_m = rng.range(0.5, 3.0) as f32;
            let mut body = RigidBody::new(
                pos,
                inv_m,
                box_inv_inertia(1.0 / inv_m, [0.15, 0.15, 0.15]),
                col,
            );
            body.vel = [
                rng.range(-0.8, 0.8) as f32,
                rng.range(-1.5, 0.2) as f32,
                rng.range(-0.8, 0.8) as f32,
            ];
            body.ang_vel = [
                rng.range(-2.0, 2.0) as f32,
                rng.range(-2.0, 2.0) as f32,
                rng.range(-2.0, 2.0) as f32,
            ];
            if rng.next() % 2 == 0 {
                let (s, c) = (rng.range(-0.6, 0.6) as f32).sin_cos();
                body.orientation = [0.0, s, 0.0, c];
            }
            if rng.next() % 2 == 0 {
                let (s, c) = (rng.range(-0.5, 0.5) as f32).sin_cos();
                body.inertia_frame = [s, 0.0, 0.0, c];
            }
            bodies.push(body);
        }

        let rigid = RigidWorld::new(
            RigidConfig {
                dt,
                substeps: 1 + rng.next() % 4,
                gravity,
                ..RigidConfig::default()
            },
            &bodies,
            &colliders,
        )
        .unwrap();

        let cells = [2 + rng.next() % 2, 2, 2 + rng.next() % 2];
        let (mut particles, constraints) =
            SoftWorld::tetrahedral_box([-0.15, 0.31, -0.15], 0.12, cells, 1.5, 1e-5, 1e-6).unwrap();
        for p in &mut particles {
            p.vel = [
                rng.range(-0.3, 0.3) as f32,
                rng.range(-0.8, 0.1) as f32,
                rng.range(-0.3, 0.3) as f32,
            ];
            if rng.next() % 11 == 0 {
                p.inv_mass = 0.0;
            }
        }
        let soft = SoftWorld::new(
            SoftConfig {
                dt,
                substeps: 4 + rng.next() % 8,
                gravity,
                ground: Some(0.0),
                ground_friction: 0.4,
            },
            &particles,
            &constraints,
        )
        .unwrap();

        let friction = if rng.next() % 4 == 0 {
            0.0
        } else {
            rng.range(0.1, 0.8) as f32
        };
        CoupledWorld::new(
            rigid,
            soft,
            CouplingConfig {
                particle_radius: 0.025,
                friction,
            },
        )
        .unwrap()
    }};
}

/// The port's soft-rigid coupling substep (`CoupledWorld::step`,
/// vesper/src/couple.ves) against the frozen Rust build: the coupled SHA-256
/// digest and contact count after every tick across static and dynamic boxes,
/// spheres, capsules, convex hulls and segments (SH6 lane 9, #2243).
#[test]
fn couple_matches_the_rust_build() {
    let mut differences = 0usize;
    let mut cases = 0usize;
    let mut total_contacts = 0usize;
    let mut errors = 0usize;
    for round in 0..COUPLE_DIFF_ROUNDS {
        let seed = COUPLE_SEED_BASE + round;
        let mut pw = couple_scene!(vesper_kinetic, seed);
        let mut rw = couple_scene!(vesper_kinetic_rust, seed);
        for step in 0..COUPLE_DIFF_STEPS {
            match (pw.step(), rw.step()) {
                (Ok(ph), Ok(rh)) => {
                    cases += 1;
                    total_contacts += pw.last_contacts();
                    if ph != rh || pw.last_contacts() != rw.last_contacts() {
                        differences += 1;
                    }
                }
                (p, r) => {
                    assert_eq!(
                        format!("{p:?}"),
                        format!("{r:?}"),
                        "round {round} step {step}"
                    );
                    errors += 1;
                    break;
                }
            }
        }
    }
    println!(
        "couple differential differences={differences} cases={cases} contacts={total_contacts} errors={errors}"
    );
    assert!(cases >= COUPLE_MIN_CASES && total_contacts >= COUPLE_MIN_CONTACTS);
    assert_eq!(differences, 0);
}

#[test]
#[ignore = "ABBA benchmark; run in release with --ignored --nocapture"]
fn abba_couple() {
    use std::hint::black_box;
    use vesper_bench::{BenchCase, Invocation, RunConfig, run_abba};

    const ABBA_SEED: u64 = 0x636f_7570_abba;
    const ABBA_EPOCHS: usize = 30;
    const ABBA_WARMUP: usize = 2;
    const ABBA_OPS_PER_WINDOW: usize = 4;
    const ABBA_MAX_CALLS: usize = 100_000;
    const ABBA_MAX_NANOS: u64 = 600_000_000_000;
    const ABBA_LAYOUT_SEED: u64 = 1;
    const SIGN_TEST_ALPHA_HALF: f64 = 0.025;
    const FLIP_UPPER_BOUND_LIMIT: f64 = 1.50;

    let mut pw = couple_scene!(vesper_kinetic, ABBA_SEED);
    let mut rw = couple_scene!(vesper_kinetic_rust, ABBA_SEED);

    struct CoupleCase<F>(F);
    impl<F: FnMut() -> usize> BenchCase for CoupleCase<F> {
        type Input = ();
        type Output = usize;
        type Error = std::convert::Infallible;
        fn setup(&mut self, _: Invocation) -> Result<Self::Input, Self::Error> {
            Ok(())
        }
        fn run(&mut self, _: Self::Input) -> Result<Self::Output, Self::Error> {
            Ok(black_box((self.0)()))
        }
    }

    let cfg = RunConfig {
        epochs: ABBA_EPOCHS,
        warmup_epochs: ABBA_WARMUP,
        operations_per_window: ABBA_OPS_PER_WINDOW,
        max_total_calls: ABBA_MAX_CALLS,
        max_elapsed_nanos: ABBA_MAX_NANOS,
        layout_seed: ABBA_LAYOUT_SEED,
    };
    let mut r_case = CoupleCase(|| {
        rw.step().unwrap();
        rw.last_contacts()
    });
    let mut p_case = CoupleCase(|| {
        pw.step().unwrap();
        pw.last_contacts()
    });
    let report = run_abba(cfg, &mut r_case, &mut p_case).unwrap();
    let pairs = report.pairs();
    let mut ratios: Vec<f64> = pairs
        .iter()
        .map(|p| p.candidate_total() as f64 / p.baseline_total() as f64)
        .collect();
    ratios.sort_by(f64::total_cmp);
    let mid = ratios.len() / 2;
    let mut cdf = 0.0;
    let mut choose = 1.0;
    let mut k = 0usize;
    for j in 0..ratios.len() {
        cdf += choose * 0.5f64.powi(ratios.len() as i32);
        if cdf > SIGN_TEST_ALPHA_HALF {
            break;
        }
        k = j + 1;
        choose = choose * (ratios.len() - j) as f64 / (j + 1) as f64;
    }
    let i = k.saturating_sub(1).min(mid);
    let (ci_lo, ci_hi) = (ratios[i], ratios[ratios.len() - 1 - i]);
    println!(
        "ABBA couple: median {:.3}x [{ci_lo:.3}x, {ci_hi:.3}x]",
        ratios[mid]
    );
    assert!(
        ci_hi <= FLIP_UPPER_BOUND_LIMIT,
        "ABBA 95% upper bound {ci_hi:.3}x exceeds {FLIP_UPPER_BOUND_LIMIT:.2}x"
    );
}
